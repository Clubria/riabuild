//! Standing by for the channel, and serving it once it is this session's.
//!
//! One of these runs for the whole of every remote session — the one that owns
//! the channel *and* the ones that do not, which is the difference between this
//! and what it replaces. A session that lost the lease used to start nothing and
//! never ask again, so an owner whose laptop went away took every sibling's
//! paste with it and left riabuild running, idle, in a terminal that could have
//! had the channel back in seconds.
//!
//! The loop is three states and no more: ask for the lease, serve while it is
//! held, and give it back the moment serving ends.
//!
//! **A named wall ends this session's participation, and that is deliberate.**
//! `supervise` returns rather than retrying when it meets something retrying
//! cannot fix — a server with no `riabuild channel pump`, a host key riabuild
//! never recorded, a key the server refuses. Releasing the lease lets a sibling
//! try, which is right: it may be a newer riabuild, or the wall may have been
//! about this session's own connection. Standing by *again* afterwards is not:
//! two sessions each re-taking a lease they cannot use is a pair of laptops
//! reconnecting to a wall in turn, for ever, and every attempt is an
//! authentication on somebody's `sshd`. So the wall is said once, the lease goes
//! back, and this session is done. `riabuild remote` is what tries again, and by
//! then the developer has run the command the wall's own message named.

use super::lease;
use riabuild_channel::agent::Agent;
use riabuild_channel::supervisor::{Outcome, Stop, Tunnel, backoff, supervise};
use riabuild_runner::CommandRunner;
use riabuild_ui::{StatusBar, Ui};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How often a session that is standing by asks whether the channel has fallen
/// free.
///
/// This is the whole of how long paste stays dead after an owner leaves, so it
/// is short. It costs one `flock` on a file this laptop already has open pages
/// for — no process, no network, and nothing on the server, which is what makes
/// five seconds affordable where a poll that opened an SSH connection would not
/// be.
const STANDBY_POLL: Duration = Duration::from_secs(5);

/// How many times in a row this session may take the lease, find the server's
/// socket already served, and hand the lease straight back, before it says so.
///
/// The count exists because two very different things produce that bounce and
/// only one of them is worth a word.
///
/// **The ordinary one is not.** Two of this laptop's windows can hold two
/// *different* leases for one machine — the lease is keyed by the login target
/// as typed, so `build-01.fly.dev` in one terminal and `10.0.0.5` in another
/// are two leases over one socket. Both try to serve, one of them loses the
/// race, and paste works perfectly in both. Bouncing quietly is the whole
/// answer, and saying anything would be the false alarm this design was
/// rewritten to stop printing.
///
/// **The other one is.** A pump that outlived its laptop holds the socket,
/// answering connects it will never reply to, so paste really is dead — until
/// `KEEPALIVE_DEADLINE`, 45 seconds, after which that pump gives the socket
/// back on its own and the next ask here binds it. Seven bounces is about a
/// minute and a half of [`backoff`], comfortably past that: a bounce still
/// going by then is one the pump's own keepalive did not end, and the developer
/// is owed a sentence rather than another silent minute.
const QUIET_BOUNCES: u32 = 7;

/// Everything one session needs to serve the channel when its turn comes.
///
/// Owned rather than borrowed, every field of it: this runs as a background
/// task beside the developer's shell and outlives the call that spawned it by
/// hours.
pub(super) struct Holder {
    /// This laptop's lease directory for this server, from [`lease::dir`].
    pub(super) dir: PathBuf,
    /// The lease if this session already has it — the first ask happens before
    /// the banner, so that the banner can say which of the two it is.
    pub(super) lease: Option<lease::Lease>,
    pub(super) runner: Arc<dyn CommandRunner>,
    pub(super) tunnel: Tunnel,
    pub(super) agent: Arc<Agent>,
    /// Shared rather than owned, because the loop below now calls `supervise`
    /// more than once: a session that takes the lease and finds the socket
    /// already served comes back round and asks again. `Ui` is not `Clone` —
    /// it carries the pending-status-line counter every printer on this laptop
    /// has to agree about — so sharing it is the only way two calls can print
    /// through the same one.
    pub(super) ui: Arc<Ui>,
    pub(super) stop: Stop,
    pub(super) bar: Arc<StatusBar>,
}

/// Serves the channel for as long as this session is the one holding the lease,
/// and waits for it in between.
///
/// **Two different things can say "not you", and the loop has to hear both.**
/// The lease is this laptop's own answer and is cheap to ask. The server's
/// socket is the other one, and it is the only one that is *authoritative*: a
/// lease saves a needless `ssh`, it does not promise that no sibling is
/// serving, and it cannot — two windows can reach one machine under two
/// spellings of its address and hold a lease each. So a session that takes the
/// lease and then finds the socket already served is not in an error state and
/// never was. It gives the lease back and stands by, exactly as though it had
/// never won it.
pub(super) async fn hold(holder: Holder) {
    let Holder {
        dir,
        mut lease,
        runner,
        tunnel,
        agent,
        ui,
        stop,
        bar,
    } = holder;

    // Consecutive rounds that took the lease and found the socket already
    // served — see `QUIET_BOUNCES`. What it counts is a *run* of them, so
    // anything else ends this task and the count with it.
    let mut bounces = 0u32;

    loop {
        if stop.asked() {
            return;
        }

        let held = match lease.take() {
            Some(held) => held,
            None => match lease::try_take(&dir).await {
                Ok(Some(held)) => held,
                Ok(None) => {
                    // A sibling is serving it. Paste works in this terminal too
                    // — it is the same socket on the same server — so there is
                    // nothing to say and nothing to do but be here when that
                    // session ends.
                    tokio::select! {
                        () = tokio::time::sleep(STANDBY_POLL) => continue,
                        () = stop.stopped() => return,
                    }
                }
                Err(error) => {
                    // riabuild cannot read its own lease directory, which asking
                    // again in five seconds will not change. Said once, on the
                    // bar where there is one, and then this session stops
                    // standing by rather than repeating it for the length of a
                    // shell.
                    if bar.enabled() {
                        bar.show(
                            "▲ Clipboard channel — riabuild cannot tell which session is \
                             serving it · paste may be off",
                        );
                    } else {
                        ui.warn(&format!("Clipboard channel — {error}"));
                    }
                    return;
                }
            },
        };

        // Serving. This returns when the session ends, at a wall `supervise` has
        // already reported, or the moment the server answers that a sibling's
        // pump holds the socket; an ordinary disconnect is its own to rebuild
        // and never comes back here.
        let outcome = supervise(
            Arc::clone(&runner),
            tunnel.clone(),
            Arc::clone(&agent),
            Arc::clone(&ui),
            stop.clone(),
            Arc::clone(&bar),
        )
        .await;
        // Before anything else, and before this task ends rather than as it
        // ends, so a sibling standing by finds the lease free on its next ask
        // rather than on this process's exit — which for a wall may be hours
        // away, and for the bounce below is the whole of the point.
        drop(held);

        if !matches!(outcome, Outcome::AlreadyServed) {
            return;
        }

        // A sibling terminal's pump has the socket. Nothing is wrong — paste in
        // this session's shell goes through that very pump — so this is a
        // standby, not a message.
        //
        // On [`backoff`] rather than [`STANDBY_POLL`], and the difference
        // between the two is the difference between a poll and a hammer: the
        // free-lease ask above costs one `flock` on a local file, while every
        // round of this one costs an `ssh` and an authentication against
        // somebody's `sshd`. Two windows holding two leases over one socket
        // would otherwise open one every five seconds for as long as both
        // shells are open.
        bounces = bounces.saturating_add(1);
        if bounces == QUIET_BOUNCES {
            // Once, and never "paste is off". From here riabuild cannot tell a
            // working sibling from a pump that outlived its laptop; only
            // `channel status`, which asks the socket itself, can.
            if bar.enabled() {
                bar.show(
                    "▲ Clipboard channel — served by another session on this \
                     server · run `riabuild channel status` if paste is not working",
                );
            } else {
                ui.warn(
                    "Clipboard channel — another session on this server is serving it. \
                     Run `riabuild channel status` if paste is not working.",
                );
            }
        }

        tokio::select! {
            () = tokio::time::sleep(backoff(bounces)) => continue,
            () = stop.stopped() => return,
        }
    }
}

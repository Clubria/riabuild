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
use riabuild_channel::supervisor::{Stop, Tunnel, supervise};
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
    pub(super) ui: Ui,
    pub(super) stop: Stop,
    pub(super) bar: Arc<StatusBar>,
}

/// Serves the channel for as long as this session is the one holding the lease,
/// and waits for it in between.
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

        // Serving. This returns when the session ends, or at a wall `supervise`
        // has already reported; an ordinary disconnect is its own to rebuild and
        // never comes back here.
        let _ = supervise(runner, tunnel, agent, ui, stop, bar).await;
        // Before this task ends rather than as it ends, so the sibling standing
        // by finds the lease free on its next ask rather than on this process's
        // exit — which for a wall may be hours away.
        drop(held);
        return;
    }
}

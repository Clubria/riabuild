//! The clipboard channel, for the length of one remote session.
//!
//! `riabuild_channel` builds the parts — the laptop's agent, the rules about
//! which socket is ours, the supervisor that keeps the exec session up. This is
//! where remote mode puts them together: the paths that are remote mode's to
//! choose, the refcount that stops two terminals into one box from fighting
//! over the channel, and the rule that outranks everything else here — a
//! channel that will not start costs a warning, never the shell.
//!
//! Inside `remote`, `channel::` is this module and the parts it drives are
//! spelled `riabuild_channel::` throughout. The two are one word apart and only
//! one of them is optional, so the longer spelling is worth the noise.

mod claim;
mod sockets;

pub use sockets::remote_socket;

use super::{Remote, askpass, identity, shell};
use anyhow::Result;
use riabuild_channel::supervisor::{StatusLine, Stop, Tunnel, supervise};
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::{Failure, Ui};
use sockets::fits;
use std::path::Path;
use std::sync::Arc;

/// The one line remote mode's banner gains. Rendered through `Ui` exactly as
/// the tasks above it are, so it reads as another finished item rather than as
/// a message.
///
/// `Ui::satisfied` rather than `Ui::applied`, in both of the branches below.
/// The two differ by one role — `applied` leaves the title unpainted so a task
/// riabuild just *ran* stands out from one that was already fine — and this
/// line joins a list where every entry is a check that passed. Painted as work
/// done, it is the brightest thing on the screen at the end of a run, and the
/// brightest thing is a facility nobody asked about rather than any of the
/// setup it is sitting under. What is honest about it is unchanged either way:
/// a tunnel that then fails to come up still reports itself in the
/// supervisor's own voice.
const BANNER: &str = "Clipboard channel — connected";

/// What a second terminal into the same server says instead.
///
/// It starts nothing — the first session owns the connection — so it cannot
/// claim one. The wording carries the limit with it, because this is the shape
/// the failure actually takes: the supervisor is a task inside the *owner's*
/// process, so whichever terminal exits first takes the channel with it, and
/// the survivor is left naming a socket path that is correct and unbound. Its
/// paste, image paste and `xdg-open` stop; its copying appears to carry on,
/// because Claude Code's OSC 52 escape needs no channel. `riabuild channel
/// status` is the thing that says so, which is why it is named here rather than
/// left to be found.
const SHARED_BANNER: &str = "Clipboard channel — shared with this laptop's other session on this server \
     (it ends when that one does; `riabuild channel status` reports it)";

/// Everything remote mode has to tell the channel about this session.
///
/// A struct rather than eight arguments, and the three strings are built by the
/// caller on purpose: the namespace, the server's binary path and `env_command`
/// are remote mode's, and a channel that reached for them would stop being
/// something a test can drive without a server.
pub struct Plan<'a> {
    pub remote: &'a Remote,
    pub paths: &'a dyn Paths,
    pub runner: Arc<dyn CommandRunner>,
    pub ui: &'a Ui,
    /// `--quiet`, for the supervisor's own printer. It outlives this call —
    /// it runs beside the developer's shell — so it cannot borrow `ui`, and
    /// `Ui` is not `Clone`.
    pub quiet: bool,
    /// Where the forward lands on the server, from [`remote_socket`].
    pub remote_socket: String,
    /// The server's own `riabuild channel pump`, env-prefixed, so the pump
    /// binds the socket where this session's shims look for it.
    ///
    /// This *is* the transport. `ssh -T <host> <this>` is the whole of what the
    /// channel asks a server for, which is why it works on servers that refuse
    /// socket forwarding or have never implemented it.
    pub pump: String,
    /// The env-prefixed `riabuild shell` this session is really here for.
    pub shell: String,
    /// An issued identity riabuild is carrying because its own key cannot sign
    /// in to this server — see `identity::ssh_options`. `None` on every
    /// ordinary server.
    pub carry: Option<&'a crate::issued::Working>,
}

/// Opens the developer's shell, with a clipboard channel beside it if one can
/// be had.
///
/// The channel is started before the shell and stopped after it, and every
/// failure along the way is a warning — a laptop with no clipboard tool, a
/// socket riabuild refuses to take over, a server that forbids socket
/// forwarding. None of them may cost the developer the shell they asked for;
/// see `riabuild_channel`'s module doc.
pub async fn open_shell(plan: Plan<'_>) -> Result<i32> {
    let channel = Channel::start(&plan).await;
    let code = shell::open(
        plan.remote,
        plan.paths,
        plan.runner.clone(),
        plan.ui,
        &plan.shell,
        plan.carry,
    )
    .await;
    // Bound rather than `?`d: the shell returning an error is still the end of
    // the session, and a tunnel left up behind it would hold the server's
    // socket against the next one.
    channel.stop().await;
    code
}

/// What this session is holding open, and has to give back.
struct Channel {
    claim: Option<claim::Claim>,
    started: Option<Started>,
}

/// The two background tasks the first session into a server owns.
struct Started {
    stop: Stop,
    tunnel: tokio::task::JoinHandle<Option<Failure>>,
    /// The one line the channel is allowed to say anything on while the
    /// developer's shell owns the screen.
    ///
    /// Started here rather than inside the supervisor because the reason it
    /// exists is remote mode's own: the next thing this function's caller does
    /// is hand the terminal to mosh, and a background task printing into a
    /// screen mosh is drawing — in the raw mode any interactive shell puts it
    /// in, where a newline drops a row and does not return to column one —
    /// produces the staircase of half-lines developers reported. Held here for
    /// the same reason: the session's end is when the line comes off, and that
    /// is [`Channel::stop`], not anything the supervisor can see.
    line: StatusLine,
}

impl Channel {
    /// Never fails, and never returns an `Err` for a caller to `?` — that is
    /// the whole contract. A channel that could not start is a `Channel` with
    /// nothing in it.
    async fn start(plan: &Plan<'_>) -> Channel {
        let dir = claim::dir(plan.paths, plan.remote);
        let claim = match claim::Claim::open(&dir, std::process::id(), plan.runner.clone()).await {
            Ok(claim) => claim,
            Err(error) => {
                warn(plan.ui, &error);
                return Channel {
                    claim: None,
                    started: None,
                };
            }
        };

        if !claim.owner {
            // A sibling terminal into this same server already has one. Set the
            // environment, say so, and start nothing: a second pump would find
            // the first one's socket live and refuse it.
            //
            // The honest limit, since it is not the one the markers imply: the
            // supervisor is a task inside the *owner's* process, so a first
            // terminal that exits first takes the channel with it and this
            // session's paste stops. That is the channel's documented failure
            // rather than a new one, and the alternative is a daemon outliving
            // the shell — which remote mode does not have and should not grow
            // for paste.
            //
            // Said in its own words rather than with `BANNER`, which asserts a
            // connection this branch has neither made nor checked. All it knows
            // is that a sibling *process* is alive — not that the channel it
            // owns ever came up, and not that it still has one. Claiming
            // "connected" there is how a developer ends up reporting paste as
            // broken while riabuild's own banner told them it was fine.
            plan.ui.satisfied(SHARED_BANNER);
            return Channel {
                claim: Some(claim),
                started: None,
            };
        }

        match try_start(plan).await {
            Ok(started) => {
                // Honest as far as it goes: the agent is serving and the
                // supervisor holds the forward. A tunnel that then fails to
                // come up reports itself, in the supervisor's own voice, rather
                // than being promised away here.
                plan.ui.satisfied(BANNER);
                Channel {
                    claim: Some(claim),
                    started: Some(started),
                }
            }
            Err(error) => {
                warn(plan.ui, &error);
                // Released rather than held: a marker with no channel behind it
                // would tell the next terminal a channel exists, and it would
                // start nothing either.
                claim.close().await;
                Channel {
                    claim: None,
                    started: None,
                }
            }
        }
    }

    async fn stop(self) {
        if let Some(started) = self.started {
            // Before the supervisor is even asked to stop: the shell has
            // already exited, so riabuild is about to print its own output to
            // a terminal that must not have a stale warning pinned to row two.
            started.line.stop();
            started.stop.stop();
            // Awaited, not detached: the supervisor kills its `ssh` on the way
            // out, which ends the pump, which frees the server's socket before
            // the next session tries to bind it. There is no laptop socket to
            // clean up any more — the agent is served on the child's stdio.
            let _ = started.tunnel.await;
        }
        if let Some(claim) = self.claim {
            claim.close().await;
        }
    }
}

/// Everything that can go wrong, in one place, so the caller above can turn all
/// of it into a warning.
async fn try_start(plan: &Plan<'_>) -> Result<Started> {
    // Still checked, and still the server's path: the pump binds it there, and
    // a `sockaddr_un` that cannot hold it fails inside `bind()` with
    // `ENAMETOOLONG` at best and a silent truncation at worst. What no longer
    // needs checking is a laptop socket, because there no longer is one.
    fits(Path::new(&plan.remote_socket), plan.remote)?;

    let agent = riabuild_channel::laptop_agent(plan.runner.clone(), &plan.paths.bin_dir())?;

    let stop = Stop::new();
    // After everything that can fail, and before the one thing that outlives
    // this call: an early return past this point would leave a task repainting
    // a line for a channel that never started.
    let line = StatusLine::start(plan.quiet);
    let tunnel = tokio::spawn(supervise(
        plan.runner.clone(),
        Tunnel {
            host: plan.remote.host.clone(),
            user: plan.remote.user.clone(),
            // The same options every other ssh in this flow is built from —
            // riabuild's own `known_hosts`, its own key, `-F /dev/null`, and
            // the issued identity when one is being carried. The supervisor
            // used to compose a `-p` and an `-i` of its own, which is how the
            // one connection nobody watches came to reach servers by different
            // rules than the connection right beside it. `carry` in particular
            // was never passed at all, so the servers riabuild's own key cannot
            // sign in to — the whole reason issued keys exist — could not carry
            // a channel however well the rest of the session worked.
            options: identity::ssh_options(plan.remote, plan.paths, true, plan.carry),
            command: plan.pump.clone(),
            env: askpass::ssh_env(plan.remote, plan.paths),
        },
        agent,
        Ui::new(plan.quiet),
        stop.clone(),
        line.bar(),
    ));

    Ok(Started { stop, tunnel, line })
}

/// The channel's own failure voice.
///
/// Never `Ui::failure`, which prints "riabuild stopped:" — nothing stopped. The
/// setup run, the secrets and the shell are all untouched, and sending a
/// developer to look for a broken environment they do not have is worse than
/// saying nothing. The same shape `supervisor::run::report` uses once the
/// tunnel is up; it is private there, and four lines are cheaper than widening
/// it.
fn warn(ui: &Ui, error: &anyhow::Error) {
    ui.warn(&format!("Clipboard channel — {error}"));
    if let Some(failure) = error.downcast_ref::<Failure>() {
        for line in failure
            .detail
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(4)
        {
            ui.note(line);
        }
    }
    ui.info("Paste will not work in this session. Nothing else is affected.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;
    use sockets::SUN_PATH_MAX;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// The shell, and nothing but the shell: no mosh anywhere, so `open` takes
    /// its ssh fallback and one scripted stub covers the run.
    fn shell_runner() -> Arc<FakeRunner> {
        Arc::new(FakeRunner::new().with("ssh", 0, "", ""))
    }

    fn plan<'a>(
        remote: &'a Remote,
        paths: &'a dyn Paths,
        runner: Arc<dyn CommandRunner>,
        ui: &'a Ui,
        remote_socket: String,
    ) -> Plan<'a> {
        Plan {
            carry: None,
            remote,
            paths,
            runner,
            ui,
            quiet: true,
            remote_socket,
            pump: "env 'RIABUILD_CHANNEL_SOCKET=/x' riabuild channel pump".into(),
            shell: "env 'RIABUILD_CHANNEL_SOCKET=/x' riabuild shell".into(),
        }
    }

    /// The regression a later refactor will introduce: a `?` on the channel's
    /// way up, and a laptop that cannot open one loses its shell as well as its
    /// paste. The channel is strictly optional — its absence degrades to "no
    /// clipboard", never to "environment broken".
    #[tokio::test]
    async fn a_channel_that_cannot_start_still_opens_a_shell() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = shell_runner();
        let ui = Ui::new(true);
        // A namespace nothing could ever bind: the failure is real, it happens
        // before anything is spawned, and it is the same one a genuinely long
        // home directory would produce.
        let impossible = format!("/home/{}/channel.sock", "d".repeat(SUN_PATH_MAX));

        let code = open_shell(plan(&remote(), &paths, fake.clone(), &ui, impossible))
            .await
            .expect("a channel that cannot start is not a reason to withhold a shell");

        assert_eq!(code, 0);
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh") && call.contains("riabuild shell")),
            "the developer must still get their shell: {:?}",
            fake.calls()
        );
        assert!(
            fake.spawns().is_empty(),
            "nothing may be left running behind a channel that never started: {:?}",
            fake.spawns()
        );
    }

    /// Two terminals into one server share one channel. The second must start
    /// no connection of its own: its pump would find the first one's socket
    /// live and refuse it, and the second terminal would report a failure for
    /// a channel that is working perfectly.
    #[tokio::test]
    async fn a_second_session_to_one_server_joins_the_channel_rather_than_rebuilding_it() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(
            FakeRunner::new()
                .with("ssh", 0, "", "")
                // The sibling terminal's process, still running.
                .with("kill -0", 0, "", ""),
        );
        let dir = claim::dir(&paths, &remote());
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        tokio::fs::write(dir.join("4242"), "0")
            .await
            .expect("write");
        let ui = Ui::new(true);

        open_shell(plan(
            &remote(),
            &paths,
            fake.clone(),
            &ui,
            "/home/dev/.riabuild-remote/abc/channel.sock".into(),
        ))
        .await
        .expect("opens");

        assert!(
            fake.spawns().is_empty(),
            "the second session must start no agent and no tunnel: {:?}",
            fake.spawns()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("riabuild shell")),
            "{:?}",
            fake.calls()
        );
        assert!(
            !dir.join(std::process::id().to_string()).exists(),
            "a session that has returned must not leave its marker behind"
        );
    }
}

//! The clipboard channel, for the length of one remote session.
//!
//! `riabuild_channel` builds the parts — the laptop's agent, the rules about
//! which socket is ours, the supervisor that keeps the exec session up. This is
//! where remote mode puts them together: the paths that are remote mode's to
//! choose, the lease that decides which of two terminals into one box serves
//! the channel — and hands it to the other when that one ends — and the rule
//! that outranks everything else here: a channel that will not start costs a
//! warning, never the shell.
//!
//! Inside `remote`, `channel::` is this module and the parts it drives are
//! spelled `riabuild_channel::` throughout. The two are one word apart and only
//! one of them is optional, so the longer spelling is worth the noise.

mod hold;
mod lease;
mod sockets;

pub use sockets::remote_socket;

use super::{Remote, askpass, shell};
use anyhow::Result;
use hold::{Holder, hold};
use riabuild_channel::supervisor::{StatusLine, Stop, Tunnel};
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
/// It starts no connection of its own — a second pump would find the first
/// one's socket live and be refused — but it is not a bystander either: it
/// holds a place in the queue for the channel's lease and takes the channel
/// over if the session serving it ends. That is the half worth putting in the
/// banner, because it is the half a developer would otherwise have to discover
/// by watching paste come back.
///
/// This used to carry a limit instead of a promise: *it ends when that one
/// does*. It really did, and the sentence was honest, but a documented failure
/// is still a failure — the survivor sat there naming a socket path that was
/// correct and unbound, with paste, image paste and `xdg-open` dead, while
/// riabuild ran in that very terminal. See `hold`.
const STANDBY_BANNER: &str = "Clipboard channel — served by this laptop's other session on this server, and taken over \
     by this one if that session ends";

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
    /// The server's own riabuild with the same environment prefix and no
    /// arguments — `env 'K=V'… '/abs/path/riabuild'`.
    ///
    /// What the mosh probe and the server end of a TCP-tunnelled mosh session
    /// are appended to. It is here rather than derived from [`Plan::shell`]
    /// because the prefix is the caller's to build, and a second spelling of it
    /// is a second thing to get wrong.
    pub binary: String,
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
        &plan.binary,
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
    started: Option<Started>,
}

/// The two background tasks every session into a server owns.
///
/// *Every* session, including one that found a sibling already serving. The
/// holder is what makes that session more than a bystander: it is standing by
/// for the channel's lease, and there is no longer such a thing as a remote
/// session with nothing running behind it but a banner.
struct Started {
    stop: Stop,
    holder: tokio::task::JoinHandle<()>,
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
        match try_start(plan).await {
            Ok((started, serving)) => {
                // Which of the two sentences is the *first* ask for the lease,
                // and nothing else. Both are honest as far as they go: the
                // agent is serving and this session either holds the channel or
                // is standing by for it. A connection that then fails to come up
                // reports itself, in the supervisor's own voice, rather than
                // being promised away here.
                plan.ui
                    .satisfied(if serving { BANNER } else { STANDBY_BANNER });
                Channel {
                    started: Some(started),
                }
            }
            Err(error) => {
                warn(plan.ui, &error);
                Channel { started: None }
            }
        }
    }

    async fn stop(self) {
        if let Some(started) = self.started {
            // Before the holder is even asked to stop: the shell has already
            // exited, so riabuild is about to print its own output to a
            // terminal that must not have a stale warning pinned to row two.
            started.line.stop();
            started.stop.stop();
            // Awaited, not detached, for two things that both have to have
            // happened before this function returns. The supervisor kills its
            // `ssh` on the way out, which ends the pump, which frees the
            // server's socket before the next session tries to bind it. And the
            // holder drops the lease, which is what lets a sibling terminal
            // take the channel over rather than waiting on this process to
            // exit.
            let _ = started.holder.await;
        }
    }
}

/// Everything that can go wrong, in one place, so the caller above can turn all
/// of it into a warning.
///
/// Answers whether this session is *serving* the channel as well as what it
/// started, because that is the one thing the banner cannot work out for itself
/// and the one thing that stops being true five seconds later.
async fn try_start(plan: &Plan<'_>) -> Result<(Started, bool)> {
    // Still checked, and still the server's path: the pump binds it there, and
    // a `sockaddr_un` that cannot hold it fails inside `bind()` with
    // `ENAMETOOLONG` at best and a silent truncation at worst. What no longer
    // needs checking is a laptop socket, because there no longer is one.
    fits(Path::new(&plan.remote_socket), plan.remote)?;

    let agent = riabuild_channel::laptop_agent(plan.runner.clone(), &plan.paths.bin_dir())?;

    // Asked once here, and again every few seconds in `hold` for as long as the
    // answer is no. The first ask is what the banner reports; every ask after it
    // is what makes the channel come back on its own when the session serving it
    // goes away.
    let dir = lease::dir(plan.paths, plan.remote);
    let lease = lease::try_take(&dir).await?;
    let serving = lease.is_some();

    let stop = Stop::new();
    // After everything that can fail, and before the one thing that outlives
    // this call: an early return past this point would leave a task repainting
    // a line for a channel that never started.
    let line = StatusLine::start(plan.quiet);
    let holder = tokio::spawn(hold(Holder {
        dir,
        lease,
        runner: plan.runner.clone(),
        tunnel: Tunnel {
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
            options: crate::ssh::Ssh::to(plan.remote, plan.paths, plan.runner.clone())
                .carry(plan.carry)
                .options_only(),
            command: plan.pump.clone(),
            env: askpass::ssh_env(plan.remote, plan.paths),
        },
        agent,
        ui: Arc::new(Ui::new(plan.quiet)),
        stop: stop.clone(),
        bar: line.bar(),
    }));

    Ok((Started { stop, holder, line }, serving))
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
    ///
    /// `xclip` is here so the laptop has a clipboard tool for `laptop_agent` to
    /// find. Without it these tests would pass on a Mac — where `detect`
    /// answers `macos` before it looks for anything — and on Linux would take
    /// the "this laptop has no clipboard tool" branch, starting no channel and
    /// asserting nothing they were written to assert.
    fn shell_runner() -> Arc<FakeRunner> {
        Arc::new(
            FakeRunner::new()
                .with("ssh", 0, "", "")
                .with("xclip", 0, "", ""),
        )
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
            binary: "env 'RIABUILD_CHANNEL_SOCKET=/x' riabuild".into(),
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
        let fake = shell_runner();
        // The sibling terminal, serving the channel.
        let held = lease::try_take(&lease::dir(&paths, &remote()))
            .await
            .expect("take")
            .expect("owns");
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
            "the second session must start no connection of its own: {:?}",
            fake.spawns()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("riabuild shell")),
            "{:?}",
            fake.calls()
        );
        drop(held);
    }

    /// Virtual time from now until the fake has started `count` children.
    ///
    /// Polling rather than a channel, because under a paused clock the polling
    /// *is* the mechanism: time only moves when the runtime has nothing left to
    /// run, so this sleep is what lets the holder's standby interval elapse.
    async fn until_spawns(fake: &FakeRunner, count: usize) {
        for _ in 0..2_000 {
            if fake.spawns().len() >= count {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("only {} of {count} spawns happened", fake.spawns().len());
    }

    /// The bug this change exists to fix, end to end.
    ///
    /// A developer with two terminals into one server closes the laptop for the
    /// night. The session that was serving the channel ends; the other one is
    /// still there in the morning. It used to be a bystander — ownership was
    /// decided once, at startup, so it started nothing and never asked again,
    /// and paste, image paste and `xdg-open` stayed dead in a terminal with
    /// riabuild running in it. Copying went on working, because Claude Code's
    /// OSC 52 escape needs no channel, which is what made it read as two
    /// unrelated bugs rather than one dead channel.
    ///
    /// Now the survivor takes the channel over and paste comes back with
    /// nothing typed anywhere.
    #[tokio::test(start_paused = true)]
    async fn a_session_standing_by_takes_the_channel_over_when_the_one_serving_it_ends() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = shell_runner();
        let ui = Ui::new(true);
        let remote = remote();

        let serving = lease::try_take(&lease::dir(&paths, &remote))
            .await
            .expect("take")
            .expect("the other terminal is serving the channel");

        let channel = Channel::start(&plan(
            &remote,
            &paths,
            fake.clone(),
            &ui,
            "/home/dev/.riabuild-remote/abc/channel.sock".into(),
        ))
        .await;
        assert!(
            fake.spawns().is_empty(),
            "a session standing by must start no second connection: {:?}",
            fake.spawns()
        );

        // The laptop's other session ends.
        drop(serving);

        until_spawns(&fake, 1).await;
        let spawned = fake.spawns().join(" ");
        assert!(
            spawned.contains("channel pump"),
            "the survivor has to open the channel itself: {spawned}"
        );

        channel.stop().await;
    }
}

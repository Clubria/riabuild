//! The loop that drives the decisions next door.
//!
//! One connection at a time: spawn `ssh -T <host> riabuild channel pump`, serve
//! the laptop's agent on the child's stdio for as long as it lives, and decide
//! from how it ended whether to rebuild or to stop. The child is *held* rather
//! than waited for, which is the whole reason `CommandRunner::spawn_piped`
//! exists — the channel runs for the length of a session, and a connection run
//! through `run` would only return once it had already died.
//!
//! Nothing here propagates an error to its caller. The channel is strictly
//! optional (see `channel`'s module doc): a supervisor that cannot start, or
//! that gives up, must degrade to "no clipboard" and never to "environment
//! broken". So `supervise` returns the failure that stopped it rather than an
//! `Err` — the developer's shell is not this task's to take down, and a `?`
//! reaching a caller that used `?` in turn is exactly how it would.

use super::{Tunnel, backoff, diagnose, ssh_args};

/// How many consecutive failures, with the channel never once having come up,
/// before the supervisor says out loud that it cannot reach the server.
///
/// Four puts it around half a minute into the backoff schedule — long enough
/// that an ordinary reconnect after a closed lid stays silent, short enough
/// that a channel which is never coming up says so while the developer is still
/// wondering why paste does nothing.
const QUIET_FAILURES: u32 = 4;

/// Whether this failure is the one to say out loud.
///
/// A predicate rather than three conditions inline, because it is the whole of
/// the decision and the loop around it cannot be unit-tested without an `ssh`:
/// `supervise` takes an owned `Ui`, so a test cannot hold on to the printer it
/// moved in and read back what was said. Extracted, every branch is reachable.
///
/// `ever_connected`, not "ever carried a request", and the difference is the
/// bug this sentence exists to keep fixed. Those were one flag, and on a link
/// that drops and rebuilds — which is the whole reason the developer is on mosh
/// — a channel nobody happened to paste through carried nothing on any attempt.
/// Four rebuilds later riabuild told them it could not reach a server it had
/// reached every single time. What proves a connection came up is the pump's
/// keepalive, which is why the pump has one.
fn should_say_it_cannot_connect(ever_connected: bool, said_so: bool, attempt: u32) -> bool {
    // A channel that has worked and then dropped is a laptop that slept, and
    // there is nothing for anyone to do about it.
    !ever_connected
        // Once per supervisor. At the backoff ceiling, "every time" is a line
        // every thirty seconds printed over whatever the developer is doing.
        && !said_so
        // Late enough that an ordinary slow reconnect stays quiet.
        && attempt >= QUIET_FAILURES
}
use crate::agent::{Agent, Served};
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::{Failure, StatusBar, Ui};
use std::sync::Arc;
use tokio::sync::watch;

/// The caller's end of a running supervisor.
///
/// Cloneable and inert: holding one keeps nothing alive, so a caller that drops
/// it without stopping the supervisor gets a channel that shuts itself down
/// rather than one that outlives the shell it belongs to.
#[derive(Clone)]
pub struct Stop(Arc<watch::Sender<bool>>);

impl Default for Stop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop {
    pub fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    /// Asks the supervisor to close the connection and return.
    ///
    /// Idempotent, and safe both before the supervisor has started and after it
    /// has already returned. `send_replace` rather than `send` for the first of
    /// those: `send` fails when nobody is subscribed *and leaves the value
    /// unchanged*, so a stop that arrived first would be silently forgotten and
    /// the supervisor would come up already-stale.
    pub fn stop(&self) {
        self.0.send_replace(true);
    }

    fn signal(&self) -> watch::Receiver<bool> {
        self.0.subscribe()
    }
}

/// Resolves once a stop has been asked for — immediately if it already has.
///
/// `changed()` on its own reports only transitions this receiver has not seen,
/// so a stop that landed before the supervisor reached this point would never
/// wake it: the shell would exit and the connection would stay up behind it.
async fn stopped(signal: &mut watch::Receiver<bool>) {
    loop {
        let asked = *signal.borrow_and_update();
        if asked {
            return;
        }
        if signal.changed().await.is_err() {
            // Every `Stop` handle is gone, so nothing can ever ask again. An
            // ssh nobody holds a stop for is the leak `kill_on_drop` exists to
            // prevent, and shutting down is the honest reading of "the caller
            // is finished with us".
            return;
        }
    }
}

/// Keeps the channel up until asked to stop.
///
/// Returns the failure that ended it, or `None` when it ended because it was
/// told to. A returned failure has already been shown to the developer; it
/// comes back as well so the caller can put it in a banner, and so this loop's
/// hard-stop path is something a test can assert on rather than something it
/// has to scrape off stderr.
///
/// Takes an owned `Ui` because it outlives the call that started it: this runs
/// as a background task beside the developer's shell, so borrowing the caller's
/// printer would tie the channel's lifetime to a stack frame that returned long
/// ago. `agent` is shared rather than owned for the mirror reason — one agent
/// answers every connection this loop builds, and rebuilding it per attempt
/// would re-detect the laptop's clipboard tooling on every network blip.
/// `bar` is the line the channel speaks on while a full-screen shell owns the
/// terminal, from `remote::channel`. A disabled one — which is what every run
/// without a remote session and every test has — sends each message back to
/// `Ui`, printed the ordinary way.
pub async fn supervise(
    runner: Arc<dyn CommandRunner>,
    tunnel: Tunnel,
    agent: Arc<Agent>,
    ui: Ui,
    stop: Stop,
    bar: Arc<StatusBar>,
) -> Option<Failure> {
    let mut signal = stop.signal();
    // Consecutive failures, and therefore the position in the backoff schedule.
    // Reset by a connection that reached the pump at all, so a laptop that
    // suspends every afternoon reconnects in a second rather than inheriting
    // the ceiling from a bad week — and so does one on a link that keeps
    // dropping, which is the case that most needs paste back quickly and the
    // one a rule counting only requests left at the thirty-second ceiling.
    let mut attempt = 0u32;
    // Whether this channel has ever come up. A connection that dropped is a
    // laptop that slept; one that has never reached the pump at all is a
    // channel that cannot come up, and only the second is worth a message.
    let mut ever_connected = false;
    // The unrecognised-wall message is said once per supervisor, never once per
    // attempt: at the backoff ceiling that would be a line every thirty seconds
    // for the length of a session, printed over whatever the developer is doing.
    let mut said_so = false;

    loop {
        let asked = *signal.borrow_and_update();
        if asked {
            return None;
        }

        let args = ssh_args(&tunnel);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();

        let options = RunOptions {
            env: tunnel.env.clone(),
            ..Default::default()
        };
        let child = match runner.spawn_piped("ssh", &argv, &options).await {
            Ok(child) => child,
            Err(error) => {
                // An ssh that will not start at all does not start on the next
                // attempt either, so this is the same wall `diagnose` keeps the
                // loop off: retrying it is an infinite loop, not resilience.
                return Some(report(
                    &ui,
                    &bar,
                    Failure::new(
                        "riabuild could not start the clipboard channel",
                        "Check that `ssh` is installed and runnable, then open a new riabuild shell. Everything except paste works without it.",
                    )
                    .command(format!("ssh {}", args.join(" ")))
                    .detail(error.to_string()),
                ));
            }
        };

        let (Some(to_server), Some(from_server)) = (child.take_stdin(), child.take_stdout()) else {
            // Unreachable through `RealRunner`, which pipes both halves for
            // every `spawn_piped`. Reported rather than ignored because the
            // alternative is a supervisor that rebuilds a channel carrying
            // nothing, forever, with nothing on screen saying why.
            let _ = child.kill().await;
            return Some(report(
                &ui,
                &bar,
                Failure::new(
                    "riabuild could not open the clipboard channel's pipe",
                    "Open a new riabuild shell. Everything except paste works without it.",
                ),
            ));
        };

        // The agent owns both halves for the length of this connection, and
        // dropping them on its way out is load-bearing: closing the child's
        // stdin gives the pump an end of input, which ends the pump, which ends
        // this `ssh`. That chain is what makes an agent that stops serving —
        // on a frame it cannot read, say — turn into a clean rebuild rather
        // than a live connection nobody is listening to.
        let serving = tokio::spawn(Arc::clone(&agent).serve_pipe(from_server, to_server));

        let ended = tokio::select! {
            exited = child.wait() => match exited {
                Ok(output) => Ended::Exited(output.stderr),
                // Losing track of the child is not a configuration fault, so it
                // takes the ordinary retry path rather than being fed to
                // `diagnose` as if ssh had said it.
                Err(error) => Ended::Exited(error.to_string()),
            },
            () = stopped(&mut signal) => Ended::Stopped,
        };

        // Killed explicitly rather than left to `kill_on_drop`: the developer's
        // shell has exited, and the pump on the far side should go with it
        // rather than wait out its own read.
        let _ = child.kill().await;
        // The pipes close with the child, so this resolves rather than hanging.
        let served = match serving.await {
            Ok(Ok(served)) => served,
            _ => Served::default(),
        };

        match ended {
            Ended::Stopped => return None,
            Ended::Exited(stderr) => {
                if let Some(failure) = diagnose(&stderr) {
                    return Some(report(&ui, &bar, failure));
                }
                // A failure `diagnose` does not recognise, repeated, with the
                // channel never once having come up. That is a wall too — simply
                // one nobody has written a sentence for yet — and the loop used
                // to retry it in silence for the length of the session while
                // the banner overhead said "connected".
                //
                // Said once rather than every time, and the loop carries on
                // rather than stopping: unlike the named walls, this one cannot
                // be told apart from a server that is slow to come back, and
                // giving up on a laptop that will reconnect in a minute is the
                // worse mistake. What it buys is that the next cause of a dead
                // channel costs one message instead of three rounds of
                // guesswork.
                if should_say_it_cannot_connect(ever_connected, said_so, attempt) {
                    said_so = true;
                    report(
                        &ui,
                        &bar,
                        cannot_connect(&stderr).command(format!("ssh {}", args.join(" "))),
                    );
                }
            }
        }

        if served.connected() {
            attempt = 0;
            ever_connected = true;
            // The connection came up, so whatever the bar was saying about one
            // that would not is over. Cleared here rather than left for the
            // developer to disbelieve: a warning that outlives its cause is how
            // the next true one stops being read.
            bar.clear();
        }

        let delay = backoff(attempt);
        attempt = attempt.saturating_add(1);

        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = stopped(&mut signal) => return None,
        }
    }
}

/// How one connection finished.
enum Ended {
    /// The caller asked the supervisor to stop.
    Stopped,
    /// ssh exited on its own, with whatever it wrote to stderr — the only place
    /// a server that cannot run the pump says so.
    Exited(String),
}

/// The sentence for a connection that keeps failing in a way `diagnose` has no
/// pattern for.
///
/// Two of them, because one of the two is not a network fault at all and saying
/// it was sent developers looking at their wifi. A pump that outlived its
/// laptop — the connection dropped, the server never noticed, and the process
/// stayed bound to the socket — refuses every replacement with `already
/// serving`, so the `ssh` reaches the server perfectly and comes back with a
/// message about a *colleague's* session. "Cannot reach this server" is the one
/// thing that is definitely not happening.
///
/// It resolves itself now, which is why the wording says to wait rather than to
/// do something: the pump gives the socket up once its own keepalive goes
/// unanswered, and the next attempt binds it.
fn cannot_connect(stderr: &str) -> Failure {
    if stderr.to_ascii_lowercase().contains("already serving") {
        return Failure::new(
            "another session on this server is still holding the channel",
            "Nothing to do — it is usually a session whose connection dropped without the \
             server noticing, and it gives the channel up within a minute. If paste is still \
             dead after that, run `riabuild channel status` on the server.",
        )
        .detail(stderr.trim().to_string());
    }

    Failure::new(
        "the clipboard channel cannot reach this server",
        "Run `riabuild channel status` on the server to check, and `riabuild remote` again \
         from here to rebuild it. Everything except paste works without it.",
    )
    .detail(stderr.trim().to_string())
}

/// Shows a failure without claiming riabuild stopped.
///
/// `Ui::failure` prints "riabuild stopped:", which would be a lie here. The
/// setup run, the secrets and the mosh session are all untouched — only paste
/// stops — and sending a developer to look for a broken environment they do not
/// have is worse than saying nothing.
///
/// **On the bar where there is one, and printed only where there is not.** This
/// runs beside the developer's remote shell, which means printing it lands
/// multi-line prose in the middle of a screen mosh and Claude Code are drawing,
/// through a terminal an interactive shell has put in raw mode — where `\n`
/// drops a row without returning to column one, so the folded sentence arrives
/// as a staircase and stays there. One line at a fixed row, with the cursor put
/// back, is the whole of the repair; the detail and the remedy are what the bar
/// cannot carry, and `riabuild channel status` is where a developer gets them.
fn report(ui: &Ui, bar: &StatusBar, failure: Failure) -> Failure {
    if bar.enabled() {
        bar.show(&format!(
            "▲ Clipboard channel — {} · paste is off",
            failure.attempting
        ));
        return failure;
    }

    ui.warn(&format!("Clipboard channel — {failure}"));
    for line in failure
        .detail
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(4)
    {
        ui.note(line);
    }
    ui.info("Paste will not work for the rest of this session. Nothing else is affected.");
    failure
}

#[cfg(test)]
mod tests {
    use super::super::tests::tunnel;
    use super::*;
    use crate::agent::tests::agent_holding;
    use crate::mime::TEXT;
    use riabuild_runner::FakeRunner;
    use std::time::Duration;

    fn agent() -> Arc<Agent> {
        Arc::new(agent_holding(&[TEXT], b"hello"))
    }

    /// Virtual time from now until the fake has started `count` children.
    ///
    /// Polling rather than a channel, because under a paused clock the polling
    /// *is* the mechanism: time only moves when the runtime has nothing left to
    /// run, so this sleep is what lets the supervisor's backoff elapse.
    async fn until_spawns(fake: &FakeRunner, count: usize) {
        for _ in 0..2_000 {
            if fake.spawns().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("only {} of {count} spawns happened", fake.spawns().len());
    }

    /// The whole point, asserted against what actually reaches `ssh`: a command
    /// on the server, and no forward anywhere.
    #[tokio::test(start_paused = true)]
    async fn the_supervisor_runs_a_command_and_requests_no_forward() {
        let fake = Arc::new(FakeRunner::new());
        let stop = Stop::new();
        let supervising = tokio::spawn(supervise(
            fake.clone(),
            tunnel(),
            agent(),
            Ui::new(true),
            stop.clone(),
            Arc::new(StatusBar::disabled()),
        ));

        until_spawns(&fake, 1).await;
        let spawned = fake.spawns().join(" ");
        assert!(spawned.contains("channel pump"), "{spawned}");
        assert!(!spawned.contains("-R"), "{spawned}");

        stop.stop();
        assert!(supervising.await.expect("join").is_none());
    }

    /// A stop must end the loop *and* kill the connection: an ssh left behind
    /// holds a pump on the server that the next session would collide with.
    #[tokio::test(start_paused = true)]
    async fn a_stop_kills_the_connection_and_returns_no_failure() {
        let fake = Arc::new(FakeRunner::new());
        let stop = Stop::new();
        let supervising = tokio::spawn(supervise(
            fake.clone(),
            tunnel(),
            agent(),
            Ui::new(true),
            stop.clone(),
            Arc::new(StatusBar::disabled()),
        ));

        until_spawns(&fake, 1).await;
        stop.stop();

        assert!(supervising.await.expect("join").is_none());
        assert_eq!(
            fake.killed().len(),
            1,
            "the ssh must be killed: {:?}",
            fake.killed()
        );
    }

    /// An ordinary disconnect is rebuilt rather than reported. This is a laptop
    /// that slept, and there is nothing for the developer to do about it.
    #[tokio::test(start_paused = true)]
    async fn an_ordinary_disconnect_is_retried() {
        let fake = Arc::new(
            FakeRunner::new()
                .spawning("ssh", 255, "Connection closed by remote host")
                .spawning("ssh", 255, "Connection closed by remote host"),
        );
        let stop = Stop::new();
        let supervising = tokio::spawn(supervise(
            fake.clone(),
            tunnel(),
            agent(),
            Ui::new(true),
            stop.clone(),
            Arc::new(StatusBar::disabled()),
        ));

        until_spawns(&fake, 2).await;
        stop.stop();
        assert!(supervising.await.expect("join").is_none());
    }

    /// A failure nobody has written a sentence for still has to produce one.
    ///
    /// This is the gap that hid a real bug for the whole life of the exec
    /// transport: the channel's `ssh` was refusing an unverifiable host key,
    /// `diagnose` matched none of its patterns, and the loop retried in silence
    /// for the length of every session. Three rounds of "paste does not work"
    /// went by with nothing anywhere naming a cause.
    #[test]
    fn a_failure_nobody_recognises_is_still_said_once() {
        // Silent while an ordinary reconnect might still succeed.
        assert!(!should_say_it_cannot_connect(false, false, 0));
        assert!(!should_say_it_cannot_connect(
            false,
            false,
            QUIET_FAILURES - 1
        ));
        // Then said.
        assert!(should_say_it_cannot_connect(false, false, QUIET_FAILURES));
        // Once. At the backoff ceiling, "every time" is a line every thirty
        // seconds printed over whatever the developer is doing.
        assert!(!should_say_it_cannot_connect(false, true, QUIET_FAILURES));
        // And never for a channel that has worked: that is a laptop that
        // slept, and there is nothing for anyone to do about it.
        assert!(!should_say_it_cannot_connect(true, false, QUIET_FAILURES));
        assert!(!should_say_it_cannot_connect(true, false, 99));
    }

    /// The wall that is not a network fault, told apart from the one that is.
    ///
    /// `already serving` comes back from a server the `ssh` reached perfectly:
    /// a pump that outlived its laptop is still bound to the socket and refuses
    /// the replacement. Reported as "cannot reach this server" — which is what
    /// every unrecognised failure used to become — it sends a developer to look
    /// at their network, which is the one thing that is definitely working.
    #[test]
    fn a_socket_another_pump_still_holds_is_not_reported_as_an_unreachable_server() {
        let held = cannot_connect(
            "riabuild stopped: another riabuild is already serving the clipboard channel at /x",
        );
        assert!(
            !held.to_string().contains("cannot reach"),
            "{held} blames the network for a server that answered"
        );
        assert!(held.attempting.contains("another session"), "{held}");
        // And it says to wait rather than to do something, because the other
        // pump's own keepalive is what ends this.
        assert!(held.action.contains("within a minute"), "{held}");

        let unreachable = cannot_connect("ssh: connect to host build-01 port 22: No route to host");
        assert!(
            unreachable.attempting.contains("cannot reach"),
            "{unreachable}"
        );
    }

    /// A server with no pump is a wall: every attempt fails identically, so the
    /// supervisor stops and says so instead of backing off to the ceiling and
    /// retrying there for the rest of the session.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_cannot_run_the_pump_stops_the_loop_with_a_failure() {
        let fake =
            Arc::new(FakeRunner::new().spawning("ssh", 127, "bash: riabuild: command not found"));
        let supervising = tokio::spawn(supervise(
            fake.clone(),
            tunnel(),
            agent(),
            Ui::new(true),
            Stop::new(),
            Arc::new(StatusBar::disabled()),
        ));

        let failure = supervising.await.expect("join").expect("a failure");
        assert!(failure.to_string().contains("pump"), "{failure}");
        assert_eq!(fake.spawns().len(), 1, "it must not retry a wall");
    }

    /// An `ssh` that will not start at all is the same wall: reported once,
    /// never retried.
    #[tokio::test(start_paused = true)]
    async fn an_ssh_that_cannot_start_is_reported_once() {
        // `NoRunner` has no `spawn_piped`, so the trait's refusing default
        // answers — which is exactly the shape of a laptop with no ssh.
        struct NoRunner;
        #[async_trait::async_trait]
        impl CommandRunner for NoRunner {
            async fn run(
                &self,
                _: &str,
                _: &[&str],
                _: &RunOptions,
            ) -> anyhow::Result<riabuild_runner::CommandOutput> {
                anyhow::bail!("no")
            }
            async fn run_bytes(
                &self,
                _: &str,
                _: &[&str],
                _: &RunOptions,
            ) -> anyhow::Result<riabuild_runner::BytesOutput> {
                anyhow::bail!("no")
            }
            async fn run_forking(
                &self,
                _: &str,
                _: &[&str],
                _: &RunOptions,
            ) -> anyhow::Result<i32> {
                anyhow::bail!("no")
            }
            async fn spawn(
                &self,
                _: &str,
                _: &[&str],
                _: &RunOptions,
            ) -> anyhow::Result<Box<dyn riabuild_runner::ChildHandle>> {
                anyhow::bail!("no")
            }
            async fn run_interactive(
                &self,
                _: &str,
                _: &[&str],
                _: &RunOptions,
            ) -> anyhow::Result<i32> {
                anyhow::bail!("no")
            }
            fn which(&self, _: &str) -> Option<std::path::PathBuf> {
                None
            }
        }

        let failure = supervise(
            Arc::new(NoRunner),
            tunnel(),
            agent(),
            Ui::new(true),
            Stop::new(),
            Arc::new(StatusBar::disabled()),
        )
        .await
        .expect("a failure");
        assert!(
            failure.to_string().contains("clipboard channel"),
            "{failure}"
        );
    }
}

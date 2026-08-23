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
//!
//! Two files beside it. `stop` is the handle a caller holds and the wait that
//! resolves when it is used; `say` is what the supervisor tells the developer
//! and when — a decision that is the whole of a predicate rather than three
//! conditions inline, because `supervise` takes an owned `Ui` and a test cannot
//! read back what it printed.

mod say;
mod stop;

// `supervisor::Stop`, not `supervisor::run::stop::Stop`: which file each half
// lives in is this module's business.
pub use stop::Stop;

use say::{cannot_connect, lost_track, report, should_say_it_cannot_connect};
use stop::stopped;

use super::{Tunnel, backoff, diagnose, ssh_args};
use crate::agent::{Agent, Served};
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::{Failure, StatusBar, Ui};
use std::sync::Arc;

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
                // `diagnose` as if ssh had said it. Its own variant is what
                // makes that comment true: as `Ended::Exited` it *was* fed to
                // `diagnose`, and the sentence below says what that cost.
                Err(error) => Ended::Lost(error.to_string()),
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
            // riabuild lost track of the ssh it started. Whatever the io error
            // says, it is a fact about *this laptop's* `wait`, never about the
            // server — so it is not shown to `diagnose`, whose every match
            // stops the channel for the rest of the session.
            Ended::Lost(detail) => {
                if should_say_it_cannot_connect(ever_connected, said_so, attempt) {
                    said_so = true;
                    report(&ui, &bar, lost_track(&detail));
                }
            }
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
    /// `child.wait()` failed. riabuild does not know how the connection ended,
    /// or whether it has.
    ///
    /// **A variant of its own because an io error is not something ssh said,
    /// and feeding it to `diagnose` killed healthy channels.** This used to be
    /// an `Exited` carrying the io error's text, right under a comment claiming
    /// it "takes the ordinary retry path rather than being fed to `diagnose`" —
    /// and `diagnose` is exactly where it went. An `ErrorKind::NotFound` from
    /// `wait` renders as `No such file or directory (os error 2)`, which
    /// matches two of the first pattern's three alternatives, so the supervisor
    /// concluded the server had no `riabuild channel pump`, said so, and
    /// stopped for the rest of the session — on a server whose pump was running
    /// and whose channel had been working a moment earlier.
    ///
    /// The wall patterns are about *what ssh wrote to stderr*. Nothing about
    /// this laptop's own syscall belongs in that vocabulary, whatever words the
    /// operating system happened to choose for it.
    Lost(String),
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

    /// I056. An io error from `child.wait()` must not be read as a wall.
    ///
    /// `wait` failing says nothing about the server — it is riabuild losing
    /// track of a process on the laptop. Carried as an `Ended::Exited`, its
    /// text went to `diagnose` like anything ssh wrote, and an
    /// `ErrorKind::NotFound` renders as `No such file or directory (os error
    /// 2)`, which matches the "the server has no `riabuild channel pump`"
    /// pattern. So the supervisor named a wall on a healthy server, printed a
    /// remedy for a machine with nothing wrong with it, and stopped for the
    /// rest of the session. Paste never came back without a new
    /// `riabuild remote`.
    #[tokio::test(start_paused = true)]
    async fn an_io_error_from_wait_is_retried_rather_than_named_as_a_wall() {
        /// A runner whose children come up fine and whose `wait` fails with the
        /// one io error whose text reads like a wall.
        struct Losing {
            spawns: std::sync::Mutex<usize>,
        }

        struct LostChild {
            // Held so the supervisor's `serve_pipe` has a pipe to serve and the
            // connection is not refused before `wait` is even reached.
            stdin: std::sync::Mutex<Option<riabuild_runner::ChildWriter>>,
            stdout: std::sync::Mutex<Option<riabuild_runner::ChildReader>>,
        }

        #[async_trait::async_trait]
        impl riabuild_runner::ChildHandle for LostChild {
            async fn wait(&self) -> anyhow::Result<riabuild_runner::CommandOutput> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No such file or directory (os error 2)",
                )
                .into())
            }
            async fn kill(&self) -> anyhow::Result<()> {
                Ok(())
            }
        }

        impl riabuild_runner::PipedChildHandle for LostChild {
            fn take_stdin(&self) -> Option<riabuild_runner::ChildWriter> {
                self.stdin.lock().ok()?.take()
            }
            fn take_stdout(&self) -> Option<riabuild_runner::ChildReader> {
                self.stdout.lock().ok()?.take()
            }
        }

        #[async_trait::async_trait]
        impl CommandRunner for Losing {
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
            async fn spawn_piped(
                &self,
                _: &str,
                _: &[&str],
                _: &RunOptions,
            ) -> anyhow::Result<Box<dyn riabuild_runner::PipedChildHandle>> {
                *self.spawns.lock().expect("lock") += 1;
                // The far ends are dropped here, so the agent's reader sees an
                // end of pipe at once and the connection is over as fast as the
                // virtual clock allows.
                let (_, ours_in) = tokio::io::duplex(64);
                let (_, ours_out) = tokio::io::duplex(64);
                Ok(Box::new(LostChild {
                    stdin: std::sync::Mutex::new(Some(Box::new(ours_in))),
                    stdout: std::sync::Mutex::new(Some(Box::new(ours_out))),
                }))
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

        let runner = Arc::new(Losing {
            spawns: std::sync::Mutex::new(0),
        });
        let stop = Stop::new();
        let supervising = tokio::spawn(supervise(
            runner.clone(),
            tunnel(),
            agent(),
            Ui::new(true),
            stop.clone(),
            Arc::new(StatusBar::disabled()),
        ));

        // Three connections is past the point a wall would have stopped it: a
        // diagnosed failure returns after the *first*.
        let mut rebuilt = false;
        for _ in 0..2_000 {
            if *runner.spawns.lock().expect("lock") >= 3 {
                rebuilt = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            rebuilt,
            "an io error from `wait` stopped the supervisor after {} connection(s); it is not \
             evidence about the server and must not be fed to `diagnose`",
            runner.spawns.lock().expect("lock")
        );

        stop.stop();
        assert!(
            supervising.await.expect("join").is_none(),
            "losing track of a child is not a failure to report"
        );
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

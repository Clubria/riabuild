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
use crate::agent::Agent;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::{Failure, Ui};
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
pub async fn supervise(
    runner: Arc<dyn CommandRunner>,
    tunnel: Tunnel,
    agent: Arc<Agent>,
    ui: Ui,
    stop: Stop,
) -> Option<Failure> {
    let mut signal = stop.signal();
    // Consecutive failures, and therefore the position in the backoff schedule.
    // Reset by a connection that actually carried a request, so a laptop that
    // suspends every afternoon reconnects in a second rather than inheriting
    // the ceiling from a bad week.
    let mut attempt = 0u32;

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
        let carried = matches!(serving.await, Ok(Ok(count)) if count > 0);

        match ended {
            Ended::Stopped => return None,
            Ended::Exited(stderr) => {
                if let Some(failure) = diagnose(&stderr) {
                    return Some(report(&ui, failure));
                }
            }
        }

        if carried {
            attempt = 0;
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

/// Shows a failure without claiming riabuild stopped.
///
/// `Ui::failure` prints "riabuild stopped:", which would be a lie here. The
/// setup run, the secrets and the mosh session are all untouched — only paste
/// stops — and sending a developer to look for a broken environment they do not
/// have is worse than saying nothing.
fn report(ui: &Ui, failure: Failure) -> Failure {
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
        )
        .await
        .expect("a failure");
        assert!(
            failure.to_string().contains("clipboard channel"),
            "{failure}"
        );
    }
}

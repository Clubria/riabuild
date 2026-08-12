//! The loop that drives the decisions next door.
//!
//! One connection at a time: spawn `ssh -N -R` and hold the handle, probe on an
//! interval while it is up, and decide from how it ended whether to rebuild or
//! to stop. The child is *held* rather than waited for, which is the whole
//! reason `CommandRunner::spawn` exists — a tunnel run through `run` would only
//! return once it had already died, and the probe has to happen while it is
//! alive.
//!
//! Nothing here propagates an error to its caller. The channel is strictly
//! optional (see `channel`'s module doc): a supervisor that cannot start, or
//! that gives up, must degrade to "no clipboard" and never to "environment
//! broken". So `supervise` returns the failure that stopped it rather than an
//! `Err` — the developer's shell is not this task's to take down, and a `?`
//! reaching a caller that used `?` in turn is exactly how it would.

use super::{PING_INTERVAL, PING_MISSES, Tunnel, backoff, diagnose, probe_args, ssh_args};
use crate::runner::{ChildHandle, CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

/// The caller's end of a running supervisor.
///
/// Cloneable and inert: holding one keeps nothing alive, so a caller that drops
/// it without stopping the supervisor gets a tunnel that shuts itself down
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

    /// Asks the supervisor to kill the tunnel and return.
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
/// wake it: the shell would exit and the tunnel would stay up behind it.
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

/// Keeps the tunnel up until asked to stop.
///
/// Returns the failure that ended it, or `None` when it ended because it was
/// told to. A returned failure has already been shown to the developer; it
/// comes back as well so the caller can put it in a banner, and so this loop's
/// hard-stop path is something a test can assert on rather than something it
/// has to scrape off stderr.
///
/// Takes an owned `Ui` because it outlives the call that started it: this runs
/// as a background task beside the developer's shell, so borrowing the caller's
/// printer would tie the tunnel's lifetime to a stack frame that returned long
/// ago.
pub async fn supervise(
    runner: Arc<dyn CommandRunner>,
    tunnel: Tunnel,
    ui: Ui,
    stop: Stop,
) -> Option<Failure> {
    let mut signal = stop.signal();
    // Consecutive failures, and therefore the position in the backoff schedule.
    // Reset by a connection that actually carried a probe, so a laptop that
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
        let child = match runner.spawn("ssh", &argv, &options).await {
            Ok(child) => child,
            Err(error) => {
                // An ssh that will not start at all does not start on the next
                // attempt either, so this is the same wall `diagnose` keeps the
                // loop off: retrying it is an infinite loop, not resilience.
                return Some(report(
                    &ui,
                    Failure::new(
                        "riabuild could not start the clipboard channel's SSH tunnel",
                        "Check that `ssh` is installed and runnable, then open a new riabuild shell. Everything except paste works without it.",
                    )
                    .command(format!("ssh {}", args.join(" ")))
                    .detail(error.to_string()),
                ));
            }
        };

        let connection = hold(child.as_ref(), runner.as_ref(), &tunnel, &mut signal).await;

        match connection.ended {
            Ended::Stopped => {
                // Killed explicitly rather than left to `kill_on_drop`: the
                // developer's shell has exited and the remote socket has to be
                // free before the next session tries to bind it.
                let _ = child.kill().await;
                return None;
            }
            Ended::Wedged => {
                let _ = child.kill().await;
            }
            Ended::Exited(stderr) => {
                if let Some(failure) = diagnose(&stderr) {
                    return Some(report(&ui, failure));
                }
            }
        }

        if connection.carried {
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
    /// The probe went unanswered `PING_MISSES` times running: ssh still
    /// believes it is connected, and the forward is carrying nothing.
    Wedged,
    /// ssh exited on its own, with whatever it wrote to stderr — the only
    /// place a server that refuses the forward says so.
    Exited(String),
}

struct Connection {
    ended: Ended,
    /// Whether a probe ever came back on this connection.
    ///
    /// This, rather than how long it lasted, is what "the connection stayed up"
    /// means: a tunnel that came up, forwarded nothing and sat there for an
    /// hour has not earned a reset of the backoff schedule.
    carried: bool,
}

/// Watches one live tunnel until something ends it.
async fn hold(
    child: &dyn ChildHandle,
    runner: &dyn CommandRunner,
    tunnel: &Tunnel,
    signal: &mut watch::Receiver<bool>,
) -> Connection {
    let mut misses = 0u32;
    let mut carried = false;

    let mut ticker = tokio::time::interval(PING_INTERVAL);
    // `interval`'s first tick is immediate, and probing a tunnel that has not
    // finished connecting would score a miss against every healthy start.
    ticker.tick().await;
    // Without this, a probe that took longer than the interval is followed by
    // an instant second tick, so a slow server burns through `PING_MISSES` in
    // no time and the supervisor tears down a tunnel that is merely sluggish.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // Recreated on every iteration, which the losing branches make
            // routine: `ChildHandle::wait` takes `&self` and releases what it
            // holds when its future is dropped, precisely so this is allowed.
            exited = child.wait() => {
                let stderr = match exited {
                    Ok(output) => output.stderr,
                    // Losing track of the child is not a configuration fault,
                    // so it takes the ordinary retry path rather than being
                    // fed to `diagnose` as if ssh had said it.
                    Err(error) => error.to_string(),
                };
                return Connection { ended: Ended::Exited(stderr), carried };
            }
            () = stopped(signal) => {
                return Connection { ended: Ended::Stopped, carried };
            }
            _ = ticker.tick() => {
                if probe(runner, tunnel).await {
                    misses = 0;
                    carried = true;
                } else {
                    misses = misses.saturating_add(1);
                    if misses >= PING_MISSES {
                        return Connection { ended: Ended::Wedged, carried };
                    }
                }
            }
        }
    }
}

/// One end-to-end health probe, run on the server over its own ssh.
///
/// The cost, stated so nobody has to rediscover it: this is **one extra
/// short-lived SSH connection every `PING_INTERVAL`**, for as long as a
/// developer's session lasts. That is the price of a probe that can observe a
/// wedged forward at all. The forward runs server→laptop, so a probe that
/// originates on the laptop — opening `tunnel.local_socket` here, say — tests
/// the agent's liveness and reports a wedged tunnel as perfectly healthy. It
/// would be cheaper, it would pass, and it would silently delete the only
/// mechanism that catches a half-open socket. Do not optimise it into one.
async fn probe(runner: &dyn CommandRunner, tunnel: &Tunnel) -> bool {
    let args = probe_args(tunnel);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    // A probe that has not answered within the interval it is measuring is a
    // miss by definition. Without the bound, one hung ssh holds this loop and
    // the supervisor stops noticing anything at all — including the exit of
    // the tunnel it is supposed to be supervising.
    let answered = tokio::time::timeout(
        PING_INTERVAL,
        runner.run(
            "ssh",
            &argv,
            &RunOptions {
                env: tunnel.env.clone(),
                ..Default::default()
            },
        ),
    )
    .await;

    matches!(answered, Ok(Ok(output)) if output.ok())
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
    use crate::runner::FakeRunner;
    use std::time::Duration;

    /// Virtual time from now until the fake has started `count` tunnels.
    ///
    /// Polling rather than a channel, because under a paused clock the polling
    /// *is* the mechanism: time only moves when the runtime has nothing left to
    /// run, so this sleep is what lets the supervisor's backoff and ping
    /// intervals elapse — instantly, in wall-clock terms. `PING_INTERVAL` is
    /// thirty seconds and the backoff ceiling is thirty more; a suite that
    /// waited them out is a suite people stop running.
    async fn spawns_reach(fake: &FakeRunner, count: usize) -> Duration {
        let start = tokio::time::Instant::now();
        for _ in 0..40_000 {
            if fake.spawns().len() >= count {
                return start.elapsed();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("only {} tunnels were started", fake.spawns().len());
    }

    /// What the supervisor returned, once its task has ended.
    async fn finished(loops: tokio::task::JoinHandle<Option<Failure>>) -> Option<Failure> {
        loops.await.expect("the supervisor task ran to completion")
    }

    fn probes(fake: &FakeRunner) -> Vec<String> {
        fake.calls()
            .into_iter()
            .filter(|call| call.contains("riabuild channel status"))
            .collect()
    }

    /// A tight reconnect loop against a server that is down is a denial of
    /// service against the developer's own machine, so the schedule the
    /// supervisor publishes has to be the one it actually waits.
    #[tokio::test(start_paused = true)]
    async fn successive_failures_wait_the_backoff_schedule_they_promise() {
        let fake = Arc::new(
            FakeRunner::new()
                .spawning("ssh", 255, "Connection to build-01 closed by remote host.")
                .spawning("ssh", 255, "Connection to build-01 closed by remote host.")
                .spawning("ssh", 255, "Connection to build-01 closed by remote host.")
                .spawning("ssh", 255, "Connection to build-01 closed by remote host."),
        );
        let stop = Stop::new();
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let loops = tokio::spawn(supervise(runner, tunnel(), Ui::new(true), stop.clone()));

        // Each call measures from where the previous one stopped, so these are
        // the gaps between attempts rather than times since the start.
        spawns_reach(&fake, 1).await;
        let first = spawns_reach(&fake, 2).await;
        let second = spawns_reach(&fake, 3).await;
        let third = spawns_reach(&fake, 4).await;

        stop.stop();
        assert!(finished(loops).await.is_none());

        // `backoff(0)` is a second exactly — its jitter falls below the floor.
        assert!(first >= Duration::from_millis(990), "{first:?}");
        assert!(first <= Duration::from_millis(1_030), "{first:?}");
        // `backoff(1)` is two seconds jittered down by up to a quarter.
        assert!(second >= Duration::from_millis(1_490), "{second:?}");
        assert!(second <= Duration::from_millis(2_030), "{second:?}");
        // `backoff(2)`, likewise, from four.
        assert!(third >= Duration::from_millis(2_990), "{third:?}");
        assert!(third <= Duration::from_millis(4_030), "{third:?}");
    }

    /// The half-open socket: ssh is happy, the forward carries nothing, and
    /// only the probe can tell. Two misses and the tunnel is rebuilt.
    #[tokio::test(start_paused = true)]
    async fn a_probe_that_stops_answering_tears_the_tunnel_down_and_rebuilds_it() {
        let fake = Arc::new(
            FakeRunner::new()
                .spawning_until_killed("ssh")
                .spawning_until_killed("ssh")
                .containing("riabuild channel status", 1, "", "the channel is down"),
        );
        let stop = Stop::new();
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let loops = tokio::spawn(supervise(runner, tunnel(), Ui::new(true), stop.clone()));

        spawns_reach(&fake, 2).await;
        // Read before stopping, because the stop tears down the *replacement*
        // tunnel too and that kill would mask the one this test is about.
        let probes = probes(&fake);
        let killed = fake.killed();
        stop.stop();
        assert!(finished(loops).await.is_none());

        assert_eq!(
            probes.len(),
            PING_MISSES as usize,
            "the tunnel should be torn down on miss {PING_MISSES}, not later: {probes:?}"
        );
        // Without this the test passes just as well against a supervisor that
        // leaks every wedged ssh it replaces — one per resume, each still
        // holding a forward.
        assert_eq!(killed.len(), 1, "{killed:?}");
        assert!(killed[0].starts_with("ssh -N -R"), "{killed:?}");
    }

    /// One dropped probe is a blip. Tearing a working tunnel down for it would
    /// make the mechanism that protects the channel the thing that interrupts
    /// it.
    #[tokio::test(start_paused = true)]
    async fn a_probe_that_answers_again_forgets_the_miss() {
        let fake = Arc::new(
            FakeRunner::new()
                .spawning_until_killed("ssh")
                .then("ssh", 1, "", "no answer")
                .containing("riabuild channel status", 0, "", ""),
        );
        let stop = Stop::new();
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let loops = tokio::spawn(supervise(runner, tunnel(), Ui::new(true), stop.clone()));

        spawns_reach(&fake, 1).await;
        tokio::time::sleep(PING_INTERVAL * 5).await;

        assert!(probes(&fake).len() >= 4, "{:?}", probes(&fake));
        assert_eq!(fake.spawns().len(), 1, "the tunnel was rebuilt anyway");
        assert!(fake.killed().is_empty(), "{:?}", fake.killed());

        stop.stop();
        assert!(finished(loops).await.is_none());
    }

    /// Retrying a server that forbids socket forwarding is an infinite loop
    /// against a wall, and the developer never learns why paste is dead.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_forbids_the_forward_stops_the_supervisor() {
        let fake = Arc::new(FakeRunner::new().spawning(
            "ssh",
            255,
            "Error: remote port forwarding failed for listen path /run/user/1000/riabuild/channel.sock",
        ));
        let runner: Arc<dyn CommandRunner> = fake.clone();

        let failure = supervise(runner, tunnel(), Ui::new(true), Stop::new())
            .await
            .expect("a refused forward is a failure the developer must act on");

        assert!(
            failure.to_string().contains("AllowStreamLocalForwarding"),
            "{failure}"
        );
        assert_eq!(fake.spawns().len(), 1, "it retried a server that said no");
    }

    /// The other half of the same decision: a laptop that closed its lid is not
    /// a configuration fault, and stopping for one would leave paste broken for
    /// the rest of the session over a blip.
    #[tokio::test(start_paused = true)]
    async fn an_ordinary_disconnect_is_retried_rather_than_reported() {
        let fake = Arc::new(
            FakeRunner::new()
                .spawning("ssh", 255, "Connection to build-01 closed by remote host.")
                .spawning_until_killed("ssh"),
        );
        let stop = Stop::new();
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let loops = tokio::spawn(supervise(runner, tunnel(), Ui::new(true), stop.clone()));

        spawns_reach(&fake, 2).await;
        stop.stop();
        let ended = finished(loops).await;
        assert!(
            ended.is_none(),
            "a disconnect was reported as something to act on: {ended:?}"
        );
    }

    /// The developer's shell has exited. A tunnel that takes until the next
    /// ping to notice holds the remote socket the next session needs.
    #[tokio::test(start_paused = true)]
    async fn a_stop_request_kills_the_tunnel_and_returns_promptly() {
        let fake = Arc::new(FakeRunner::new().spawning_until_killed("ssh"));
        let stop = Stop::new();
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let loops = tokio::spawn(supervise(runner, tunnel(), Ui::new(true), stop.clone()));

        spawns_reach(&fake, 1).await;

        let asked = tokio::time::Instant::now();
        stop.stop();
        assert!(finished(loops).await.is_none());

        // Under a paused clock an idle runtime jumps straight to the next
        // deadline, so a supervisor that only noticed on its next tick would
        // show a full `PING_INTERVAL` here.
        assert!(
            asked.elapsed() < Duration::from_secs(1),
            "{:?}",
            asked.elapsed()
        );
        assert_eq!(fake.killed().len(), 1, "{:?}", fake.killed());
    }

    /// Otherwise a laptop that suspends every afternoon inherits the ceiling
    /// from whatever went wrong last week, and a healthy reconnect waits half a
    /// minute for no reason.
    #[tokio::test(start_paused = true)]
    async fn a_connection_that_carried_traffic_starts_the_backoff_over() {
        let fake = Arc::new(
            FakeRunner::new()
                .spawning("ssh", 255, "Connection to build-01 closed by remote host.")
                .spawning("ssh", 255, "Connection to build-01 closed by remote host.")
                .spawning_until_killed("ssh")
                .spawning_until_killed("ssh")
                // The third tunnel answers once and then goes quiet, which is
                // what makes it a connection that carried traffic.
                .then("ssh", 0, "", "")
                .containing("riabuild channel status", 1, "", "the channel is down"),
        );
        let stop = Stop::new();
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let loops = tokio::spawn(supervise(runner, tunnel(), Ui::new(true), stop.clone()));

        spawns_reach(&fake, 3).await;
        let after_a_working_tunnel = spawns_reach(&fake, 4).await;
        stop.stop();
        assert!(finished(loops).await.is_none());

        // Three intervals to answer once and then miss twice, and then the
        // wait this test is about. Two failures preceded it, so an unreset
        // schedule would wait `backoff(2)` — at least three seconds.
        let waited = after_a_working_tunnel - PING_INTERVAL * 3;
        assert!(waited >= Duration::from_millis(990), "{waited:?}");
        assert!(waited <= Duration::from_millis(1_030), "{waited:?}");
    }
}

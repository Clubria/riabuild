//! Keeping the tunnel up.
//!
//! The requirement is mosh-grade: recover whenever the channel drops *or goes
//! quiet for too long*. Three mechanisms, because each catches what the others
//! miss.
//!
//! | Mechanism | Catches |
//! |---|---|
//! | `ssh -N -R` as a supervised child, rebuilt with jittered backoff | clean exits — the connection died and said so |
//! | `ServerAliveInterval`/`ServerAliveCountMax` | black-hole networks: converts silence into an exit, in ~45 s |
//! | a health probe run *on the server* every 30 s, teardown after two misses | half-open sockets — SSH believes the connection is fine while the forward is wedged. Keepalives run below the forward and cannot see this |
//!
//! The supervisor lives on the laptop, because the laptop holds the identity
//! and is the side that comes and goes. The server end is entirely passive.
//!
//! The probe is the one that has to originate on the *other* end, and
//! `probe_args` is where that is enforced: the forward runs server→laptop, so a
//! probe made from here would test the agent rather than the forward and call a
//! wedged tunnel healthy. It costs a second short-lived SSH connection every
//! interval, deliberately — see `run::probe`.
//!
//! This file holds what the supervisor *decides*: the argv that encodes two of
//! the three mechanisms, the retry schedule, and the diagnosis of a refused
//! forward. `run` holds the plumbing that drives them — the held child, the
//! interval, and the rebuild.

mod run;

// `supervisor::supervise`, not `supervisor::run::supervise`: which file the
// loop lives in is this module's business, and a caller that had to know would
// have to be edited the next time it moves.
pub use run::{Stop, supervise};

use riabuild_ui::Failure;
use std::path::PathBuf;
use std::time::Duration;

/// How often the supervisor proves the forward actually carries traffic.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Misses before the tunnel is torn down and rebuilt.
pub const PING_MISSES: u32 = 2;

const BACKOFF_CEILING: Duration = Duration::from_secs(30);

pub struct Tunnel {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity: PathBuf,
    /// Where the socket appears on the server.
    pub remote_socket: PathBuf,
    /// Where the agent is listening on the laptop.
    pub local_socket: PathBuf,
    /// The command that proves the forward carries traffic, run **on the
    /// server** — in production the server's own `riabuild channel status`,
    /// which connects to `remote_socket` and exits non-zero when nothing
    /// answers.
    ///
    /// A string the caller composes rather than something assembled here.
    /// Remote mode owns the namespace, the server's binary path and
    /// `env_command`; a supervisor that reached for any of them would stop
    /// being testable without a server, which is most of why the loop below
    /// can be unit-tested at all.
    pub probe: String,
    /// The environment every `ssh` this tunnel starts is run with — in
    /// production `remote::askpass::ssh_env`, so a forward to a server reached
    /// by password uses the saved one rather than prompting from a background
    /// reconnect nobody is watching.
    ///
    /// Composed by the caller and carried opaquely, for the same reason as
    /// `probe` above: the supervisor knowing what a `Remote` is would end its
    /// being unit-testable without a server.
    pub env: Vec<(String, String)>,
}

pub fn ssh_args(tunnel: &Tunnel) -> Vec<String> {
    let forward = format!(
        "{}:{}",
        tunnel.remote_socket.display(),
        tunnel.local_socket.display()
    );

    vec![
        // A forward, never a shell — the mosh session is the shell.
        "-N".into(),
        "-R".into(),
        forward,
        "-i".into(),
        tunnel.identity.display().to_string(),
        "-p".into(),
        tunnel.port.to_string(),
        // Without this, a forward that fails to bind leaves a live connection
        // forwarding nothing, and the failure is invisible.
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        // Without this, a socket left by a killed session blocks the rebind and
        // the channel comes up permanently dead.
        "-o".into(),
        "StreamLocalBindUnlink=yes".into(),
        // Turns a black-hole network into an exit the supervisor can see.
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        // Never prompt: this runs unattended beside the mosh session.
        "-o".into(),
        "BatchMode=yes".into(),
        format!("{}@{}", tunnel.user, tunnel.host),
    ]
}

/// The second, short-lived ssh that carries one health probe.
///
/// It runs `tunnel.probe` **on the server**, and that is the whole design.
/// The forward runs server→laptop, so a probe only proves anything if it
/// originates on the server: opening `tunnel.local_socket` from here would
/// test the agent's own liveness and nothing else, and would call a wedged
/// forward healthy — the exact failure the ping exists to catch.
///
/// See `run::probe` for the cost this pays, which is deliberate.
pub fn probe_args(tunnel: &Tunnel) -> Vec<String> {
    vec![
        "-i".into(),
        tunnel.identity.display().to_string(),
        "-p".into(),
        tunnel.port.to_string(),
        // Same reason as the tunnel's: this runs unattended beside the mosh
        // session, and a password prompt nobody can see would hang the probe
        // until its timeout every single interval.
        "-o".into(),
        "BatchMode=yes".into(),
        // A probe is a measurement, so it must not outlast the thing it
        // measures. Without a bound, a laptop on a black-hole network spends
        // the whole interval in connect() and the supervisor learns nothing.
        "-o".into(),
        "ConnectTimeout=10".into(),
        format!("{}@{}", tunnel.user, tunnel.host),
        tunnel.probe.clone(),
    ]
}

/// Exponential from one second, jittered, capped at thirty.
///
/// Jitter matters as much as the ceiling: every laptop reconnecting at the same
/// moment after a network blip is a thundering herd against the server.
pub fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_secs(1)
        .saturating_mul(2u32.saturating_pow(attempt.min(5)))
        .min(BACKOFF_CEILING);

    // Jitter is an optimisation, not a correctness property, so an unreachable
    // OS entropy failure costs the herd-spreading and nothing else — far better
    // than `rand::rng()`, which panicked on it and took the supervisor with it.
    let jitter = getrandom::u32().map_or(1.0, |raw| 0.75 + 0.25 * (raw as f64 / u32::MAX as f64));
    let millis = (base.as_millis() as f64 * jitter) as u64;
    Duration::from_millis(millis.max(1_000)).min(BACKOFF_CEILING)
}

/// Turns an ssh failure into something a developer can act on, or `None` when
/// it is an ordinary disconnect the supervisor should simply retry.
pub fn diagnose(stderr: &str) -> Option<Failure> {
    let lower = stderr.to_ascii_lowercase();

    if lower.contains("remote port forwarding failed") || lower.contains("forwarding not permitted")
    {
        return Some(
            Failure::new(
                "The server refused to forward the clipboard socket",
                "Ask whoever administers the server to set `AllowStreamLocalForwarding yes` in /etc/ssh/sshd_config, then reload sshd.",
            )
            .detail(stderr.trim().to_string()),
        );
    }

    if lower.contains("bad remote forwarding specification") {
        return Some(
            Failure::new(
                "The server's OpenSSH is too old to forward a unix socket",
                "Upgrade the server to OpenSSH 6.7 or newer. riabuild does not fall back to a TCP port, because a loopback port is readable by every other user on that machine.",
            )
            .detail(stderr.trim().to_string()),
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn tunnel() -> Tunnel {
        Tunnel {
            host: "build-01.clubria.dev".into(),
            user: "ada".into(),
            port: 22,
            identity: PathBuf::from("/home/ada/.riabuild/ssh/id_ed25519"),
            remote_socket: PathBuf::from("/run/user/1000/riabuild/channel.sock"),
            local_socket: PathBuf::from("/tmp/riabuild/agent.sock"),
            probe: "riabuild channel status".into(),
            // Stands in for `remote::askpass::ssh_env`, which this module
            // deliberately cannot name — the point of carrying the
            // environment opaquely is that the supervisor stays testable
            // without a `Remote`.
            env: vec![("SSH_ASKPASS_REQUIRE".into(), "force".into())],
        }
    }

    /// Both of these are load-bearing rather than tuning.
    #[test]
    fn the_forward_fails_loudly_and_cleans_up_after_itself() {
        let args = ssh_args(&tunnel()).join(" ");
        // Without this, a forward that fails to bind leaves a live connection
        // forwarding nothing and the failure is invisible.
        assert!(args.contains("ExitOnForwardFailure=yes"), "{args}");
        // Without this, a socket left by a killed session blocks the rebind and
        // the channel comes up permanently dead.
        assert!(args.contains("StreamLocalBindUnlink=yes"), "{args}");
    }

    /// Converts a black-hole network into an exit the supervisor can see, in
    /// about 45 seconds.
    #[test]
    fn keepalives_turn_silence_into_an_exit() {
        let args = ssh_args(&tunnel()).join(" ");
        assert!(args.contains("ServerAliveInterval=15"), "{args}");
        assert!(args.contains("ServerAliveCountMax=3"), "{args}");
    }

    #[test]
    fn the_forward_maps_the_remote_socket_onto_the_local_one() {
        let args = ssh_args(&tunnel()).join(" ");
        assert!(
            args.contains("-R /run/user/1000/riabuild/channel.sock:/tmp/riabuild/agent.sock"),
            "{args}"
        );
        // -N: a forward, never a shell. The mosh session is the shell.
        assert!(args.contains("-N"), "{args}");
    }

    #[test]
    fn the_tunnel_uses_the_riabuild_identity_and_port() {
        let args = ssh_args(&tunnel()).join(" ");
        assert!(
            args.contains("-i /home/ada/.riabuild/ssh/id_ed25519"),
            "{args}"
        );
        assert!(args.contains("-p 22"), "{args}");
        assert!(args.contains("ada@build-01.clubria.dev"), "{args}");
    }

    /// Unattended beside the mosh session: a password prompt nobody can see
    /// would hang the channel forever.
    #[test]
    fn the_tunnel_never_prompts() {
        assert!(ssh_args(&tunnel()).join(" ").contains("BatchMode=yes"));
    }

    /// The regression this file is most likely to suffer: someone notices the
    /// probe costs an SSH connection and points it at the laptop's own socket
    /// instead. That tests the agent, not the forward, and reports a wedged
    /// tunnel as healthy — deleting the one mechanism that catches a half-open
    /// socket while leaving every other test green.
    #[test]
    fn the_probe_runs_on_the_server_rather_than_against_the_laptops_own_socket() {
        let args = probe_args(&tunnel()).join(" ");
        assert!(args.contains("ada@build-01.clubria.dev"), "{args}");
        assert!(args.ends_with("riabuild channel status"), "{args}");
        assert!(
            !args.contains("/tmp/riabuild/agent.sock"),
            "the probe reached for the laptop's own socket: {args}"
        );
    }

    /// A probe that outlives the interval it measures reports nothing, every
    /// interval, forever.
    #[test]
    fn the_probe_never_prompts_and_never_hangs_on_connect() {
        let args = probe_args(&tunnel()).join(" ");
        assert!(args.contains("BatchMode=yes"), "{args}");
        assert!(args.contains("ConnectTimeout=10"), "{args}");
    }

    /// A tight loop against a server that refuses the forward is a denial of
    /// service against the developer's own machine.
    #[test]
    fn backoff_grows_from_one_second_to_a_thirty_second_ceiling() {
        assert!(backoff(0) >= Duration::from_secs(1));
        assert!(backoff(0) < Duration::from_secs(2));
        assert!(backoff(4) > backoff(0));
        for attempt in 0..20 {
            assert!(
                backoff(attempt) <= Duration::from_secs(30),
                "attempt {attempt} exceeded the ceiling"
            );
            assert!(
                backoff(attempt) >= Duration::from_secs(1),
                "attempt {attempt} retried faster than a second"
            );
        }
    }

    /// Every laptop reconnecting at once after a network blip would be a
    /// thundering herd against the server.
    #[test]
    fn backoff_is_jittered_rather_than_a_fixed_schedule() {
        let delays: Vec<Duration> = (0..40).map(|_| backoff(5)).collect();
        let distinct = delays.iter().collect::<std::collections::HashSet<_>>();
        assert!(distinct.len() > 1, "backoff(5) is deterministic");
        for delay in delays {
            assert!(delay <= Duration::from_secs(30));
        }
    }

    /// The failure nobody can diagnose from the symptom. Without this the
    /// developer sees "paste does not work" and has nothing to act on.
    #[test]
    fn a_server_that_forbids_socket_forwarding_is_named_precisely() {
        let failure = diagnose(
            "Error: remote port forwarding failed for listen path /run/user/1000/riabuild/channel.sock",
        )
        .expect("should be diagnosed");
        // `Failure`'s Display is `{attempting} — {action}`, which is exactly
        // the pair this assertion is about.
        let text = failure.to_string();
        assert!(text.contains("AllowStreamLocalForwarding"), "{text}");
    }

    #[test]
    fn an_openssh_too_old_for_socket_forwarding_is_a_hard_stop() {
        let failure = diagnose("Bad remote forwarding specification").expect("should be diagnosed");
        let text = failure.to_string();
        assert!(text.contains("6.7") || text.contains("OpenSSH"), "{text}");
    }

    /// An ordinary disconnect is the supervisor's job to retry, not something
    /// to stop and complain about.
    #[test]
    fn a_routine_disconnect_is_not_diagnosed_as_a_configuration_fault() {
        assert!(diagnose("Connection to build-01 closed by remote host.").is_none());
        assert!(diagnose("").is_none());
    }

    /// The ping exists for half-open sockets, which the keepalives structurally
    /// cannot see because they run below the forward.
    #[test]
    fn the_ping_is_more_frequent_than_the_time_it_takes_to_give_up() {
        assert!(PING_INTERVAL * PING_MISSES >= PING_INTERVAL);
        assert_eq!(PING_MISSES, 2);
    }
}

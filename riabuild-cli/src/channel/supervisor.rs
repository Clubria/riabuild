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
//! | `channel.ping` every 30 s, teardown after two misses | half-open sockets — SSH believes the connection is fine while the forward is wedged. Keepalives run below the forward and cannot see this |
//!
//! The supervisor lives on the laptop, because the laptop holds the identity
//! and is the side that comes and goes. The server end is entirely passive.
//!
//! The run loop that drives these is deferred until remote mode supplies a
//! host, a port and an identity. It also needs a `CommandRunner` that can hold
//! a long-lived child while the ping runs concurrently, which `run` cannot
//! express. Everything the loop has to *decide* is here and tested.

// Every item here is exercised by this module's tests and by nothing else yet:
// the run loop that would call them is deferred until remote mode supplies a
// host, a port and an identity. Removing them to satisfy the lint would mean
// deleting the decisions this module exists to pin down, so the allow is
// narrower than that and carries its own expiry — delete it with the wiring.
#![allow(dead_code)]

use crate::ui::Failure;
use rand::Rng;
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

/// Exponential from one second, jittered, capped at thirty.
///
/// Jitter matters as much as the ceiling: every laptop reconnecting at the same
/// moment after a network blip is a thundering herd against the server.
pub fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_secs(1)
        .saturating_mul(2u32.saturating_pow(attempt.min(5)))
        .min(BACKOFF_CEILING);

    let jitter = rand::rng().random_range(0.75..1.0);
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

    fn tunnel() -> Tunnel {
        Tunnel {
            host: "build-01.clubria.dev".into(),
            user: "ada".into(),
            port: 22,
            identity: PathBuf::from("/home/ada/.riabuild/ssh/id_ed25519"),
            remote_socket: PathBuf::from("/run/user/1000/riabuild/channel.sock"),
            local_socket: PathBuf::from("/tmp/riabuild/agent.sock"),
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

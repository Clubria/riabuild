//! Keeping the channel up.
//!
//! The requirement is mosh-grade: recover whenever the channel drops *or goes
//! quiet for too long*. Two mechanisms now, where there were three.
//!
//! | Mechanism | Catches |
//! |---|---|
//! | `ssh -T <host> riabuild channel pump` as a supervised child, rebuilt with jittered backoff | clean exits — the connection died and said so |
//! | `ServerAliveInterval`/`ServerAliveCountMax` | black-hole networks: converts silence into an exit, in ~45 s |
//!
//! **The third one is gone because what it watched for no longer exists.** Under
//! `ssh -N -R` the data path was a *forwarded socket*, a separate channel from
//! the ssh session carrying it, so ssh could believe itself perfectly connected
//! while the forward was wedged — keepalives run below a forward and cannot see
//! it. Catching that took a health probe executed on the server every thirty
//! seconds, which cost a second short-lived SSH connection per interval for the
//! whole of a developer's session.
//!
//! Here the data path *is* the ssh session's stdio. There is no second channel
//! to wedge independently: if requests are not arriving, either the pipe has
//! closed — which `serve_pipe` reports as an end — or the connection has gone
//! silent, which is exactly what `ServerAliveInterval` measures, on the same
//! connection the bytes travel over. So the probe is not an optimisation that
//! was removed; it is a question that stopped being askable, and the extra ssh
//! goes with it.
//!
//! The supervisor lives on the laptop, because the laptop holds the identity and
//! is the side that comes and goes. The server end is a relay.
//!
//! This file holds what the supervisor *decides*: the argv, the retry schedule,
//! and the diagnosis of a connection that will not come up. `run` holds the
//! plumbing that drives them — the held child, the agent on its pipe, and the
//! rebuild.

mod run;

// `supervisor::supervise`, not `supervisor::run::supervise`: which file the
// loop lives in is this module's business, and a caller that had to know would
// have to be edited the next time it moves.
pub use run::{Stop, supervise};

use riabuild_ui::Failure;
use std::path::PathBuf;
use std::time::Duration;

const BACKOFF_CEILING: Duration = Duration::from_secs(30);

pub struct Tunnel {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity: PathBuf,
    /// The server's own `riabuild channel pump`, environment prefix and all.
    ///
    /// A string the caller composes rather than something assembled here.
    /// Remote mode owns the namespace, the server's binary path and
    /// `env_command`; a supervisor that reached for any of them would stop
    /// being testable without a server, which is most of why the loop next door
    /// can be unit-tested at all.
    pub command: String,
    /// The environment every `ssh` this tunnel starts is run with — in
    /// production `remote::askpass::ssh_env`, so a channel to a server reached
    /// by password uses the saved one rather than prompting from a background
    /// reconnect nobody is watching.
    ///
    /// Composed by the caller and carried opaquely, for the same reason as
    /// `command` above.
    pub env: Vec<(String, String)>,
}

pub fn ssh_args(tunnel: &Tunnel) -> Vec<String> {
    vec![
        // No pty. The channel's framing is binary — a screenshot travels as raw
        // bytes — and a pty would translate newlines, expand tabs and treat
        // 0x03 as an interrupt, corrupting every payload that happened to
        // contain one. `-T` is not a tidiness flag here; without it the
        // transport does not work at all.
        "-T".into(),
        "-i".into(),
        tunnel.identity.display().to_string(),
        "-p".into(),
        tunnel.port.to_string(),
        // Turns a black-hole network into an exit the supervisor can see.
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        // Never prompt: this runs unattended beside the mosh session.
        "-o".into(),
        "BatchMode=yes".into(),
        format!("{}@{}", tunnel.user, tunnel.host),
        // The whole of what this connection asks the server for: run a command.
        // No `-R`, no `ExitOnForwardFailure`, no `StreamLocalBindUnlink` — the
        // three options that made the channel depend on a forwarding permission
        // most hardened servers refuse and some SSH implementations have never
        // implemented. Nothing here needs a line in anyone's `sshd_config`.
        tunnel.command.clone(),
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
///
/// Deliberately short. The forwarding diagnoses that used to live here named a
/// cause they could not establish — every `remote port forwarding failed` was
/// reported as `AllowStreamLocalForwarding`, a directive that defaults to `yes`
/// and was usually not the reason — and a confident wrong instruction costs
/// more than no instruction. Nothing below claims a cause the text does not
/// state.
pub fn diagnose(stderr: &str) -> Option<Failure> {
    let lower = stderr.to_ascii_lowercase();

    // The server's riabuild is missing or too old to have a pump. Worth
    // naming, because retrying cannot fix it: every attempt fails identically
    // and the loop would back off to the ceiling and stay there.
    if lower.contains("command not found")
        || lower.contains("no such file or directory")
        || lower.contains("not found")
    {
        return Some(
            Failure::new(
                "the server has no `riabuild channel pump` to run",
                "Run `riabuild remote` again to reinstall riabuild on that server. Everything except paste works without it.",
            )
            .detail(stderr.trim().to_string()),
        );
    }

    if lower.contains("permission denied") || lower.contains("authentication failed") {
        return Some(
            Failure::new(
                "the server refused the clipboard channel's SSH key",
                "Run `riabuild remote` again to re-authorise this laptop. Everything except paste works without it.",
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
            command: "env 'RIABUILD_CHANNEL_SOCKET=/home/ada/.riabuild-remote/abc/channel.sock' \
                      /home/ada/.riabuild/bin/riabuild channel pump"
                .into(),
            env: Vec::new(),
        }
    }

    /// The point of the whole change, pinned: the channel asks the server for a
    /// command and for nothing else. A `-R` reappearing here is the dependency
    /// on `AllowStreamLocalForwarding` coming back, and with it a channel that
    /// cannot work on a server whose sshd forbids socket forwarding or has
    /// never implemented it.
    #[test]
    fn the_channel_asks_for_a_command_and_never_a_forward() {
        let args = ssh_args(&tunnel()).join(" ");

        assert!(args.contains("channel pump"), "{args}");
        assert!(
            !args.contains("-R"),
            "no remote forward may be requested: {args}"
        );
        assert!(
            !args.contains("StreamLocalBindUnlink"),
            "the socket is the pump's to manage now: {args}"
        );
        assert!(
            !args.contains("ExitOnForwardFailure"),
            "there is no forward to fail: {args}"
        );
    }

    /// Without `-T` the payloads are corrupted rather than merely untidy: a pty
    /// translates newlines and eats control bytes, and a screenshot is full of
    /// both.
    #[test]
    fn the_session_takes_no_pty() {
        assert!(ssh_args(&tunnel()).contains(&"-T".to_string()));
    }

    /// A background reconnect nobody is watching must never sit on a prompt.
    #[test]
    fn the_connection_never_prompts_and_notices_a_dead_network() {
        let args = ssh_args(&tunnel()).join(" ");
        assert!(args.contains("BatchMode=yes"), "{args}");
        assert!(args.contains("ServerAliveInterval=15"), "{args}");
        assert!(args.contains("ServerAliveCountMax=3"), "{args}");
    }

    /// The command is the last argument, after the destination — anywhere else
    /// and ssh reads it as an option or as part of the host.
    #[test]
    fn the_command_comes_after_the_destination() {
        let args = ssh_args(&tunnel());
        let destination = args
            .iter()
            .position(|arg| arg == "ada@build-01.clubria.dev")
            .expect("a destination");
        let command = args
            .iter()
            .position(|arg| arg.contains("channel pump"))
            .expect("a command");
        assert!(command > destination, "{args:?}");
    }

    /// A server with no pump cannot be fixed by retrying, so it is named.
    #[test]
    fn a_server_with_no_pump_is_named_rather_than_retried_forever() {
        let failure =
            diagnose("bash: riabuild: command not found").expect("a server with no riabuild");
        assert!(failure.to_string().contains("pump"), "{failure}");
        assert!(
            failure.action.contains("riabuild remote"),
            "{}",
            failure.action
        );
    }

    /// The regression this change exists to prevent: a confident instruction
    /// naming a cause the text does not support. Every `remote port forwarding
    /// failed` used to be reported as `AllowStreamLocalForwarding`, which
    /// defaults to `yes` and was usually not the reason.
    #[test]
    fn no_diagnosis_names_a_forwarding_directive() {
        for stderr in [
            "remote port forwarding failed for listen path /home/dev/.riabuild-remote/x/channel.sock",
            "Warning: remote port forwarding failed",
            "forwarding not permitted",
        ] {
            let advice = diagnose(stderr).map(|failure| failure.to_string() + &failure.action);
            assert!(
                !advice.unwrap_or_default().contains("StreamLocalForwarding"),
                "a cause that cannot be established from `{stderr}` must not be asserted"
            );
        }
    }

    /// An ordinary disconnect is retried silently rather than turned into a
    /// banner: a laptop that closed its lid has nothing for a developer to fix.
    #[test]
    fn an_ordinary_disconnect_is_not_diagnosed() {
        assert!(diagnose("Connection to build-01 closed by remote host.").is_none());
        assert!(diagnose("").is_none());
    }

    #[test]
    fn backoff_climbs_and_is_capped() {
        assert!(backoff(0) >= Duration::from_secs(1));
        assert!(backoff(10) <= BACKOFF_CEILING);
        assert!(backoff(0) < BACKOFF_CEILING);
    }
}

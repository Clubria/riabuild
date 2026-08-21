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

mod bar;
mod run;

// `supervisor::supervise`, not `supervisor::run::supervise`: which file the
// loop lives in is this module's business, and a caller that had to know would
// have to be edited the next time it moves.
pub use run::{Stop, supervise};

/// The one line the channel speaks on while a full-screen shell owns the
/// screen — see `bar`. Remote mode starts it, because remote mode is what
/// knows a shell is about to take the terminal over.
pub use bar::StatusLine;

use riabuild_ui::Failure;
use std::time::Duration;

const BACKOFF_CEILING: Duration = Duration::from_secs(30);

pub struct Tunnel {
    pub host: String,
    pub user: String,
    /// Every `ssh` option this connection needs, composed by the caller —
    /// `remote::identity::ssh_options`, the same list the setup run, the mosh
    /// bootstrap and the developer's own shell are built from.
    ///
    /// **Composed rather than assembled here, and that is load-bearing.** This
    /// used to be a `port` and an `identity` that `ssh_args` turned into `-p`
    /// and `-i` itself, which looked complete and was missing everything else
    /// remote mode knows about reaching a server. Two of the omissions were
    /// fatal and neither said so:
    ///
    /// - riabuild records a server's host key in **its own** `known_hosts`,
    ///   never `~/.ssh/known_hosts`. Without `UserKnownHostsFile` the channel's
    ///   `ssh` read the developer's file, did not find the host, and — under
    ///   the `BatchMode=yes` below, which is right and stays — exited with
    ///   `Host key verification failed`. A server the developer had once
    ///   `ssh`'d to by hand worked; one only riabuild had ever reached never
    ///   did, and the difference was invisible from either end.
    /// - an issued identity riabuild is *carrying* (`IdentityAgent`) never
    ///   reached this connection, so the servers that feature exists for —
    ///   the ones riabuild's own key cannot sign in to — could never carry a
    ///   channel at all.
    ///
    /// It also silently opted out of `-F /dev/null`, so a `Host` block in the
    /// developer's `~/.ssh/config` could redirect the one connection in remote
    /// mode that was supposed to be unredirectable.
    ///
    /// The rule this restores is the one `command` below already states: remote
    /// mode owns how a server is reached, and a supervisor that reinvents any
    /// part of it drifts from the flow it belongs to without anything failing
    /// to compile.
    pub options: Vec<String>,
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
    let mut args: Vec<String> = vec![
        // No pty. The channel's framing is binary — a screenshot travels as raw
        // bytes — and a pty would translate newlines, expand tabs and treat
        // 0x03 as an interrupt, corrupting every payload that happened to
        // contain one. `-T` is not a tidiness flag here; without it the
        // transport does not work at all.
        "-T".into(),
    ];
    // How this server is reached — the port, riabuild's own known_hosts and
    // key, and any issued identity being carried. The caller's list, never
    // this file's guess at it; see `Tunnel::options`.
    args.extend(tunnel.options.iter().cloned());
    args.extend([
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
    ]);
    args
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
    let lower = decisive(stderr);

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

    // The wall that hid the bug above for as long as it existed. `ssh` under
    // `BatchMode=yes` refuses a host it cannot verify, every attempt fails
    // identically, and none of the patterns here matched it — so the loop
    // backed off to the ceiling and retried in silence for the whole session,
    // with a banner overhead that said "connected". Named now so that the next
    // reason a channel cannot come up costs a message rather than a mystery.
    if lower.contains("host key verification failed") {
        return Some(
            Failure::new(
                "the clipboard channel could not verify the server's host key",
                "Run `riabuild remote` again to record the server's host key. Everything except paste works without it.",
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

/// What ssh said about why it gave up, with what it said on the way there taken
/// out first.
///
/// Every pattern above is a `contains` over the whole of stderr, and a match
/// **stops the supervisor for the rest of the session**. That is the right
/// answer for a wall and the worst possible one for a blip, so what the
/// patterns are matched against has to be the reason ssh stopped rather than
/// everything ssh happened to write. Two kinds of line come out, and both carry
/// words that read as decisive while being nothing of the sort:
///
/// - **`Warning: Identity file … not accessible: No such file or directory.`**
///   is ssh saying it will offer one key fewer and then carrying on. Left in,
///   it turns every ordinary disconnect underneath it into "the server has no
///   `riabuild channel pump` to run" — a wall, permanent, on a server that has
///   one.
/// - **a hostname that will not resolve** is a laptop whose network has not
///   come back yet. That is the single most common way this connection fails
///   and the one case that must always be retried, since retrying is the whole
///   of how the channel survives a closed lid. Resolvers disagree about the
///   words (`Name or service not known`, `Temporary failure in name
///   resolution`, `nodename nor servname provided`, and `Host not found`, which
///   really does contain one of the patterns above), so the line is dropped by
///   what it is about rather than by which spelling it used.
///
/// Only the *matching* is narrowed. `Failure::detail` still carries the whole
/// of stderr, because a developer reading a wall wants everything ssh said.
fn decisive(stderr: &str) -> String {
    stderr
        .to_ascii_lowercase()
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.starts_with("warning:")
                && !line.contains("could not resolve")
                && !line.contains("name or service not known")
                && !line.contains("temporary failure in name resolution")
                && !line.contains("nodename nor servname")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tunnel carrying what `remote::identity::ssh_options` really produces,
    /// rather than the `-p`/`-i` pair this file used to invent. The options are
    /// spelled out here because the tests below assert that they survive into
    /// the argv — a supervisor that dropped them would still connect on a
    /// laptop that had `ssh`'d to the box by hand, and nowhere else.
    pub(super) fn tunnel() -> Tunnel {
        Tunnel {
            host: "build-01.clubria.dev".into(),
            user: "ada".into(),
            options: [
                "-p",
                "22",
                "-F",
                "/dev/null",
                "-o",
                "UserKnownHostsFile=/home/ada/.riabuild/ssh/known_hosts",
                "-o",
                "StrictHostKeyChecking=yes",
                "-i",
                "/home/ada/.riabuild/ssh-identities/abc",
                "-o",
                "IdentitiesOnly=yes",
            ]
            .iter()
            .map(|option| option.to_string())
            .collect(),
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

    /// The bug this pins, and it was silent for the whole life of the exec
    /// transport: the channel's `ssh` composed its own argv, so it reached a
    /// server by different rules than the setup run and the shell right beside
    /// it. riabuild records a host key in **its own** `known_hosts`, so without
    /// this the channel's `ssh` read the developer's file, did not find the
    /// host, and — correctly, under `BatchMode=yes` — refused. A box the
    /// developer had once `ssh`'d to by hand worked; one only riabuild had ever
    /// reached never did.
    #[test]
    fn the_connection_is_reached_by_remote_modes_own_rules() {
        let args = ssh_args(&tunnel()).join(" ");

        // riabuild's own known_hosts, not `~/.ssh/known_hosts`.
        assert!(
            args.contains("UserKnownHostsFile=/home/ada/.riabuild/ssh/known_hosts"),
            "{args}"
        );
        assert!(args.contains("StrictHostKeyChecking=yes"), "{args}");
        // The developer's own ssh config may not redirect the one connection
        // nobody is watching.
        assert!(args.contains("-F /dev/null"), "{args}");
        assert!(
            args.contains("-i /home/ada/.riabuild/ssh-identities/abc"),
            "{args}"
        );
        assert!(args.contains("IdentitiesOnly=yes"), "{args}");
        assert!(args.contains("-p 22"), "{args}");
    }

    /// A carried identity has to reach this connection too. The servers issued
    /// keys exist for are exactly the ones riabuild's own key cannot sign in
    /// to, so a channel offering only riabuild's key can never come up there —
    /// however well the rest of the session works.
    #[test]
    fn a_carried_identity_reaches_the_channel_as_well_as_the_shell() {
        let mut tunnel = tunnel();
        tunnel.options.push("-o".into());
        tunnel
            .options
            .push("IdentityAgent=/home/ada/.riabuild/agent/abc/agent.sock".into());
        tunnel.options.push("-i".into());
        tunnel
            .options
            .push("/home/ada/.riabuild/agent/abc/issued.pub".into());

        let args = ssh_args(&tunnel).join(" ");
        assert!(
            args.contains("IdentityAgent=/home/ada/.riabuild/agent/abc/agent.sock"),
            "{args}"
        );
        assert!(args.contains("issued.pub"), "{args}");
    }

    /// The options come before the destination, like every other `ssh` argv:
    /// after it they are the remote command's arguments, not `ssh`'s.
    #[test]
    fn the_options_come_before_the_destination() {
        let args = ssh_args(&tunnel());
        let destination = args
            .iter()
            .position(|arg| arg == "ada@build-01.clubria.dev")
            .expect("a destination");
        let known_hosts = args
            .iter()
            .position(|arg| arg.starts_with("UserKnownHostsFile="))
            .expect("the known_hosts option");
        assert!(known_hosts < destination, "{args:?}");
    }

    /// A host key riabuild never recorded is a wall, not a blip: every attempt
    /// fails identically. It matched none of the patterns here for the whole
    /// life of the bug above, so the loop retried in silence for the length of
    /// a session while the banner said "connected".
    #[test]
    fn an_unverifiable_host_key_is_named_rather_than_retried_in_silence() {
        let failure = diagnose(
            "Host key verification failed.\r\nkex_exchange_identification: Connection closed",
        )
        .expect("a wall");
        assert!(failure.to_string().contains("host key"), "{failure}");
        assert!(
            failure.action.contains("riabuild remote"),
            "{}",
            failure.action
        );
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

    /// The laptop this whole design exists to survive: one that has just woken
    /// up, whose network is a second or two behind it.
    ///
    /// A wall stops the supervisor for the rest of the session, so diagnosing
    /// this one is the channel failing to come back from precisely the event it
    /// is built to come back from — and the developer is told their server has
    /// no riabuild on it, which is a confident answer about the wrong machine.
    /// `Host not found` is the spelling that made this reachable rather than
    /// theoretical: it contains `not found`.
    #[test]
    fn a_hostname_that_will_not_resolve_is_retried_rather_than_named_as_a_wall() {
        for stderr in [
            "ssh: Could not resolve hostname build-01.clubria.dev: Host not found",
            "ssh: Could not resolve hostname build-01.clubria.dev: Name or service not known",
            "ssh: Could not resolve hostname build-01: Temporary failure in name resolution",
            "ssh: Could not resolve hostname build-01: nodename nor servname provided, or not known",
        ] {
            assert!(
                diagnose(stderr).is_none(),
                "a laptop with no DNS yet must be retried, not walled off: {stderr}"
            );
        }
    }

    /// ssh warns about a key it cannot read and then carries on without it, so
    /// that line is not the reason it stopped. Read as one, it makes every
    /// ordinary disconnect underneath it permanent.
    #[test]
    fn a_warning_ssh_carried_on_from_is_not_read_as_the_reason_it_stopped() {
        let stderr = "Warning: Identity file /home/ada/.riabuild/ssh/id_ed25519 not accessible: \
                      No such file or directory.\r\n\
                      Connection closed by 10.0.0.4 port 22";
        assert!(
            diagnose(stderr).is_none(),
            "a warning is not a wall: {stderr}"
        );
    }

    /// …and narrowing what is matched must not stop a real wall being named.
    /// The warning above arrives beside genuine walls too, and a server with no
    /// pump is still a server with no pump.
    #[test]
    fn a_real_wall_is_still_named_when_a_warning_arrives_with_it() {
        let stderr = "Warning: Identity file /home/ada/.riabuild/ssh/id_ed25519 not accessible: \
                      No such file or directory.\r\n\
                      bash: riabuild: command not found";
        assert!(
            diagnose(stderr)
                .expect("a wall")
                .to_string()
                .contains("pump")
        );
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

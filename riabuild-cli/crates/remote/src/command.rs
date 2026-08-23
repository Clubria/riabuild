//! Composing a command for a server, and the environment it runs under.
//!
//! Three shapes, and which one a call needs is decided by what will interpret
//! it on the other side: a quoted argument, an `sh -c` script for anything
//! multi-step, and an `env K=V …` prefix for the paths where no shell is
//! involved at all.

use crate::{channel, session};

// Commands a server actually runs.
//
// `sshd` hands whatever `ssh` sends it to the login shell configured for
// that account — `fish`, `csh`, `bash`, whatever the developer chose on
// that box — and `mosh` skips a shell entirely (`execvp`). So nothing built
// below may lean on POSIX-only syntax (`VAR=x cmd`, unquoted `~`) that only
// some of those accept: `shell_quote` makes a value inert regardless of
// which shell (if any) re-reads it, `shell_command` names the shell
// explicitly for anything that needs one, and `env_command` sets
// environment without shell syntax at all. `ssh_once` and `resolve_home`
// are what put them on the wire.

/// Single-quotes a value for a POSIX shell, so it survives intact as one
/// argument no matter what characters it contains.
///
/// Re-exported rather than defined: this file, `riabuild env` in the binary,
/// and `shell::{bash,zsh}` all carried a byte-identical copy of the same three
/// lines, and the tests below — the ones that pin `$(…)`, a backtick and a
/// leading `-` as inert — only ever covered this one. `riabuild-tasks` is the
/// lowest crate that all three can see. Reached through `remote::shell_quote`
/// everywhere in this crate, so the fifteen call sites did not have to move.
pub use riabuild_tasks::shell::shell_quote;

/// Wraps a multi-step script so it runs under `/bin/sh`, whatever the
/// account's login shell happens to be — `fish` and `csh` do not speak
/// POSIX `sh` syntax, so a script written against it must say so explicitly
/// rather than rely on whatever the account defaults to.
pub fn shell_command(script: &str) -> String {
    format!("/bin/sh -c {}", shell_quote(script))
}

/// `env K=V … program args…`, with every part quoted.
///
/// No shell syntax at all — not even the `sh -c` wrapper `shell_command`
/// uses — so this is what survives fish and csh (which reject a bare
/// `VAR=value command` prefix as a syntax error) and mosh (which `execvp`s
/// the command with no shell to expand or interpret anything).
pub fn env_command(env: &[(&str, &str)], program: &str, args: &[&str]) -> String {
    let mut parts = vec!["env".to_string()];
    for (key, value) in env {
        parts.push(shell_quote(&format!("{key}={value}")));
    }
    parts.push(shell_quote(program));
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

/// The variable a cloudcli box reads to decide whether to wrap a session in
/// tmux, and the value that tells it not to.
///
/// riabuild opens the environment shell itself, and a profile that `exec`s
/// tmux in front of it takes the terminal riabuild is in the middle of handing
/// over: the banner, the accounts box and the `exit` that is supposed to end
/// the session all land inside a pane instead. Set as an *environment*
/// variable rather than written into a generated rcfile, because the rcfile
/// sources the developer's own config first and by then tmux has already
/// started — the answer has to be in the environment before any shell reads
/// anything.
///
/// A pair rather than two constants so the key and the value cannot be used
/// apart; [`env_prefix`] and [`shell::open`] are its two callers, and they set
/// it at different depths on purpose. See `shell::open` for why the mosh path
/// needs its own copy.
pub const NO_TMUX: (&str, &str) = ("CLOUDCLI_NO_TMUX", "1");

/// The environment the server's own riabuild runs under, as `(key, value)`
/// pairs ready for [`env_command`] — never a `VAR=x` prefix, which fish
/// rejects outright and which mosh never gives a shell the chance to parse
/// anyway.
///
/// `home` and `member_id` produce the absolute, tilde-free namespace
/// (`session::namespace`); `name` is the local label so the server's riabuild
/// can say which saved connection it is running under.
///
/// The channel socket is named here rather than left for the server to resolve,
/// and that is load-bearing rather than tidiness: several developers share one
/// Unix account on a server, so they share one uid and therefore one
/// `$XDG_RUNTIME_DIR`. A server working the path out for itself would give
/// every developer on the box the same `…/riabuild/channel.sock`, and Ada's
/// `xclip` would read Ben's laptop. Naming it also switches on
/// `shell::browser_for`, which exports `BROWSER` only where a channel exists to
/// open a link on.
///
/// [`NO_TMUX`] rides along here rather than being set anywhere further in,
/// because everything the server runs hangs off this prefix — the setup run,
/// the channel probe, `internal seed-github`, and the `riabuild shell` that
/// becomes the developer's session. `RealRunner` adds to the child's inherited
/// environment rather than replacing it, so a value set on the wire here is
/// still set for the bash riabuild spawns three processes later, before that
/// bash reads a line of the developer's own config.
pub fn env_prefix(home: &str, member_id: &str, name: &str) -> Vec<(String, String)> {
    let namespace = session::namespace(home, member_id);
    vec![
        ("RIABUILD_ROOT".to_string(), namespace.clone()),
        ("RIABUILD_REMOTE".to_string(), name.to_string()),
        (
            riabuild_channel::SOCKET_ENV.to_string(),
            channel::remote_socket(&namespace),
        ),
        (NO_TMUX.0.to_string(), NO_TMUX.1.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_makes_a_hostile_value_inert() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("with space"), "'with space'");
        // The one character that ends a single-quoted string, and the reason this
        // is a function rather than a format string at each call site.
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("; rm -rf /"), "'; rm -rf /'");
        assert_eq!(shell_quote("$(curl evil.sh)"), "'$(curl evil.sh)'");
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
        // Backticks are the other command-substitution syntax POSIX shells
        // honour, and single quotes disable them exactly the same way.
        assert_eq!(shell_quote("`curl evil.sh`"), "'`curl evil.sh`'");
        // Not exploitable today — nothing in this module puts untrusted data
        // in flag position — but a quoted value can never be mistaken for a
        // flag by whatever reads it next, so it belongs on the hostile list.
        assert_eq!(shell_quote("-rf"), "'-rf'");
    }

    #[test]
    fn a_command_never_depends_on_the_login_shell() {
        // fish would reject a `VAR=x cmd` prefix outright, and mosh runs the command
        // with no shell at all, so `env` does the work instead.
        let command = env_command(
            &[
                ("RIABUILD_ROOT", "/home/dev/.riabuild-remote/abc"),
                ("RIABUILD_REMOTE", "build-01"),
            ],
            "/home/dev/.riabuild/riabuild/2026.08.06/riabuild",
            &["--no-shell"],
        );
        assert!(command.starts_with("env "), "{command}");
        assert!(
            command.contains("'RIABUILD_ROOT=/home/dev/.riabuild-remote/abc'"),
            "{command}"
        );
        assert!(!command.contains('~'), "{command}");

        // Multi-step scripts say which shell runs them.
        let script = shell_command("mkdir -p /tmp/x && cat > /tmp/x/y");
        assert!(script.starts_with("/bin/sh -c '"), "{script}");
    }

    #[test]
    fn the_remote_invocation_carries_an_absolute_namespace_and_the_server_name() {
        let env = env_prefix(
            "/home/dev",
            "550e8400-e29b-41d4-a716-446655440000",
            "build-01",
        );
        let command = env_command(
            &env.iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
            "/home/dev/.riabuild/riabuild/2026.08.06/riabuild",
            &["--no-shell"],
        );
        assert!(
            command.contains("/home/dev/.riabuild-remote/550e8400"),
            "{command}"
        );
        assert!(command.contains("RIABUILD_REMOTE=build-01"), "{command}");
        // A tilde here is the bug: root_for refuses it, and before it did, every
        // developer on the box silently shared one namespace.
        assert!(!command.contains('~'), "{command}");
    }

    #[test]
    fn the_channel_socket_is_named_inside_the_namespace_not_left_to_the_server() {
        // The shared-uid failure: developers on one account share one
        // `$XDG_RUNTIME_DIR`, so a server resolving its own path puts all of
        // them on one socket and one developer's paste reads another's laptop.
        let env = env_prefix(
            "/home/dev",
            "550e8400-e29b-41d4-a716-446655440000",
            "build-01",
        );
        let socket = env
            .iter()
            .find(|(key, _)| key == riabuild_channel::SOCKET_ENV)
            .map(|(_, value)| value.as_str())
            .expect("every remote invocation carries the socket");
        assert_eq!(
            socket,
            "/home/dev/.riabuild-remote/550e8400-e29b-41d4-a716-446655440000/channel.sock"
        );
        // The same namespace RIABUILD_ROOT gets, not a second spelling of it.
        let root = env
            .iter()
            .find(|(key, _)| key == "RIABUILD_ROOT")
            .map(|(_, value)| value.as_str())
            .expect("root");
        assert!(socket.starts_with(root), "{socket} is not under {root}");
    }

    /// Every remote invocation, not only the shell: the setup run hands the
    /// terminal over too, and a tmux that swallowed *that* would leave the
    /// developer watching a pane instead of a provisioning run.
    #[test]
    fn no_remote_invocation_lets_the_server_wrap_the_session_in_tmux() {
        let env = env_prefix(
            "/home/dev",
            "550e8400-e29b-41d4-a716-446655440000",
            "build-01",
        );
        assert_eq!(
            env.iter()
                .find(|(key, _)| key == NO_TMUX.0)
                .map(|(_, value)| value.as_str()),
            Some(NO_TMUX.1),
            "{env:?}"
        );

        // On the wire it is a quoted `env` argument like every other pair —
        // never a `VAR=x` prefix, which fish rejects and mosh never parses.
        let command = env_command(
            &env.iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
            "/home/dev/.riabuild/riabuild/2026.08.06/riabuild",
            &["shell"],
        );
        assert!(command.starts_with("env "), "{command}");
        assert!(command.contains("'CLOUDCLI_NO_TMUX=1'"), "{command}");
    }
}

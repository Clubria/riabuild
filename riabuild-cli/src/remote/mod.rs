//! Remote mode: identifying and remembering the servers a developer has set up.
//!
//! `Remote` names a server the way a developer typed it — host, port, and
//! login — and [`Remote::hash`] derives a stable key from those three answers
//! so the same server always finds the same SSH identity on disk
//! (`~/.riabuild/ssh-identities/<hash>`). The `store` submodule persists what
//! has already been set up, in `remotes.json`. Later tasks add provisioning
//! and the shell handoff on top of this.

pub mod store;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // consumed by Task 21 (`remote::run` builds and stores a Remote)
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
    #[allow(dead_code)] // consumed by Task 21
    pub fn hash(&self) -> String {
        let host = self.host.strip_suffix('.').unwrap_or(&self.host);
        let key = format!("{}@{}:{}", self.user, host.to_ascii_lowercase(), self.port);
        let digest = crate::download::sha256_hex(key.as_bytes());
        digest[..16].to_string()
    }

    /// `user@host`, for `ssh`/`mosh`'s target argument.
    #[allow(dead_code)] // consumed by Task 21
    pub fn target(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// `[user@]host[:port]`, with the local login as the default user.
    #[allow(dead_code)] // consumed by Task 21
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
}

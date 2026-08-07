//! Per-remote SSH identity and connection options.
//!
//! Task 15 owns this module in full: trusting a host key on first connect and
//! picking the per-remote private key under
//! `~/.riabuild/ssh-identities/<hash>`. Task 13b (this file, for now) only
//! declares [`ssh_options`] so `remote::ssh_once` has something real to call
//! — every command Task 13b builds still has to survive an actual `ssh`
//! invocation in its own tests. Task 15 replaces the body; the signature is
//! the interface `remote::ssh_once` was written against and should not need
//! to change.

use crate::paths::Paths;
use crate::remote::Remote;

/// The `ssh` options common to every connection to `remote`.
///
/// `batch`, when true, adds `BatchMode=yes`: a one-shot command
/// (`ssh_once`) has no terminal attached to answer an interactive prompt
/// (a host-key confirmation, a password), so it must fail fast instead of
/// hanging forever. Task 15 extends this with `-i <identity file>` and
/// `-o StrictHostKeyChecking=…` once host trust exists; until then, `ssh`
/// falls back to the account's own default key and known-hosts handling.
pub fn ssh_options(remote: &Remote, _paths: &dyn Paths, batch: bool) -> Vec<String> {
    let mut args = vec!["-p".to_string(), remote.port.to_string()];
    if batch {
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    #[test]
    fn the_port_is_always_present() {
        let paths = crate::paths::RealPaths::rooted_at(std::env::temp_dir());
        let args = ssh_options(&remote(), &paths, false);
        assert_eq!(args, vec!["-p".to_string(), "2222".to_string()]);
    }

    #[test]
    fn batch_mode_is_added_for_a_one_shot_command() {
        let paths = crate::paths::RealPaths::rooted_at(std::env::temp_dir());
        let args = ssh_options(&remote(), &paths, true);
        assert!(args.contains(&"-o".to_string()));
        assert!(args.contains(&"BatchMode=yes".to_string()));
    }
}

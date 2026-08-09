//! Secret storage for the one secret riabuild keeps: its own session token.
//!
//! Never `~/.riabuild/`. A token on disk outlives the machine it was meant for —
//! it ends up in backups, in synced folders, and in tarballs sent to support.
//!
//! This file holds the trait every store implements and the decision of which
//! store a given machine gets. The stores themselves live beside it: `platform`
//! drives the macOS and Linux credential tools through `CommandRunner`, and
//! `file` holds the one exception to the paragraph above — a server, which has
//! no keyring to put a token in.

mod file;
mod platform;

pub(crate) use file::FileKeychain;
pub(crate) use platform::{SecretToolKeychain, SecurityCliKeychain};

use crate::runner::CommandRunner;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

#[async_trait]
pub trait Keychain: Send + Sync {
    async fn get(&self) -> Result<Option<String>>;
    async fn set(&self, token: &str) -> Result<()>;
    async fn delete(&self) -> Result<()>;
    /// Shown in diagnostics so a developer knows where the token lives.
    fn describe(&self) -> &'static str;
}

/// The keychain account a server's session is stored under, on the laptop.
pub fn remote_account(hash: &str) -> String {
    format!("remote:{hash}")
}

/// Like `for_platform`, but for a named account rather than this machine's
/// own. Used to cache a server's own session token on the laptop that minted
/// it, so a second `riabuild remote <server>` finds it without re-minting.
///
/// `RIABUILD_TOKEN` is deliberately *not* consulted here: it is this
/// machine's override, and using it for a server's session would give every
/// server the same token.
pub fn for_account(
    runner: Arc<dyn CommandRunner>,
    account: &str,
    session_token_file: Option<PathBuf>,
) -> Box<dyn Keychain> {
    if let Some(path) = session_token_file {
        return Box::new(FileKeychain::new(path));
    }
    if cfg!(target_os = "macos") {
        return Box::new(SecurityCliKeychain::for_account(runner, account));
    }
    Box::new(SecretToolKeychain::for_account(runner, account))
}

/// Reads `RIABUILD_TOKEN`. For CI and for end-to-end tests against a local
/// backend, where there is no keyring daemon to talk to.
pub struct EnvKeychain;

#[async_trait]
impl Keychain for EnvKeychain {
    async fn get(&self) -> Result<Option<String>> {
        Ok(std::env::var("RIABUILD_TOKEN")
            .ok()
            .filter(|t| !t.is_empty()))
    }

    async fn set(&self, _token: &str) -> Result<()> {
        Err(anyhow!(
            "RIABUILD_TOKEN is set, so riabuild will not store a token itself.\n\
             Unset it to sign in normally."
        ))
    }

    async fn delete(&self) -> Result<()> {
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "RIABUILD_TOKEN environment variable"
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryKeychain {
    token: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl MemoryKeychain {
    pub fn with_token(token: &str) -> Self {
        Self {
            token: std::sync::Mutex::new(Some(token.to_string())),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl Keychain for MemoryKeychain {
    async fn get(&self) -> Result<Option<String>> {
        Ok(self.token.lock().unwrap().clone())
    }

    async fn set(&self, token: &str) -> Result<()> {
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    async fn delete(&self) -> Result<()> {
        *self.token.lock().unwrap() = None;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "in-memory (test)"
    }
}

/// The outcome of [`select`] — which store, and (for the file store) where.
#[derive(Debug, PartialEq, Eq)]
enum Choice {
    Env,
    File(PathBuf),
    Macos,
    Linux,
}

/// The ordering decision itself, as a pure function of inputs rather than of
/// `cfg!`/`std::env` directly.
///
/// Pulled out of `for_platform` so the ordering is testable on any host: a
/// `cfg!(target_os = "macos")` branch can only be exercised by a test running
/// *on* macOS, which the CI that gates pull requests never does (only the
/// release workflow's tag-triggered job has a macOS runner). Taking
/// `is_macos` as a plain `bool` means a Linux test can still assert what
/// happens when it's `true`.
///
/// Order matters. An explicit `RIABUILD_TOKEN` wins so automation can run with no
/// store at all. A server comes next, *before* any platform question: a macOS
/// server has `security(1)` and a login keychain an SSH session cannot unlock, so
/// asking the platform first would pick a store that always fails.
fn select(is_macos: bool, token_env: Option<&str>, session_token_file: Option<PathBuf>) -> Choice {
    if token_env.is_some_and(|value| !value.is_empty()) {
        return Choice::Env;
    }
    if let Some(path) = session_token_file {
        return Choice::File(path);
    }
    if is_macos {
        Choice::Macos
    } else {
        Choice::Linux
    }
}

/// Picks the right store for this machine. See [`select`] for the ordering
/// this delegates to.
pub fn for_platform(
    runner: Arc<dyn CommandRunner>,
    session_token_file: Option<PathBuf>,
) -> Box<dyn Keychain> {
    let token_env = std::env::var("RIABUILD_TOKEN").ok();
    match select(
        cfg!(target_os = "macos"),
        token_env.as_deref(),
        session_token_file,
    ) {
        Choice::Env => Box::new(EnvKeychain),
        Choice::File(path) => Box::new(FileKeychain::new(path)),
        Choice::Macos => Box::new(SecurityCliKeychain::new(runner)),
        Choice::Linux => Box::new(SecretToolKeychain::new(runner)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    #[test]
    fn a_servers_session_is_stored_under_its_own_account() {
        // One laptop, several servers, one keychain: the account is what keeps
        // them apart, and revoking one must not sign the laptop out.
        assert_eq!(
            remote_account("9f2c000000000000"),
            "remote:9f2c000000000000"
        );
        assert_ne!(remote_account("aaaa"), remote_account("bbbb"));
    }

    #[test]
    fn a_server_never_reaches_for_a_keyring() {
        // A macOS server is what makes this a rule rather than a preference:
        // `security` cannot open a login keychain an SSH session has not unlocked,
        // so asking the platform first would pick a store that always fails.
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let remote = for_platform(
            runner.clone(),
            Some(PathBuf::from("/home/dev/ns/session.token")),
        );
        assert_eq!(remote.describe(), "this server's riabuild namespace");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_macos_server_still_picks_the_file_store_over_the_keychain() {
        // Pins the *ordering* in `for_platform`, not just the outcome: on a
        // machine where `cfg!(target_os = "macos")` is true, the file store must
        // still win when `session_token_file` is `Some`, because a macOS server
        // has no way to unlock its login keychain over SSH.
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let remote = for_platform(runner, Some(PathBuf::from("/home/dev/ns/session.token")));
        assert_eq!(remote.describe(), "this server's riabuild namespace");
    }

    #[test]
    fn a_laptop_with_no_session_token_file_never_selects_the_file_store() {
        // The other half of the ordering guarantee: `None` must never resolve to
        // `FileKeychain` regardless of platform. A laptop always gets its
        // platform keychain.
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let laptop = for_platform(runner, None);
        assert_ne!(laptop.describe(), "this server's riabuild namespace");
    }

    // The three tests above go through `for_platform`, so on this (Linux) host
    // `cfg!(target_os = "macos")` is always `false` inside it — meaning none of
    // them can catch a regression that swaps the remote check and the platform
    // check in `select`, and neither can PR CI, which only runs `ubuntu-latest`
    // (the sole macOS runner is `release.yml`'s tag-triggered job, not the
    // pull_request gate). The tests below call `select` directly with
    // `is_macos: true` so that exact regression is caught on any host,
    // including this one.

    #[test]
    fn select_prefers_the_file_store_over_macos_even_when_is_macos_is_true() {
        // This is the test that would fail if `select`'s remote-check and
        // platform-check branches were swapped: with `is_macos: true` and a
        // `session_token_file`, swapped code would return `Choice::Macos`
        // instead. Confirmed by temporarily swapping the branches locally —
        // this test fails with:
        //   assertion `left == right` failed
        //     left: Macos
        //    right: File("/home/dev/ns/session.token")
        let path = PathBuf::from("/home/dev/ns/session.token");
        assert_eq!(select(true, None, Some(path.clone())), Choice::File(path));
    }

    #[test]
    fn select_prefers_env_over_the_file_store_and_over_macos() {
        let path = PathBuf::from("/home/dev/ns/session.token");
        assert_eq!(select(true, Some("rb_live_token"), Some(path)), Choice::Env);
    }

    #[test]
    fn select_falls_back_to_the_platform_keyring_with_no_server_and_no_env() {
        assert_eq!(select(true, None, None), Choice::Macos);
        assert_eq!(select(false, None, None), Choice::Linux);
    }
}

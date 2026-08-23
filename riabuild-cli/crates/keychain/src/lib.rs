//! Secret storage for the one secret riabuild keeps: its own session token.
//!
//! Never `~/.riabuild/`. A token on disk outlives the machine it was meant for —
//! it ends up in backups, in synced folders, and in tarballs sent to support.
//!
//! This crate holds the trait every store implements and the decision of which
//! store a given machine gets. The stores themselves live beside it: `platform`
//! drives the macOS and Linux credential tools through `CommandRunner`, and
//! `file` holds the exceptions to the paragraph above — the machines with no
//! keyring to put a token in.
//!
//! There are two of those, and they are the same situation reached from
//! different directions: a **managed server**, which never had a keyring an SSH
//! session could unlock, and a **headless Linux machine** someone installed
//! riabuild on directly, whose keyring does not answer. The second used to be
//! unhandled — not "handled badly", unhandled: `select` had no branch for it,
//! so such a machine was given the `secret-tool` store, signed the developer in
//! through a browser, and then failed to keep the token it had just minted.
//!
//! Deciding which machine this is means asking whether a Secret Service
//! actually replies, not whether `secret-tool` is on `PATH` — see
//! [`platform::keyring_answers`], which is where that mistake lived.
//!
//! Which of them a machine gets is `selection`: [`for_platform`],
//! [`for_password`] and [`for_account`], the ordering underneath them, and the
//! platform parameter every one of the three carries.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct — but the exemption is `test` and nothing else, and must stay that
// way.
//
// It read `any(test, feature = "testing")`, which switched the lint off for
// this crate's *production* code under the one command that enforces it.
// `cargo clippy --workspace --all-targets` resolves dev-dependencies, a
// dev-dependency somewhere in the workspace asks for `testing`, and features
// unify onto the lib target — so the whole crate compiled with the allow on.
// With `test` alone the lib target is linted again, and the unit-test target
// that keeps the allow holds no production code the lib target does not.
//
// Scaffolding behind `feature = "testing"` carries its own allow where it is
// defined, which is a hole the size of a module rather than of a crate.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

mod file;
mod platform;
mod selection;

pub(crate) use file::FileKeychain;
pub(crate) use platform::{SecretToolKeychain, SecurityCliKeychain};
pub use selection::{for_account, for_password, for_platform};

use anyhow::{Result, anyhow};
use async_trait::async_trait;

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
        "the RIABUILD_TOKEN environment variable"
    }
}

#[cfg(any(test, feature = "testing"))]
#[derive(Default)]
pub struct MemoryKeychain {
    token: std::sync::Mutex<Option<String>>,
    unreadable: bool,
}

#[cfg(any(test, feature = "testing"))]
impl MemoryKeychain {
    pub fn with_token(token: &str) -> Self {
        Self {
            token: std::sync::Mutex::new(Some(token.to_string())),
            unreadable: false,
        }
    }

    /// A store that answers every read with an error — a locked keyring, or a
    /// `secret-tool` with no session bus behind it.
    ///
    /// Written for the tests that prove a path never *reaches* the keychain:
    /// "did no work" is otherwise invisible, and asserting it against a store
    /// that succeeds asserts nothing.
    pub fn unreadable() -> Self {
        Self {
            token: std::sync::Mutex::new(None),
            unreadable: true,
        }
    }
}

/// The cell above, locked without a panic.
///
/// `MemoryKeychain` is `testing`-gated but it is not test code: it is compiled
/// into the lib target of every crate that turns the feature on, so
/// `unwrap_used` applies to it like any other production line. A poisoned mutex
/// means a test panicked while holding it and has already failed; raising a
/// `PoisonError` on top would replace its message with one about locking, and
/// there is no invariant an interrupted `Option<String>` write could break.
#[cfg(any(test, feature = "testing"))]
fn held<T>(cell: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(test, feature = "testing"))]
#[async_trait]
impl Keychain for MemoryKeychain {
    async fn get(&self) -> Result<Option<String>> {
        if self.unreadable {
            anyhow::bail!("this keychain cannot be read");
        }
        Ok(held(&self.token).clone())
    }

    async fn set(&self, token: &str) -> Result<()> {
        *held(&self.token) = Some(token.to_string());
        Ok(())
    }

    async fn delete(&self) -> Result<()> {
        *held(&self.token) = None;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "in-memory (test)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

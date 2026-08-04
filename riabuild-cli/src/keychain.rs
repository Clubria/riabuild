//! Secret storage for the one secret riabuild keeps: its own session token.
//!
//! Never `~/.riabuild/`. A token on disk outlives the machine it was meant for —
//! it ends up in backups, in synced folders, and in tarballs sent to support.
//!
//! Both real implementations drive the platform's credential tool through
//! `CommandRunner`, which keeps this file free of platform crates and keeps the
//! behaviour unit-testable.

use crate::runner::{CommandRunner, RunOptions};
use anyhow::{Result, anyhow};
use std::sync::Arc;

const SERVICE: &str = "com.clubria.riabuild";
const ACCOUNT: &str = "session-token";

pub trait Keychain: Send + Sync {
    fn get(&self) -> Result<Option<String>>;
    fn set(&self, token: &str) -> Result<()>;
    fn delete(&self) -> Result<()>;
    /// Shown in diagnostics so a developer knows where the token lives.
    fn describe(&self) -> &'static str;
}

/// macOS: `security(1)`, which talks to the login keychain.
pub struct SecurityCliKeychain {
    runner: Arc<dyn CommandRunner>,
}

impl SecurityCliKeychain {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl Keychain for SecurityCliKeychain {
    fn get(&self) -> Result<Option<String>> {
        let output = self.runner.run(
            "security",
            &["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"],
            &RunOptions::default(),
        )?;
        if !output.ok() {
            return Ok(None);
        }
        let token = output.trimmed().to_string();
        Ok((!token.is_empty()).then_some(token))
    }

    fn set(&self, token: &str) -> Result<()> {
        // `-U` updates in place; without it a second login errors on a duplicate
        // item, which would make `apply()` unsafe to run twice.
        let output = self.runner.run(
            "security",
            &[
                "add-generic-password",
                "-U",
                "-s",
                SERVICE,
                "-a",
                ACCOUNT,
                "-w",
                token,
            ],
            &RunOptions::default(),
        )?;
        if output.ok() {
            Ok(())
        } else {
            Err(anyhow!(
                "could not save the riabuild token to your Keychain: {}",
                output.stderr.trim()
            ))
        }
    }

    fn delete(&self) -> Result<()> {
        self.runner.run(
            "security",
            &["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT],
            &RunOptions::default(),
        )?;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "macOS Keychain"
    }
}

/// Linux: libsecret via `secret-tool`. Present so the Linux path is an addition
/// rather than a rewrite when we get there.
pub struct SecretToolKeychain {
    runner: Arc<dyn CommandRunner>,
}

impl SecretToolKeychain {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl Keychain for SecretToolKeychain {
    fn get(&self) -> Result<Option<String>> {
        let output = self.runner.run(
            "secret-tool",
            &["lookup", "service", SERVICE, "account", ACCOUNT],
            &RunOptions::default(),
        )?;
        if !output.ok() {
            return Ok(None);
        }
        let token = output.stdout.trim().to_string();
        Ok((!token.is_empty()).then_some(token))
    }

    fn set(&self, token: &str) -> Result<()> {
        let output = self.runner.run(
            "secret-tool",
            &[
                "store",
                "--label=riabuild session token",
                "service",
                SERVICE,
                "account",
                ACCOUNT,
            ],
            &RunOptions {
                stdin: Some(token.to_string()),
                ..Default::default()
            },
        )?;
        if output.ok() {
            Ok(())
        } else {
            Err(anyhow!(
                "could not save the riabuild token to your keyring: {}",
                output.stderr.trim()
            ))
        }
    }

    fn delete(&self) -> Result<()> {
        self.runner.run(
            "secret-tool",
            &["clear", "service", SERVICE, "account", ACCOUNT],
            &RunOptions::default(),
        )?;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "system keyring"
    }
}

/// Reads `RIABUILD_TOKEN`. For CI and for end-to-end tests against a local
/// backend, where there is no keyring daemon to talk to.
pub struct EnvKeychain;

impl Keychain for EnvKeychain {
    fn get(&self) -> Result<Option<String>> {
        Ok(std::env::var("RIABUILD_TOKEN")
            .ok()
            .filter(|t| !t.is_empty()))
    }

    fn set(&self, _token: &str) -> Result<()> {
        Err(anyhow!(
            "RIABUILD_TOKEN is set, so riabuild will not store a token itself.\n\
             Unset it to sign in normally."
        ))
    }

    fn delete(&self) -> Result<()> {
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
impl Keychain for MemoryKeychain {
    fn get(&self) -> Result<Option<String>> {
        Ok(self.token.lock().unwrap().clone())
    }

    fn set(&self, token: &str) -> Result<()> {
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        *self.token.lock().unwrap() = None;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "in-memory (test)"
    }
}

/// Picks the right store for this machine. An explicit `RIABUILD_TOKEN` wins so
/// automation can run without a keyring at all.
pub fn for_platform(runner: Arc<dyn CommandRunner>) -> Box<dyn Keychain> {
    if std::env::var("RIABUILD_TOKEN").is_ok_and(|value| !value.is_empty()) {
        return Box::new(EnvKeychain);
    }
    if cfg!(target_os = "macos") {
        return Box::new(SecurityCliKeychain::new(runner));
    }
    Box::new(SecretToolKeychain::new(runner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    #[test]
    fn reads_a_token_from_the_macos_keychain() {
        let runner = Arc::new(FakeRunner::new().with(
            "security find-generic-password",
            0,
            "rb_token_value\n",
            "",
        ));
        let keychain = SecurityCliKeychain::new(runner);
        assert_eq!(keychain.get().unwrap().as_deref(), Some("rb_token_value"));
    }

    #[test]
    fn a_missing_item_is_none_rather_than_an_error() {
        let runner = Arc::new(FakeRunner::new().with(
            "security find-generic-password",
            44,
            "",
            "The specified item could not be found in the keychain.",
        ));
        let keychain = SecurityCliKeychain::new(runner);
        assert_eq!(keychain.get().unwrap(), None);
    }

    #[test]
    fn storing_twice_updates_rather_than_failing() {
        // `apply()` must be safe to run twice, which is what `-U` buys.
        let runner = Arc::new(FakeRunner::new().with("security add-generic-password", 0, "", ""));
        let keychain = SecurityCliKeychain::new(runner.clone());
        keychain.set("first").unwrap();
        keychain.set("second").unwrap();
        assert!(runner.calls().iter().all(|call| call.contains("-U")));
    }

    #[test]
    fn a_secret_never_appears_in_a_secret_tool_argument_list() {
        // Arguments are world-readable through `ps`; stdin is not.
        let runner = Arc::new(FakeRunner::new().with("secret-tool store", 0, "", ""));
        let keychain = SecretToolKeychain::new(runner.clone());
        keychain.set("super-secret").unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("super-secret"))
        );
    }
}

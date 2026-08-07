//! Secret storage for the one secret riabuild keeps: its own session token.
//!
//! Never `~/.riabuild/`. A token on disk outlives the machine it was meant for —
//! it ends up in backups, in synced folders, and in tarballs sent to support.
//!
//! Both real implementations drive the platform's credential tool through
//! `CommandRunner`, which keeps this file free of platform crates and keeps the
//! behaviour unit-testable.

use crate::runner::{CommandRunner, RunOptions};
use crate::ui::Failure;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;

const SERVICE: &str = "com.clubria.riabuild";
const ACCOUNT: &str = "session-token";

#[async_trait]
pub trait Keychain: Send + Sync {
    async fn get(&self) -> Result<Option<String>>;
    async fn set(&self, token: &str) -> Result<()>;
    async fn delete(&self) -> Result<()>;
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

#[async_trait]
impl Keychain for SecurityCliKeychain {
    async fn get(&self) -> Result<Option<String>> {
        let output = self
            .runner
            .run(
                "security",
                &["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"],
                &RunOptions::default(),
            )
            .await?;
        if !output.ok() {
            return Ok(None);
        }
        let token = output.trimmed().to_string();
        Ok((!token.is_empty()).then_some(token))
    }

    async fn set(&self, token: &str) -> Result<()> {
        // `-U` updates in place; without it a second login errors on a duplicate
        // item, which would make `apply()` unsafe to run twice.
        let output = self
            .runner
            .run(
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
            )
            .await?;
        if output.ok() {
            Ok(())
        } else {
            Err(anyhow!(
                "could not save the riabuild token to your Keychain: {}",
                output.stderr.trim()
            ))
        }
    }

    async fn delete(&self) -> Result<()> {
        self.runner
            .run(
                "security",
                &["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT],
                &RunOptions::default(),
            )
            .await?;
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

    /// A machine with no keyring is an ordinary situation on Linux — a headless
    /// box, a container, a minimal install — not a bug in riabuild. Without this
    /// guard the missing binary surfaces as a raw `os error 2` under a message
    /// telling the developer to report it, which sends them nowhere useful.
    fn ensure_available(&self) -> Result<()> {
        if self.runner.which("secret-tool").is_some() {
            return Ok(());
        }
        Err(Failure::new(
            "reading the riabuild token from your keyring",
            "Install libsecret (`sudo apt install libsecret-tools`), or set RIABUILD_TOKEN \
             to a token from the riabuild dashboard if this machine has no keyring.",
        )
        .detail("`secret-tool` is not installed, so riabuild has nowhere to keep your token")
        .into())
    }
}

#[async_trait]
impl Keychain for SecretToolKeychain {
    async fn get(&self) -> Result<Option<String>> {
        self.ensure_available()?;
        let output = self
            .runner
            .run(
                "secret-tool",
                &["lookup", "service", SERVICE, "account", ACCOUNT],
                &RunOptions::default(),
            )
            .await?;
        if !output.ok() {
            return Ok(None);
        }
        let token = output.stdout.trim().to_string();
        Ok((!token.is_empty()).then_some(token))
    }

    async fn set(&self, token: &str) -> Result<()> {
        self.ensure_available()?;
        let output = self
            .runner
            .run(
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
                    stdin: Some(token.as_bytes().to_vec()),
                    ..Default::default()
                },
            )
            .await?;
        if output.ok() {
            Ok(())
        } else {
            Err(anyhow!(
                "could not save the riabuild token to your keyring: {}",
                output.stderr.trim()
            ))
        }
    }

    async fn delete(&self) -> Result<()> {
        // Nothing to remove if there is no keyring; signing out succeeds.
        if self.runner.which("secret-tool").is_none() {
            return Ok(());
        }
        self.runner
            .run(
                "secret-tool",
                &["clear", "service", SERVICE, "account", ACCOUNT],
                &RunOptions::default(),
            )
            .await?;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "system keyring"
    }
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

    #[tokio::test]
    async fn reads_a_token_from_the_macos_keychain() {
        let runner = Arc::new(FakeRunner::new().with(
            "security find-generic-password",
            0,
            "rb_token_value\n",
            "",
        ));
        let keychain = SecurityCliKeychain::new(runner);
        assert_eq!(
            keychain.get().await.unwrap().as_deref(),
            Some("rb_token_value")
        );
    }

    #[tokio::test]
    async fn a_missing_item_is_none_rather_than_an_error() {
        let runner = Arc::new(FakeRunner::new().with(
            "security find-generic-password",
            44,
            "",
            "The specified item could not be found in the keychain.",
        ));
        let keychain = SecurityCliKeychain::new(runner);
        assert_eq!(keychain.get().await.unwrap(), None);
    }

    #[tokio::test]
    async fn storing_twice_updates_rather_than_failing() {
        // `apply()` must be safe to run twice, which is what `-U` buys.
        let runner = Arc::new(FakeRunner::new().with("security add-generic-password", 0, "", ""));
        let keychain = SecurityCliKeychain::new(runner.clone());
        keychain.set("first").await.unwrap();
        keychain.set("second").await.unwrap();
        assert!(runner.calls().iter().all(|call| call.contains("-U")));
    }

    #[tokio::test]
    async fn a_machine_with_no_keyring_gets_a_next_action_not_a_bug_report() {
        // A headless Linux box has no libsecret. That is an ordinary state, and
        // the message has to name both fixes.
        let keychain = SecretToolKeychain::new(Arc::new(FakeRunner::new()));
        let error = keychain.get().await.unwrap_err().to_string();
        assert!(error.contains("RIABUILD_TOKEN"), "{error}");
        assert!(error.contains("libsecret"), "{error}");
    }

    #[tokio::test]
    async fn signing_out_works_even_without_a_keyring() {
        let keychain = SecretToolKeychain::new(Arc::new(FakeRunner::new()));
        assert!(keychain.delete().await.is_ok());
    }

    #[tokio::test]
    async fn a_secret_never_appears_in_a_secret_tool_argument_list() {
        // Arguments are world-readable through `ps`; stdin is not.
        let runner = Arc::new(FakeRunner::new().with("secret-tool store", 0, "", ""));
        let keychain = SecretToolKeychain::new(runner.clone());
        keychain.set("super-secret").await.unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("super-secret"))
        );
    }
}

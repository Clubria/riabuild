//! The two platform credential tools: macOS `security(1)`, talking to the login
//! keychain, and Linux libsecret through `secret-tool`.
//!
//! Both drive the tool through `CommandRunner`, which keeps this file free of
//! platform crates and keeps the behaviour unit-testable. Neither is `cfg`-gated
//! for that reason: `for_platform` chooses between them at runtime, so the macOS
//! path still compiles and still has tests on the Linux host every pull request
//! is gated on.
//!
//! The token reaches either tool on **stdin**, never as an argument. argv is
//! world-readable through `ps`, and on a shared server `ps` shows other
//! developers' processes.

use super::Keychain;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::Failure;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;

const SERVICE: &str = "com.clubria.riabuild";
const ACCOUNT: &str = "session-token";

/// macOS: `security(1)`, which talks to the login keychain.
pub struct SecurityCliKeychain {
    runner: Arc<dyn CommandRunner>,
    account: String,
}

impl SecurityCliKeychain {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::for_account(runner, ACCOUNT)
    }

    /// Like `new`, but under a named account rather than this laptop's own —
    /// used to cache a server's session under `remote:<hash>`, alongside this
    /// laptop's own item, without either one overwriting the other.
    pub fn for_account(runner: Arc<dyn CommandRunner>, account: &str) -> Self {
        Self {
            runner,
            account: account.to_string(),
        }
    }
}

#[async_trait]
impl Keychain for SecurityCliKeychain {
    async fn get(&self) -> Result<Option<String>> {
        let output = self
            .runner
            .run(
                "security",
                &[
                    "find-generic-password",
                    "-s",
                    SERVICE,
                    "-a",
                    &self.account,
                    "-w",
                ],
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
        //
        // `-w` with no trailing value — never `-w <token>` — is what keeps the
        // token out of argv: with nothing after it, `security` reads the
        // password from stdin (it only falls back to an interactive prompt when
        // stdin is a terminal, which it never is here), the same way
        // `SecretToolKeychain::set` pipes its token to `secret-tool store`
        // below. `-X` would take a hex-encoded password instead, but that is
        // still an argv element and so not a fix. `ps` on this machine would
        // show the full `security add-generic-password …` invocation to every
        // other user, which is exactly what argv is: world-readable.
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
                    &self.account,
                    "-w",
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
                "could not save the riabuild token to your Keychain: {}",
                output.stderr.trim()
            ))
        }
    }

    async fn delete(&self) -> Result<()> {
        self.runner
            .run(
                "security",
                &[
                    "delete-generic-password",
                    "-s",
                    SERVICE,
                    "-a",
                    &self.account,
                ],
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
    account: String,
}

impl SecretToolKeychain {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::for_account(runner, ACCOUNT)
    }

    /// Like `new`, but under a named account rather than this laptop's own —
    /// see `SecurityCliKeychain::for_account`.
    pub fn for_account(runner: Arc<dyn CommandRunner>, account: &str) -> Self {
        Self {
            runner,
            account: account.to_string(),
        }
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
                &["lookup", "service", SERVICE, "account", &self.account],
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
                    &self.account,
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
                &["clear", "service", SERVICE, "account", &self.account],
                &RunOptions::default(),
            )
            .await?;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "system keyring"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    #[tokio::test]
    async fn a_remote_account_reads_and_writes_its_own_item() {
        let runner = Arc::new(
            FakeRunner::new()
                .with("security find-generic-password", 0, "rb_remote_token\n", "")
                .with("security add-generic-password", 0, "", ""),
        );
        let keychain = SecurityCliKeychain::for_account(runner.clone(), "remote:9f2c");
        keychain.set("rb_remote_token").await.expect("write");

        assert!(
            runner
                .calls()
                .iter()
                .any(|call| call.contains("remote:9f2c")),
            "{:?}",
            runner.calls()
        );
    }

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
    async fn a_secret_never_appears_in_a_security_argument_list() {
        // The macOS counterpart to `a_secret_never_appears_in_a_secret_tool_argument_list`
        // below. `-w` carries no trailing value in argv — the token travels
        // over stdin instead, exactly like `secret-tool store` already does —
        // so it must not show up in any recorded call. `FakeRunner::calls`
        // only ever sees `program` and `args`, never `RunOptions.stdin`, so
        // this is a faithful stand-in for what `ps` would show a real
        // co-tenant on the machine.
        //
        // Both halves are asserted. Absence from argv alone is satisfied just
        // as well by a `set()` that pipes nothing at all — and `-w` with no
        // trailing value and no stdin does not fail: on a real Mac it either
        // blocks on `security`'s interactive prompt or stores an empty
        // password. This is the only guard that could catch that, since no PR
        // gate runs this suite on macOS (`ci.yml` pins the cli job to ubuntu;
        // `release.yml`'s macOS `cargo test` is tag-triggered).
        let runner = Arc::new(FakeRunner::new().with("security add-generic-password", 0, "", ""));
        let keychain = SecurityCliKeychain::new(runner.clone());
        keychain.set("super-secret").await.unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("super-secret"))
        );
        assert_eq!(
            runner
                .stdin_text_of("security add-generic-password")
                .as_deref(),
            Some("super-secret"),
            "the token must actually reach `security` on stdin"
        );
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
        // Arguments are world-readable through `ps`; stdin is not. And the
        // token has to be on that stdin: `secret-tool store` handed an empty
        // pipe stores an empty password rather than failing, so asserting only
        // its absence from argv would pass on a `set()` that pipes nothing.
        let runner = Arc::new(FakeRunner::new().with("secret-tool store", 0, "", ""));
        let keychain = SecretToolKeychain::new(runner.clone());
        keychain.set("super-secret").await.unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("super-secret"))
        );
        assert_eq!(
            runner.stdin_text_of("secret-tool store").as_deref(),
            Some("super-secret"),
            "the token must actually reach `secret-tool` on stdin"
        );
    }

    /// Round-trips a real token through `security(1)` against an actual
    /// macOS login keychain: `set` then `get` must return exactly what was
    /// stored, the same way `claude_config_dir_smoke` in `shims/mod.rs` pins
    /// undocumented behaviour of a real external tool instead of guessing at
    /// it.
    ///
    /// This confirms the thing a unit test cannot: that `-w` with no
    /// trailing argv value genuinely reads the password from piped stdin,
    /// rather than — for instance — silently storing an empty password, or
    /// blocking forever on an interactive prompt that piped stdin can never
    /// satisfy. That belief comes from `security`'s documented behaviour,
    /// not from having run it: this repository's CI and every development
    /// container it runs in are Linux, where `security` does not exist, so
    /// nothing here has ever executed this path. Ignored for that reason —
    /// a human with a real Mac must run it with `cargo test -- --ignored`.
    ///
    /// Uses a throwaway account distinct from `session-token` so running
    /// this locally cannot clobber a developer's real riabuild sign-in, and
    /// cleans up after itself.
    #[tokio::test]
    #[ignore = "requires the security(1) CLI and a real macOS login keychain"]
    async fn security_cli_round_trips_a_token_through_a_real_keychain() {
        let runner: Arc<dyn CommandRunner> = Arc::new(crate::runner::RealRunner);
        if runner.which("security").is_none() {
            panic!("security is not installed; this test needs to run on macOS");
        }
        let keychain = SecurityCliKeychain::for_account(runner, "riabuild-test-roundtrip");

        keychain
            .set("rb_test_roundtrip_token")
            .await
            .expect("write");
        assert_eq!(
            keychain.get().await.expect("read").as_deref(),
            Some("rb_test_roundtrip_token")
        );

        keychain.delete().await.expect("cleanup");
    }
}

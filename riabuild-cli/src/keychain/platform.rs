//! The two platform credential tools: macOS `security(1)`, talking to the login
//! keychain, and Linux libsecret through `secret-tool`.
//!
//! Both drive the tool through `CommandRunner`, which keeps this file free of
//! platform crates and keeps the behaviour unit-testable. Neither is `cfg`-gated
//! for that reason: `for_platform` chooses between them at runtime, so the macOS
//! path still compiles and still has tests on the Linux host every pull request
//! is gated on.
//!
//! `secret-tool` is handed the token on **stdin**, never as an argument: argv
//! is world-readable through `ps`. `security` cannot be — it has no stdin path
//! for a password at all, only a `/dev/tty` prompt — so on macOS the token is
//! an argv element and the leak is accepted. `SecurityCliKeychain::set` carries
//! the whole argument.

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
        // The token is an argv element, and that is deliberate. `security` has
        // no way to be given a password on stdin. `-w` with nothing after it
        // does not read the pipe: it calls `readpassphrase(3)`, which opens
        // **/dev/tty** and asks the human, falling back to stdin only when
        // /dev/tty cannot be opened at all. That fallback is why the stdin
        // spelling looked right — it is the only path CI, `cargo test` and a
        // GitHub runner can ever take, none of them having a controlling
        // terminal. On the laptop this actually runs on, `riabuild remote` sat
        // at `password data for new item:` with the piped token ignored, stored
        // an empty password when the developer pressed Enter through it, and
        // then minted a fresh device session on every later run because the
        // item read back empty. `-X` takes a hex-encoded password and is still
        // argv, so it is not a fix either.
        //
        // The cost is real and narrow: for the few milliseconds this process
        // lives, `ps` shows the token to other accounts on this Mac. There is
        // no spelling that both works and avoids it. `SecretToolKeychain` below
        // keeps its pipe — `secret-tool store` documents stdin and honours it.
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
                    token,
                ],
                &RunOptions::default(),
            )
            .await?;
        if !output.ok() {
            return Err(anyhow!(
                "could not save the riabuild token to your Keychain: {}",
                output.stderr.trim()
            ));
        }
        // Then read it straight back, because an exit status is not evidence.
        // `security` returns 0 having stored an empty password, and a `set`
        // that lies is not a save that failed — it is a silent sign-in loop:
        // every later `get` answers `None`, every run mints another 90-day
        // session, and nothing anywhere says why. No CI can catch that (the
        // prompt needs a terminal no runner has), so this is the only check
        // that runs where the failure lives.
        if self.get().await?.as_deref() != Some(token) {
            return Err(Failure::new(
                "storing the riabuild token in your Keychain",
                format!(
                    "riabuild saved it and read something else back. Run \
                     `security find-generic-password -s {SERVICE} -a {}` to see what is there.",
                    self.account
                ),
            )
            .into());
        }
        Ok(())
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
        let runner = Arc::new(
            FakeRunner::new()
                .with("security add-generic-password", 0, "", "")
                .then("security find-generic-password", 0, "first\n", "")
                .then("security find-generic-password", 0, "second\n", ""),
        );
        let keychain = SecurityCliKeychain::new(runner.clone());
        keychain.set("first").await.unwrap();
        keychain.set("second").await.unwrap();
        assert!(
            runner
                .calls()
                .iter()
                .filter(|call| call.contains("add-generic-password"))
                .all(|call| call.contains("-U"))
        );
    }

    #[tokio::test]
    async fn the_token_goes_to_security_in_argv_because_there_is_nowhere_else() {
        // The deliberate exception to `a_secret_never_appears_in_a_secret_tool_argument_list`
        // below, pinned here so removing it is a decision rather than a tidy-up.
        // `security` reads a password from /dev/tty or from argv, and nothing
        // else; the stdin spelling this replaced was a prompt on a developer's
        // laptop, silent everywhere a test can run.
        let runner = Arc::new(
            FakeRunner::new()
                .with("security add-generic-password", 0, "", "")
                .with("security find-generic-password", 0, "super-secret\n", ""),
        );
        let keychain = SecurityCliKeychain::new(runner.clone());
        keychain.set("super-secret").await.unwrap();
        assert!(
            runner
                .calls()
                .iter()
                .any(|call| call.contains("-w super-secret")),
            "{:?}",
            runner.calls()
        );
        assert_eq!(
            runner
                .stdin_text_of("security add-generic-password")
                .as_deref(),
            None,
            "nothing may be piped: `security` would not read it, and a pipe here \
             is what made the last version look correct"
        );
    }

    #[tokio::test]
    async fn a_store_that_did_not_take_is_an_error_rather_than_a_sign_in_loop() {
        // The regression test for the bug this file was rewritten over. When
        // `security` exits 0 having stored something other than the token —
        // an empty password, because a human pressed Enter through a prompt
        // riabuild never meant to raise — `set` must say so. Reporting success
        // is what turned one broken write into `riabuild remote` minting a new
        // 90-day session on every single run, with nothing on screen to
        // explain it.
        let runner = Arc::new(
            FakeRunner::new()
                .with("security add-generic-password", 0, "", "")
                .with("security find-generic-password", 0, "\n", ""),
        );
        let keychain = SecurityCliKeychain::new(runner);
        let error = keychain
            .set("rb_live_token")
            .await
            .expect_err("an empty read-back is a failed store");
        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(
            failure.action.contains("find-generic-password"),
            "{}",
            failure.action
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
    /// This confirms the thing a unit test cannot, and the thing that was
    /// wrong for five days: that `set` stores a token a later `get` can
    /// actually retrieve. Run it **from a terminal**, not just from CI. The
    /// bug it exists to catch only appears where `/dev/tty` can be opened, so
    /// a green run in a runner or under `nohup` proves less than it looks —
    /// which is exactly how the previous spelling reached a release.
    ///
    /// Ignored because this repository's PR gate is Linux, where `security`
    /// does not exist: a human with a real Mac must run it with
    /// `cargo test -- --ignored`.
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

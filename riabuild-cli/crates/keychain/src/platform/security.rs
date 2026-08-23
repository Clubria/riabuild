//! macOS: `security(1)`, talking to the login keychain.
//!
//! The one store that hands a child a secret in **argv**, and the long comment
//! inside `set` is why: `security` has no stdin path for a password, `-w` with
//! nothing after it opens `/dev/tty` rather than reading the pipe, and every
//! place a test can run is a place where the pipe *looks* like it works. Read
//! that comment before touching the argument list.

use super::{ACCOUNT, SERVICE};
use crate::Keychain;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::Failure;
use std::sync::Arc;

/// `security(1)`'s exit status for `errSecItemNotFound`.
///
/// The one non-zero status from `find-generic-password` and
/// `delete-generic-password` that is an **answer** rather than a failure: this
/// account holds nothing. Every other status means riabuild could not read the
/// keychain, which is a different fact and must not be reported as an empty
/// one — see [`SecurityCliKeychain::get`].
///
/// macOS is discriminated by status where Linux is discriminated by stderr,
/// and the difference is not a style choice: `security` prints "The specified
/// item could not be found in the keychain." on stderr for an ordinary miss,
/// so [`keyring_answers`](super::keyring_answers)'s empty-stderr rule would
/// read every miss here as a
/// failure.
const SECURITY_ITEM_NOT_FOUND: i32 = 44;

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
            // "There is no such item" and "I could not ask" are different
            // facts, and collapsing the second into the first is expensive.
            // Every caller reads `Ok(None)` as *this developer is not signed
            // in* — `Ctx::connect` returns early on it and the login task then
            // runs the whole device-code flow — so a keychain that locked
            // between `keyring_answers` and this call sends the developer back
            // to a browser and mints another ninety-day session for a machine
            // that already holds one, on every run, with nothing on screen
            // saying why. That is the silent sign-in loop `set`'s read-back
            // below exists to end, reached from the reading side.
            //
            // Only `errSecItemNotFound` is the miss. Unlike `secret-tool`,
            // `security` writes a diagnostic on an ordinary miss too, so the
            // discriminator here is the status — see
            // [`SECURITY_ITEM_NOT_FOUND`].
            if output.code == Some(SECURITY_ITEM_NOT_FOUND) {
                return Ok(None);
            }
            return Err(Failure::new(
                "reading the riabuild token from your Keychain",
                "Unlock your login keychain and run `riabuild` again.",
            )
            .command(format!(
                "security find-generic-password -s {SERVICE} -a {}",
                self.account
            ))
            .detail(describe_exit(
                "security find-generic-password",
                output.code,
                &output.stderr,
            ))
            .into());
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
        // no spelling that both works and avoids it. `SecretToolKeychain`, in
        // `secret_tool.rs`, keeps its pipe — `secret-tool store` documents
        // stdin and honours it.
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
        let output = self
            .runner
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
        // The exit status was discarded here, and this is the one path where
        // that is not merely untidy: `riabuild remote forget` is a
        // *revocation*, and it told the developer the token was gone while
        // `delete-generic-password` had refused — a locked keychain being the
        // ordinary way that happens. A revocation that lies is worse than one
        // that fails, because nobody goes back to check.
        //
        // Nothing to delete is a clean sign-out rather than a failure: a
        // machine that was never signed in must still be able to sign out.
        if !output.ok() && output.code != Some(SECURITY_ITEM_NOT_FOUND) {
            return Err(Failure::new(
                "removing the riabuild token from your Keychain",
                "Unlock your login keychain and run the command again.",
            )
            .command(format!(
                "security delete-generic-password -s {SERVICE} -a {}",
                self.account
            ))
            .detail(describe_exit(
                "security delete-generic-password",
                output.code,
                &output.stderr,
            ))
            .into());
        }
        // Then read it back, for the reason `set` does: an exit status is not
        // evidence. `delete-generic-password` removes **one** matching item,
        // so a duplicate — which `-U` stops riabuild from making, and nothing
        // stops a developer or an older build from having made — survives a
        // delete that exited 0.
        if self.get().await?.is_some() {
            return Err(Failure::new(
                "removing the riabuild token from your Keychain",
                format!(
                    "Run `security delete-generic-password -s {SERVICE} -a {}` until it \
                     reports no such item.",
                    self.account
                ),
            )
            .detail(
                "riabuild deleted it and read one straight back, so the credential is \
                 still in the keychain and has not been revoked",
            )
            .into());
        }
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "your macOS Keychain"
    }
}

/// What a `security` call that failed is reported as.
///
/// Split out because the two `security` paths that can fail — reading and
/// deleting — must both say *what* went wrong, and stderr alone does not
/// always: `security` is terse, and a child killed by riabuild's own timeout
/// has no exit code at all. A `Failure` whose detail is an empty string sends
/// the developer to their keychain with nothing to look for.
fn describe_exit(command: &str, code: Option<i32>, stderr: &str) -> String {
    let status = match code {
        Some(code) => format!("exited {code}"),
        None => "was killed before it finished".to_string(),
    };
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("`{command}` {status} and said nothing")
    } else {
        format!("`{command}` {status}: {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

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
        // in `secret_tool.rs`, pinned here so removing it is a decision rather
        // than a tidy-up.
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
    async fn a_macos_keychain_that_cannot_be_read_is_not_reported_as_no_token() {
        // I031. `Ok(None)` means "this developer is not signed in" to every
        // caller — `Ctx::connect` returns early on it and the login task runs
        // the whole device-code flow — so a keychain riabuild could not
        // consult must not answer with it. Reporting the machine's real state
        // is cheap; a browser approval and another ninety-day session is not.
        let runner = Arc::new(FakeRunner::new().with(
            "security find-generic-password",
            36,
            "",
            "SecKeychainSearchCopyNext: User interaction is not allowed.",
        ));
        let keychain = SecurityCliKeychain::new(runner);
        let error = keychain
            .get()
            .await
            .expect_err("a keychain that refused to answer is not an empty one");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.action.contains("Unlock"), "{}", failure.action);
        assert!(
            failure.detail.contains("User interaction is not allowed"),
            "{}",
            failure.detail
        );
    }

    #[tokio::test]
    async fn a_macos_delete_the_keychain_refused_is_not_reported_as_revoked() {
        // I030. `riabuild remote forget` is a revocation path: it tells the
        // developer the credential is gone. The discarded exit status meant it
        // said so while `delete-generic-password` had refused on a locked
        // keychain and the token was still there.
        let runner = Arc::new(FakeRunner::new().with(
            "security delete-generic-password",
            36,
            "",
            "SecKeychainItemDelete: User interaction is not allowed.",
        ));
        let keychain = SecurityCliKeychain::new(runner);
        let error = keychain
            .delete()
            .await
            .expect_err("a refused delete is not a revocation");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.action.contains("Unlock"), "{}", failure.action);
    }

    #[tokio::test]
    async fn a_macos_item_that_survives_the_delete_is_reported() {
        // The read-back half, and the same lesson `set` carries: an exit
        // status is not evidence. `delete-generic-password` removes one
        // matching item, so a duplicate exits 0 with the credential still in
        // the keychain.
        let runner = Arc::new(
            FakeRunner::new()
                .with("security delete-generic-password", 0, "", "")
                .with("security find-generic-password", 0, "rb_still_here\n", ""),
        );
        let keychain = SecurityCliKeychain::new(runner);
        let error = keychain
            .delete()
            .await
            .expect_err("a credential still in the keychain has not been revoked");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(
            failure.detail.contains("has not been revoked"),
            "{}",
            failure.detail
        );
    }

    #[tokio::test]
    async fn signing_out_of_a_mac_that_was_never_signed_in_succeeds() {
        // 44 is `errSecItemNotFound`, and nothing to remove is a clean
        // sign-out. Pinned beside the case above so tightening one does not
        // quietly break the other.
        let absent = "The specified item could not be found in the keychain.";
        let runner = Arc::new(
            FakeRunner::new()
                .with("security delete-generic-password", 44, "", absent)
                .with("security find-generic-password", 44, "", absent),
        );
        let keychain = SecurityCliKeychain::new(runner);
        keychain
            .delete()
            .await
            .expect("nothing to remove is a clean sign-out");
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
        let runner: Arc<dyn CommandRunner> = Arc::new(riabuild_runner::RealRunner);
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

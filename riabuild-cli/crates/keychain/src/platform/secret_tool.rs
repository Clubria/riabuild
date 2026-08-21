//! Linux: libsecret through `secret-tool`.
//!
//! The token travels on **stdin**, never as an argument — argv is
//! world-readable through `ps`, and `secret-tool store` documents stdin and
//! honours it. Its other rule is that a diagnostic on stderr, not the exit
//! status, is what separates "this item is not here" from "the keyring could
//! not be consulted": `secret-tool` exits 1 for both, and reporting the second
//! as the first is what starts a device-code flow riabuild did not need. That
//! is `keyring_answers`'s measured rule applied to the real item.

use super::{ACCOUNT, SERVICE};
use crate::Keychain;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::Failure;
use std::sync::Arc;

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
        // Reachable only through `for_account` — a laptop caching a *server's*
        // session. `for_platform` and `for_password` both ask
        // `keyring_answers` first and pick a file store when the answer is no,
        // so neither can construct this type on a machine with no keyring.
        //
        // The advice no longer offers `RIABUILD_TOKEN` "from the riabuild
        // dashboard": the dashboard has never had a way to show a developer a
        // token, and `RIABUILD_TOKEN` is a CI and e2e hook (see `EnvKeychain`).
        // Naming it here sent developers looking for a screen that does not
        // exist. It would also be the wrong secret anyway — this store holds a
        // server's session, and `RIABUILD_TOKEN` is this machine's.
        Err(Failure::new(
            "reading a server's riabuild session from your keyring",
            "Install libsecret (`sudo apt install libsecret-tools`) and run `riabuild remote` \
             again.",
        )
        .detail("`secret-tool` is not installed, so riabuild has nowhere to cache the session")
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
            // [`keyring_answers`]'s own rule, applied to the real item and for
            // the same measured reason: `secret-tool` exits 1 on a miss *and*
            // on a service that never answered, and only stderr tells them
            // apart. A diagnostic here means the keyring could not be
            // consulted, which is not the same fact as "this developer has no
            // token" — and reporting the second is what starts a device-code
            // flow riabuild did not need, on a machine whose token is sitting
            // in a collection that was locked a moment ago.
            //
            // It fails in the same safe direction as the probe. A future
            // libsecret that printed to stderr on an ordinary miss would make
            // riabuild report a keyring it could not read, which the developer
            // can act on, rather than silently sign them in again.
            let diagnostic = output.stderr.trim();
            if diagnostic.is_empty() {
                return Ok(None);
            }
            return Err(Failure::new(
                "reading the riabuild token from your keyring",
                "Unlock your login keyring and run `riabuild` again.",
            )
            .command(format!(
                "secret-tool lookup service {SERVICE} account {}",
                self.account
            ))
            .detail(format!("`secret-tool lookup` failed: {diagnostic}"))
            .into());
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
            return Ok(());
        }
        // A `Failure`, not a bare `anyhow!`. This store is only ever chosen
        // when `keyring_answers` said a Secret Service replies, so reaching
        // here means one was there and refused — a locked keyring is the
        // ordinary way that happens, and it is the developer's to unlock. The
        // bare error this replaces was rendered under "it is a bug in
        // riabuild — send this to your team lead", which sent developers to
        // ask a colleague about a machine only they could fix.
        Err(Failure::new(
            "saving the riabuild token to your keyring",
            "Unlock your login keyring and run `riabuild` again.",
        )
        .detail(format!(
            "`secret-tool store` failed: {}",
            output.stderr.trim()
        ))
        .into())
    }

    async fn delete(&self) -> Result<()> {
        // Nothing to remove if there is no keyring; signing out succeeds.
        if self.runner.which("secret-tool").is_none() {
            return Ok(());
        }
        let output = self
            .runner
            .run(
                "secret-tool",
                &["clear", "service", SERVICE, "account", &self.account],
                &RunOptions::default(),
            )
            .await?;
        // Discarded here too, and it is the same revocation lying: `clear`
        // fails on a locked collection and `riabuild remote forget` said the
        // password was gone.
        //
        // The diagnostic is what decides, not the status, for the reason `get`
        // above gives — with one difference worth stating, since `clear` has
        // no "no such item" status of its own to lean on: a non-zero exit with
        // nothing on stderr is left to the read-back below, which is the only
        // thing that can actually answer whether the credential is still
        // there.
        let diagnostic = output.stderr.trim();
        if !output.ok() && !diagnostic.is_empty() {
            return Err(Failure::new(
                "removing the riabuild secret from your keyring",
                "Unlock your login keyring and run the command again.",
            )
            .command(format!(
                "secret-tool clear service {SERVICE} account {}",
                self.account
            ))
            .detail(format!("`secret-tool clear` failed: {diagnostic}"))
            .into());
        }
        if self.get().await?.is_some() {
            return Err(Failure::new(
                "removing the riabuild secret from your keyring",
                format!(
                    "Run `secret-tool clear service {SERVICE} account {}` until \
                     `secret-tool lookup` on the same attributes prints nothing.",
                    self.account
                ),
            )
            .detail(
                "riabuild cleared it and read one straight back, so the credential is \
                 still in the keyring and has not been revoked",
            )
            .into());
        }
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "your system keyring"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    #[tokio::test]
    async fn a_machine_with_no_keyring_gets_a_next_action_not_a_bug_report() {
        // Only `for_account` can construct this type on a keyring-less machine
        // now, so the message is about caching a *server's* session and no
        // longer offers `RIABUILD_TOKEN` — which was advice to go and find a
        // screen the dashboard has never had, and would have been the wrong
        // secret regardless. It still has to be an actionable `Failure`
        // rather than a raw error under "it is a bug in riabuild".
        let keychain = SecretToolKeychain::new(Arc::new(FakeRunner::new()));
        let error = keychain.get().await.unwrap_err();
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.action.contains("libsecret"), "{}", failure.action);
        assert!(
            !failure.action.contains("RIABUILD_TOKEN"),
            "the dashboard has no token to copy: {}",
            failure.action
        );
    }

    #[tokio::test]
    async fn a_keyring_that_refuses_a_store_is_not_reported_as_a_riabuild_bug() {
        // This store is only chosen when `keyring_answers` said yes, so a
        // failing `store` means a Secret Service was there and refused —
        // a locked keyring being the ordinary way. The developer can fix that;
        // their team lead cannot.
        let runner = Arc::new(FakeRunner::new().with(
            "secret-tool store",
            1,
            "",
            "secret-tool: Cannot create item: The collection is locked",
        ));
        let keychain = SecretToolKeychain::new(runner);
        let error = keychain.set("rb_live_token").await.expect_err("locked");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.action.contains("Unlock"), "{}", failure.action);
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

    #[tokio::test]
    async fn a_keyring_that_cannot_be_read_is_not_reported_as_no_token() {
        // I031, the Linux half. The probe's measured rule applied to the real
        // item: `secret-tool` exits 1 both on a miss and on a service that
        // never answered, and only stderr separates them.
        let runner = Arc::new(FakeRunner::new().with(
            "secret-tool lookup",
            1,
            "",
            "secret-tool: Cannot get secret of a locked object",
        ));
        let keychain = SecretToolKeychain::new(runner);
        let error = keychain
            .get()
            .await
            .expect_err("a locked collection is not an empty one");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.action.contains("Unlock"), "{}", failure.action);
    }

    #[tokio::test]
    async fn a_secret_tool_miss_is_still_none() {
        // The other side of the rule above, and the one that must not regress
        // into an error: a healthy keyring holding no token yet is every
        // developer's first run.
        let runner = Arc::new(FakeRunner::new().with("secret-tool lookup", 1, "", ""));
        let keychain = SecretToolKeychain::new(runner);
        assert_eq!(keychain.get().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_keyring_that_refused_the_clear_is_not_reported_as_revoked() {
        // I030, the Linux half.
        let runner = Arc::new(FakeRunner::new().with(
            "secret-tool clear",
            1,
            "",
            "secret-tool: Cannot delete item: The collection is locked",
        ));
        let keychain = SecretToolKeychain::new(runner);
        let error = keychain
            .delete()
            .await
            .expect_err("a refused clear is not a revocation");
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.action.contains("Unlock"), "{}", failure.action);
    }

    #[tokio::test]
    async fn a_keyring_item_that_survives_the_clear_is_reported() {
        let runner = Arc::new(FakeRunner::new().with("secret-tool clear", 0, "", "").with(
            "secret-tool lookup",
            0,
            "rb_still_here\n",
            "",
        ));
        let keychain = SecretToolKeychain::new(runner);
        let error = keychain
            .delete()
            .await
            .expect_err("a credential still in the keyring has not been revoked");
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
    async fn a_clear_that_took_reports_success() {
        let runner = Arc::new(FakeRunner::new().with("secret-tool clear", 0, "", "").with(
            "secret-tool lookup",
            1,
            "",
            "",
        ));
        let keychain = SecretToolKeychain::new(runner);
        keychain.delete().await.expect("the item is gone");
    }
}

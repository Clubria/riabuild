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
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::Failure;
use std::sync::Arc;

const SERVICE: &str = "com.clubria.riabuild";
const ACCOUNT: &str = "session-token";

/// The account [`keyring_answers`] looks up. Deliberately one nothing ever
/// stores, so the probe's answer is "does the service reply" and never "is
/// this developer signed in" — a machine with a perfectly good keyring and no
/// token yet must still be recognised as having one.
const PROBE_ACCOUNT: &str = "keyring-probe";

/// Whether this machine has a Secret Service that actually answers.
///
/// This is the question `runner.which("secret-tool").is_some()` was standing
/// in for, and getting wrong. libsecret is a **client** for a D-Bus Secret
/// Service; `secret-tool` being on `PATH` says nothing about whether anything
/// is listening. `libsecret-tools` arrives as a transitive dependency of
/// plenty of packages, so the binary is present on servers that have no
/// session bus at all — and riabuild would then pick the keyring, run the
/// whole device-code flow, and only discover it had nowhere to put the token
/// *after* the developer had approved the machine in a browser.
///
/// A `lookup` is the probe because it is read-only: it cannot create, unlock,
/// or overwrite anything. Its exit status alone cannot answer the question —
/// a miss and a dead service both exit non-zero — but **stderr** can, and this
/// was measured rather than assumed, against a real Secret Service on the bus
/// and against both ways of not having one:
///
/// | machine | exit | stderr |
/// |---|---|---|
/// | service on the bus, item absent | 1 | *empty* |
/// | no session bus at all | 1 | `Cannot autolaunch D-Bus without X11 $DISPLAY` |
/// | bus, but no keyring daemon | 1 | `The name org.freedesktop.secrets was not provided…` |
///
/// So: a diagnostic on stderr means the call did not complete, and the rule
/// below reads that rather than matching any of those messages as text, which
/// would be one libsecret release or one non-English locale from breaking.
///
/// It fails in the safe direction. If some future libsecret did print to
/// stderr on an ordinary miss, riabuild would keep the token in a 0600 file
/// instead of the keyring — a visible downgrade rather than a broken machine,
/// and `provision.rs` prints `describe()`, so the developer is told where the
/// token went.
pub(crate) async fn keyring_answers(runner: &dyn CommandRunner) -> bool {
    if runner.which("secret-tool").is_none() {
        return false;
    }
    let Ok(output) = runner
        .run(
            "secret-tool",
            &["lookup", "service", SERVICE, "account", PROBE_ACCOUNT],
            &RunOptions::default(),
        )
        .await
    else {
        return false;
    };
    output.ok() || output.stderr.trim().is_empty()
}

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
        "your macOS Keychain"
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
        "your system keyring"
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

    // `keyring_answers` — the probe that replaced `which("secret-tool")`.
    //
    // Every row below is a state observed against a real `secret-tool`: a
    // mock Secret Service on a private bus for the healthy miss, and a
    // headless box for the two failures. The table on `keyring_answers`
    // records the measurements; these pin the decision they imply.

    #[tokio::test]
    async fn a_healthy_keyring_with_no_token_yet_still_counts_as_a_keyring() {
        // The load-bearing one. A real Secret Service answering "I do not hold
        // that item" exits non-zero and prints *nothing*, so exit status alone
        // cannot tell this apart from a dead service — and if this were read as
        // "no keyring", every fresh laptop would quietly get a file instead of
        // the keychain.
        let runner = FakeRunner::new().with("secret-tool lookup", 1, "", "");
        assert!(keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn an_existing_item_counts_as_a_keyring() {
        let runner = FakeRunner::new().with("secret-tool lookup", 0, "rb_token\n", "");
        assert!(keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn a_box_with_no_dbus_session_has_no_keyring() {
        // The reported failure, verbatim. `secret-tool` is installed here —
        // which is exactly why `which` was the wrong question.
        let runner = FakeRunner::new().with(
            "secret-tool lookup",
            1,
            "",
            "secret-tool: Cannot autolaunch D-Bus without X11 $DISPLAY",
        );
        assert!(runner.which("secret-tool").is_some(), "the binary is there");
        assert!(!keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn a_bus_with_no_keyring_daemon_has_no_keyring() {
        // The second way to have no Secret Service: a session bus exists and
        // nothing has claimed `org.freedesktop.secrets` on it. Common in
        // containers and on minimal installs.
        let runner = FakeRunner::new().with(
            "secret-tool lookup",
            1,
            "",
            "secret-tool: The name org.freedesktop.secrets was not provided by any .service files",
        );
        assert!(!keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn a_missing_secret_tool_has_no_keyring() {
        assert!(!keyring_answers(&FakeRunner::new()).await);
    }

    #[tokio::test]
    async fn the_probe_reads_an_account_nothing_ever_stores() {
        // The probe must answer "does the service reply", not "is this
        // developer signed in" — otherwise a working keyring holding no token
        // yet would be misread as no keyring at all, on precisely the first
        // run that needs to store one. It must also never look at, let alone
        // disturb, the real session item.
        let runner = FakeRunner::new().with("secret-tool lookup", 1, "", "");
        keyring_answers(&runner).await;
        let calls = runner.calls();
        assert!(
            calls.iter().any(|call| call.contains(PROBE_ACCOUNT)),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.contains(ACCOUNT)),
            "the probe must not read the real session item: {calls:?}"
        );
        assert!(
            calls.iter().all(|call| !call.contains("store")
                && !call.contains("clear")
                && !call.contains("--unlock")),
            "the probe must be read-only: {calls:?}"
        );
    }

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

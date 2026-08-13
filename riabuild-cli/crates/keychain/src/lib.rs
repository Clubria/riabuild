//! Secret storage for the one secret riabuild keeps: its own session token.
//!
//! Never `~/.riabuild/`. A token on disk outlives the machine it was meant for —
//! it ends up in backups, in synced folders, and in tarballs sent to support.
//!
//! This file holds the trait every store implements and the decision of which
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

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct. The `feature = "testing"` half matters as much as the `test` half:
// when a downstream crate turns the feature on, this crate is compiled as a
// dependency and `cfg(test)` is false, so the exemption would not apply.
#![cfg_attr(any(test, feature = "testing"), allow(clippy::unwrap_used))]

mod file;
mod platform;

pub(crate) use file::FileKeychain;
pub(crate) use platform::{SecretToolKeychain, SecurityCliKeychain};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use riabuild_runner::CommandRunner;
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
///
/// A keyring-less laptop falls back to a file, on the same terms as
/// [`select_password_store`] — and this is the third of the three call sites
/// that had to stop asking `which("secret-tool")`. It errored instead of
/// falling back, which made `riabuild remote` unusable from any laptop without
/// libsecret: `e2e/remote/run.sh` carried it as a documented "known gap" that
/// stopped its CI run one stage earlier than a developer machine.
///
/// Storing it is also the more conservative option, not the looser one. The
/// alternative is not "no token on disk" — it is a *new 90-day session minted
/// on every run*, each one recorded nowhere this laptop can later revoke,
/// which is precisely what `session::ensure` warns about when it refuses to
/// let a server sign itself in.
pub async fn for_account(
    runner: Arc<dyn CommandRunner>,
    account: &str,
    keyringless_fallback: PathBuf,
) -> Box<dyn Keychain> {
    for_account_on(
        cfg!(target_os = "macos"),
        runner,
        account,
        keyringless_fallback,
    )
    .await
}

/// `for_account` with the platform question as a parameter.
///
/// This is `paths::default_project_dir_on`'s shape, and it is here for the
/// reason [`select`] is: a `cfg!(target_os = "macos")` inside the function
/// compiles every branch but the host's out of the test binary, so a test can
/// only ever assert what its own machine does. Pulling `select` out was half
/// the job — the three wrappers around it still asked `cfg!` themselves, which
/// meant every test that went *through* a wrapper silently asserted "and the
/// host is Linux".
///
/// That is not a hypothetical. The tests below did exactly that, passed the
/// `ubuntu-latest` pull-request gate for the whole life of the keyring-less
/// fallback, and then failed six-at-once on the release workflow's macOS job —
/// which is the only macOS runner this repository has, and runs after the tag
/// is pushed. A test that cannot fail until release is not a gate.
async fn for_account_on(
    is_macos: bool,
    runner: Arc<dyn CommandRunner>,
    account: &str,
    keyringless_fallback: PathBuf,
) -> Box<dyn Keychain> {
    let answers = platform::keyring_answers(runner.as_ref()).await;
    match select_password_store(is_macos, answers, keyringless_fallback) {
        Choice::Macos => Box::new(SecurityCliKeychain::for_account(runner, account)),
        Choice::Linux => Box::new(SecretToolKeychain::for_account(runner, account)),
        Choice::LinuxFile(path) => Box::new(FileKeychain::keyringless_machine(path)),
        // `select_password_store` returns neither.
        Choice::Env => Box::new(EnvKeychain),
        Choice::ServerFile(path) => Box::new(FileKeychain::server_namespace(path)),
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
        "the RIABUILD_TOKEN environment variable"
    }
}

#[cfg(any(test, feature = "testing"))]
#[derive(Default)]
pub struct MemoryKeychain {
    token: std::sync::Mutex<Option<String>>,
}

#[cfg(any(test, feature = "testing"))]
impl MemoryKeychain {
    pub fn with_token(token: &str) -> Self {
        Self {
            token: std::sync::Mutex::new(Some(token.to_string())),
        }
    }
}

#[cfg(any(test, feature = "testing"))]
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

/// The outcome of [`select`] — which store, and (for a file store) where.
#[derive(Debug, PartialEq, Eq)]
enum Choice {
    Env,
    /// A managed server's own session, in its namespace.
    ServerFile(PathBuf),
    Macos,
    Linux,
    /// A Linux machine with no Secret Service answering — see [`select`].
    LinuxFile(PathBuf),
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
///
/// `keyring_answers` is the last question, and only Linux asks it. A headless
/// Linux box — a build server, a container, a minimal install — has no D-Bus
/// session bus and so no Secret Service, and until this existed riabuild had
/// nowhere at all to put the token there: it signed the developer in, threw the
/// token away, and reported its own bug. So that machine gets a 0600 file, the
/// same `FileKeychain` a managed server has always used, for the same reason
/// `select_password_store` already falls back — the alternative is not "no
/// token on disk", it is "riabuild does not run here".
///
/// macOS is not asked. `security(1)` and a login keychain are always present,
/// and the one macOS case with no unlockable keychain — a server reached over
/// SSH — is already caught by the `session_token_file` branch above it.
fn select(
    is_macos: bool,
    token_env: Option<&str>,
    session_token_file: Option<PathBuf>,
    keyring_answers: bool,
    keyringless_fallback: PathBuf,
) -> Choice {
    if token_env.is_some_and(|value| !value.is_empty()) {
        return Choice::Env;
    }
    if let Some(path) = session_token_file {
        return Choice::ServerFile(path);
    }
    if is_macos {
        return Choice::Macos;
    }
    if keyring_answers {
        return Choice::Linux;
    }
    Choice::LinuxFile(keyringless_fallback)
}

/// Where a *server's SSH password* is kept, which is not quite the same
/// question as [`select`] answers for the session token.
///
/// Two differences, both deliberate:
///
/// - **`RIABUILD_TOKEN` is never consulted.** It holds this machine's riabuild
///   session, which has nothing to do with a Unix account's password on some
///   server. Honouring it here would hand `ssh` a bearer token as a password.
/// - **A machine with no keyring falls back to a file** rather than to a store
///   that always fails. On Linux with no Secret Service answering — a container, a CI
///   runner, a minimal distro — the alternative is not "no password on disk",
///   it is riabuild asking for the password again at every one of the ten SSH
///   connections a single `riabuild remote` opens. `~/.riabuild/ssh/passwords/`
///   is created at 0700 and the file written at 0600; see the amended
///   "No secrets in `~/.riabuild/`" note in `riabuild-cli/CLAUDE.md`.
///
/// macOS is not given the fallback: `security(1)` and a login keychain are
/// always there, and `riabuild remote` runs on a laptop, where that keychain is
/// unlocked. The server case that forces `for_platform`'s file store does not
/// arise — a server never runs `riabuild remote`.
fn select_password_store(is_macos: bool, keyring_answers: bool, fallback: PathBuf) -> Choice {
    if is_macos {
        return Choice::Macos;
    }
    if keyring_answers {
        return Choice::Linux;
    }
    Choice::LinuxFile(fallback)
}

/// The store a saved SSH password for one server goes in. See
/// [`select_password_store`] for why this is not [`for_account`].
pub async fn for_password(
    runner: Arc<dyn CommandRunner>,
    account: &str,
    fallback: PathBuf,
) -> Box<dyn Keychain> {
    for_password_on(cfg!(target_os = "macos"), runner, account, fallback).await
}

/// `for_password` with the platform question as a parameter — see
/// [`for_account_on`] for why every wrapper in this file has one.
async fn for_password_on(
    is_macos: bool,
    runner: Arc<dyn CommandRunner>,
    account: &str,
    fallback: PathBuf,
) -> Box<dyn Keychain> {
    // `keyring_answers`, not `which("secret-tool")`. This call site had the
    // same bug as `for_platform`: a laptop with the binary and no Secret
    // Service passed the old test, took the keyring branch, and failed at
    // every one of the ten SSH connections a single `riabuild remote` opens.
    let answers = platform::keyring_answers(runner.as_ref()).await;
    match select_password_store(is_macos, answers, fallback) {
        Choice::LinuxFile(path) => Box::new(FileKeychain::keyringless_machine(path)),
        Choice::Macos => Box::new(SecurityCliKeychain::for_account(runner, account)),
        Choice::Linux => Box::new(SecretToolKeychain::for_account(runner, account)),
        // `select_password_store` returns neither — the session-token
        // override and a server namespace both have no meaning for a
        // server's SSH password.
        Choice::Env => Box::new(EnvKeychain),
        Choice::ServerFile(path) => Box::new(FileKeychain::server_namespace(path)),
    }
}

/// Picks the right store for this machine. See [`select`] for the ordering
/// this delegates to.
///
/// Async because deciding now costs a `secret-tool lookup`: whether a keyring
/// is *usable* cannot be answered by stat-ing `PATH`. Paying it here, once, at
/// startup is the point — the choice has to be made before `login` runs, so a
/// machine with nowhere to keep a token never reaches a browser approval whose
/// result it would then discard.
pub async fn for_platform(
    runner: Arc<dyn CommandRunner>,
    session_token_file: Option<PathBuf>,
    keyringless_fallback: PathBuf,
) -> Box<dyn Keychain> {
    for_platform_on(
        cfg!(target_os = "macos"),
        runner,
        session_token_file,
        keyringless_fallback,
    )
    .await
}

/// `for_platform` with the platform question as a parameter — see
/// [`for_account_on`] for why every wrapper in this file has one.
///
/// `is_macos` governs the probe as well as the choice. The two must move
/// together: a version of this that took the parameter for `select` and left
/// `cfg!` in `needs_probe` would still be a function whose behaviour on a
/// macOS host differs from what any test on it can describe.
async fn for_platform_on(
    is_macos: bool,
    runner: Arc<dyn CommandRunner>,
    session_token_file: Option<PathBuf>,
    keyringless_fallback: PathBuf,
) -> Box<dyn Keychain> {
    let token_env = std::env::var("RIABUILD_TOKEN").ok();
    // Only Linux can answer "no", and only when neither branch above the
    // keyring question already applies — so the probe is skipped on macOS,
    // under `RIABUILD_TOKEN`, and on a managed server, none of which would
    // use the answer.
    let needs_probe = !is_macos
        && session_token_file.is_none()
        && !token_env.as_deref().is_some_and(|value| !value.is_empty());
    let answers = if needs_probe {
        platform::keyring_answers(runner.as_ref()).await
    } else {
        false
    };
    match select(
        is_macos,
        token_env.as_deref(),
        session_token_file,
        answers,
        keyringless_fallback,
    ) {
        Choice::Env => Box::new(EnvKeychain),
        Choice::ServerFile(path) => Box::new(FileKeychain::server_namespace(path)),
        Choice::LinuxFile(path) => Box::new(FileKeychain::keyringless_machine(path)),
        Choice::Macos => Box::new(SecurityCliKeychain::new(runner)),
        Choice::Linux => Box::new(SecretToolKeychain::new(runner)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    #[test]
    fn a_password_prefers_the_keyring_and_falls_back_to_a_file() {
        let fallback = PathBuf::from("/home/ada/.riabuild/ssh/passwords/9f2c");
        // macOS always has `security` and an unlocked login keychain on the
        // laptop `riabuild remote` runs from, so the fallback never applies —
        // asserted with `has_secret_tool` false, so a branch that reached the
        // file store on macOS could not hide behind a machine that has both.
        assert_eq!(
            select_password_store(true, false, fallback.clone()),
            Choice::Macos
        );
        assert_eq!(
            select_password_store(false, true, fallback.clone()),
            Choice::Linux
        );
        assert_eq!(
            select_password_store(false, false, fallback.clone()),
            Choice::LinuxFile(fallback)
        );
    }

    #[test]
    fn a_password_never_comes_from_riabuild_token() {
        // `RIABUILD_TOKEN` is this machine's riabuild session. Reading it here
        // would hand `ssh` a bearer token where a Unix password belongs — and
        // `select` *does* return `Env` for it, so this is a real difference
        // between the two decisions rather than a restatement.
        let fallback = PathBuf::from("/tmp/pw");
        assert_eq!(
            select(false, Some("rb_live_token"), None, true, fallback.clone()),
            Choice::Env
        );
        for (is_macos, keyring_answers) in [(true, true), (false, true), (false, false)] {
            assert_ne!(
                select_password_store(is_macos, keyring_answers, fallback.clone()),
                Choice::Env
            );
        }
    }

    #[tokio::test]
    async fn a_keyringless_laptop_can_still_cache_a_servers_session() {
        // `e2e/remote/run.sh` carried this as "known gap (a)" — `riabuild
        // remote` was unusable from any laptop without libsecret, because
        // `for_account` errored instead of falling back.
        //
        // Storing it is the conservative option. Without it the laptop mints a
        // fresh 90-day session on *every* run, and records none of them
        // anywhere it could later revoke — which is the exact outcome
        // `session::ensure` refuses to allow a server to cause.
        let fallback = PathBuf::from("/home/ada/.riabuild/remote-sessions/9f2c");
        let store = for_account_on(false, runner_with_no_dbus(), "remote:9f2c", fallback).await;
        assert_eq!(
            store.describe(),
            "a private file, because this machine has no keyring"
        );
    }

    #[tokio::test]
    async fn a_mac_laptop_caches_a_servers_session_in_the_keychain_regardless() {
        // The same machine as the test above — `secret-tool` installed, no
        // session bus — except that it is a Mac, where the probe's answer is
        // not the question: `security(1)` and an unlocked login keychain are
        // always there on the laptop `riabuild remote` runs from.
        let fallback = PathBuf::from("/Users/ada/.riabuild/remote-sessions/9f2c");
        let store = for_account_on(true, runner_with_no_dbus(), "remote:9f2c", fallback).await;
        assert_eq!(store.describe(), "your macOS Keychain");
    }

    #[tokio::test]
    async fn a_laptop_with_a_keyring_still_caches_a_servers_session_in_it() {
        let fallback = PathBuf::from("/home/ada/.riabuild/remote-sessions/9f2c");
        let store = for_account_on(
            false,
            runner_with_a_working_keyring(),
            "remote:9f2c",
            fallback,
        )
        .await;
        assert_eq!(store.describe(), "your system keyring");
    }

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

    /// A laptop whose keyring answers: `secret-tool` is on `PATH` and a
    /// `lookup` for the probe account misses *quietly* — exit 1, nothing on
    /// stderr, which is what a real Secret Service does for an item it does
    /// not hold.
    fn runner_with_a_working_keyring() -> Arc<dyn CommandRunner> {
        Arc::new(FakeRunner::new().with("secret-tool lookup", 1, "", ""))
    }

    /// A headless server: the binary is installed — `libsecret-tools` rides in
    /// as a transitive dependency all over the place — and there is no session
    /// bus for it to talk to. This is the exact stderr a developer reported.
    fn runner_with_no_dbus() -> Arc<dyn CommandRunner> {
        Arc::new(FakeRunner::new().with(
            "secret-tool lookup",
            1,
            "",
            "secret-tool: Cannot autolaunch D-Bus without X11 $DISPLAY",
        ))
    }

    fn laptop_fallback() -> PathBuf {
        PathBuf::from("/home/ada/.riabuild/session.token")
    }

    #[tokio::test]
    async fn a_server_never_reaches_for_a_keyring() {
        // A macOS server is what makes this a rule rather than a preference:
        // `security` cannot open a login keychain an SSH session has not unlocked,
        // so asking the platform first would pick a store that always fails.
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let remote = for_platform_on(
            false,
            runner.clone(),
            Some(PathBuf::from("/home/dev/ns/session.token")),
            laptop_fallback(),
        )
        .await;
        assert_eq!(remote.describe(), "this server's riabuild namespace");
    }

    #[tokio::test]
    async fn a_macos_server_still_picks_the_file_store_over_the_keychain() {
        // Pins the *ordering* in `for_platform`, not just the outcome: when the
        // platform answer is macOS, the file store must still win with a
        // `session_token_file`, because a macOS server has no way to unlock its
        // login keychain over SSH.
        //
        // This used to be `#[cfg(target_os = "macos")]`, which meant the one
        // regression it exists to catch was only caught on the release
        // workflow's macOS runner — after the tag. Passing `is_macos` makes it
        // a test the pull-request gate runs.
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let remote = for_platform_on(
            true,
            runner,
            Some(PathBuf::from("/home/dev/ns/session.token")),
            laptop_fallback(),
        )
        .await;
        assert_eq!(remote.describe(), "this server's riabuild namespace");
    }

    #[tokio::test]
    async fn a_laptop_with_a_working_keyring_still_uses_it() {
        // The half of the old `a_laptop_..._never_selects_the_file_store` test
        // that survives the fallback: a machine with a keyring must still put
        // the token in it, and must never be described as a server.
        let laptop = for_platform_on(
            false,
            runner_with_a_working_keyring(),
            None,
            laptop_fallback(),
        )
        .await;
        assert_eq!(laptop.describe(), "your system keyring");
    }

    #[tokio::test]
    async fn a_headless_linux_server_gets_a_file_rather_than_a_dead_keyring() {
        // The regression test for the reported bug. `secret-tool` is installed,
        // so the old `which`-based test said "this machine has a keyring" and
        // riabuild picked a store whose every call fails. It then ran the whole
        // device-code flow and only discovered it had nowhere to put the token
        // *after* the developer approved the machine in a browser — surfacing a
        // raw `secret-tool:` stderr line under "it is a bug in riabuild".
        let server = for_platform_on(false, runner_with_no_dbus(), None, laptop_fallback()).await;
        assert_eq!(
            server.describe(),
            "a private file, because this machine has no keyring",
            "a machine whose keyring does not answer must not be handed the keyring store"
        );
        // And it must say where the token really went — never borrow the
        // server-namespace wording, which is the shape of the earlier bug
        // `scope.rs` documents.
        assert_ne!(server.describe(), "this server's riabuild namespace");
    }

    #[tokio::test]
    async fn a_machine_with_no_secret_tool_at_all_also_gets_a_file() {
        // The other way to have no keyring, and the one the old code did
        // detect — it just had nothing to offer afterwards.
        let bare: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let store = for_platform_on(false, bare, None, laptop_fallback()).await;
        assert_eq!(
            store.describe(),
            "a private file, because this machine has no keyring"
        );
    }

    #[tokio::test]
    async fn riabuild_token_still_wins_on_a_keyringless_machine() {
        // CI and the e2e suite set it, and they are exactly the machines with
        // no keyring — so the new fallback must not get in front of it.
        // Guarded on the variable being unset, because this crate's tests
        // share one process and `select` is what the ordering is really
        // pinned on (see `select_prefers_env_...` below).
        if std::env::var("RIABUILD_TOKEN").is_ok() {
            return;
        }
        assert_eq!(
            select(false, Some("rb_live_token"), None, false, laptop_fallback()),
            Choice::Env
        );
    }

    /// Guards the wiring between each public function and the tested one —
    /// `paths`' `the_default_matches_the_platform_it_is_running_on`, once per
    /// wrapper.
    ///
    /// Without this, taking the platform out as a parameter would *move* the
    /// untested branch rather than remove it: three `cfg!(target_os =
    /// "macos")` call sites that no test names, where a wrapper passing a
    /// hardcoded `false` would send every Mac to `secret-tool` and pass the
    /// whole suite on both hosts.
    #[tokio::test]
    async fn each_wrapper_passes_the_platform_it_is_actually_running_on() {
        let is_macos = cfg!(target_os = "macos");
        let runner = runner_with_a_working_keyring;

        assert_eq!(
            for_platform(runner(), None, laptop_fallback())
                .await
                .describe(),
            for_platform_on(is_macos, runner(), None, laptop_fallback())
                .await
                .describe(),
        );
        assert_eq!(
            for_password(runner(), "remote-password:9f2c", laptop_fallback())
                .await
                .describe(),
            for_password_on(
                is_macos,
                runner(),
                "remote-password:9f2c",
                laptop_fallback()
            )
            .await
            .describe(),
        );
        assert_eq!(
            for_account(runner(), "remote:9f2c", laptop_fallback())
                .await
                .describe(),
            for_account_on(is_macos, runner(), "remote:9f2c", laptop_fallback())
                .await
                .describe(),
        );
    }

    // The tests above now pass `is_macos` explicitly, so both platforms'
    // outcomes are asserted on every host — which they were not when each
    // wrapper asked `cfg!` itself, and PR CI only runs `ubuntu-latest` (the
    // sole macOS runner is `release.yml`'s tag-triggered job, not the
    // pull_request gate). The tests below call `select` directly, which is
    // still the tightest place to pin the *ordering* of the branches: no
    // runner, no store construction, just the decision.

    #[test]
    fn select_prefers_the_file_store_over_macos_even_when_is_macos_is_true() {
        // This is the test that would fail if `select`'s remote-check and
        // platform-check branches were swapped: with `is_macos: true` and a
        // `session_token_file`, swapped code would return `Choice::Macos`
        // instead. Confirmed by temporarily swapping the branches locally —
        // this test fails with:
        //   assertion `left == right` failed
        //     left: Macos
        //    right: ServerFile("/home/dev/ns/session.token")
        let path = PathBuf::from("/home/dev/ns/session.token");
        assert_eq!(
            select(true, None, Some(path.clone()), true, laptop_fallback()),
            Choice::ServerFile(path)
        );
    }

    #[test]
    fn select_prefers_env_over_the_file_store_and_over_macos() {
        let path = PathBuf::from("/home/dev/ns/session.token");
        assert_eq!(
            select(
                true,
                Some("rb_live_token"),
                Some(path),
                true,
                laptop_fallback()
            ),
            Choice::Env
        );
    }

    #[test]
    fn select_falls_back_to_the_platform_keyring_with_no_server_and_no_env() {
        assert_eq!(
            select(true, None, None, true, laptop_fallback()),
            Choice::Macos
        );
        assert_eq!(
            select(false, None, None, true, laptop_fallback()),
            Choice::Linux
        );
    }

    /// The whole fix, end to end, against the real `secret-tool` on whatever
    /// machine is running the suite — no `FakeRunner`, no canned stderr.
    ///
    /// This is the one test that would have caught the reported bug, and it can
    /// run on the gate: PR CI is `ubuntu-latest`, which has no Secret Service,
    /// which is exactly the machine class that was broken. Before the fix this
    /// selected `SecretToolKeychain` and `set` failed; now it must select a
    /// file store and round-trip a token through it.
    ///
    /// It **skips** on a machine whose keyring answers, and that is not
    /// laziness — the alternative is a test that writes a token into a
    /// developer's real login keyring while they run `cargo test`.
    ///
    /// `is_macos: false` is the same refusal, and is why this is not simply
    /// `cfg!(target_os = "macos")`. On a Mac the real answer would select
    /// `security(1)` and this test would put `rb_live_token` in the login
    /// keychain of whoever ran it. What it asserts instead — that the file
    /// store really round-trips a token at 0600 — is a live path on macOS
    /// too, because a macOS *server* uses that same `FileKeychain`. So the
    /// coverage this buys on the release workflow's macOS runner is real,
    /// not a Linux fixture wearing a Mac's clothes.
    #[tokio::test]
    async fn a_real_machine_with_no_secret_service_can_actually_keep_a_token() {
        let runner: Arc<dyn CommandRunner> = Arc::new(riabuild_runner::RealRunner);
        if platform::keyring_answers(runner.as_ref()).await {
            // This machine has a working keyring; the fallback is not its path
            // and probing further would mean writing to a real keychain.
            return;
        }
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join("riabuild").join("session.token");

        let store = for_platform_on(false, runner, None, path.clone()).await;
        assert_eq!(
            store.describe(),
            "a private file, because this machine has no keyring",
            "a machine with no Secret Service must not be handed the keyring store"
        );

        assert_eq!(store.get().await.expect("read"), None);
        store.set("rb_live_token").await.expect(
            "storing a token must succeed here — failing this is the reported bug, \
             where riabuild signed the developer in and then had nowhere to put the token",
        );
        assert_eq!(
            store.get().await.expect("read"),
            Some("rb_live_token".to_string()),
            "and a later run must find it, rather than minting a new session every time"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(&path)
                .await
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the token must not be world-readable");
        }

        store.delete().await.expect("sign out");
        assert_eq!(store.get().await.expect("read"), None);
    }

    #[test]
    fn only_linux_is_ever_asked_whether_its_keyring_answers() {
        // macOS must not acquire a file fallback by the back door: `security`
        // and a login keychain are always there, and the one Mac that cannot
        // unlock one — a server over SSH — is caught by the branch above.
        // Asserted with `keyring_answers: false`, the input that would flip a
        // wrongly-ordered implementation.
        assert_eq!(
            select(true, None, None, false, laptop_fallback()),
            Choice::Macos
        );
        assert_eq!(
            select(false, None, None, false, laptop_fallback()),
            Choice::LinuxFile(laptop_fallback())
        );
    }

    #[test]
    fn a_server_namespace_still_beats_a_keyring_that_does_not_answer() {
        // Both branches now want a file, so the risk is not "no store" but the
        // *wrong* file — a managed server's token landing in the keyring-less
        // laptop path, which is not where `riabuild remote forget` looks.
        let path = PathBuf::from("/home/dev/ns/session.token");
        assert_eq!(
            select(false, None, Some(path.clone()), false, laptop_fallback()),
            Choice::ServerFile(path)
        );
    }
}

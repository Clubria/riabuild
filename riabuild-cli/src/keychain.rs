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
use std::path::{Path, PathBuf};
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

/// A server's own session, in the developer's namespace at 0600.
///
/// The one exception to "no secrets in ~/.riabuild", argued in the remote mode
/// design: a server has no keyring, the token is minted for that server alone,
/// it is labelled and listed in the dashboard, and `riabuild remote forget`
/// revokes it. What the invariant exists to protect — the Infisical credential —
/// is still brokered per use and still never written down.
pub struct FileKeychain {
    path: PathBuf,
}

impl FileKeychain {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl Keychain for FileKeychain {
    async fn get(&self) -> Result<Option<String>> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => {
                let token = text.trim().to_string();
                Ok((!token.is_empty()).then_some(token))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn set(&self, token: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            ensure_private_dir(parent).await?;
        }
        write_private_token(&self.path, token).await
    }

    async fn delete(&self) -> Result<()> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn describe(&self) -> &'static str {
        "this server's riabuild namespace"
    }
}

/// The namespace directory the session file lives in, `0700`.
///
/// `create_dir_all` (or `DirBuilder` with `recursive(true)`) does not re-apply
/// `mode` to a directory that already exists, so a namespace directory left
/// world-readable by something else — an earlier riabuild version, an admin
/// script, an `umask` — would otherwise keep that mode forever. This is the
/// directory the one file the "no secrets in ~/.riabuild" invariant was amended
/// for lives in, so its mode is repaired unconditionally, not just set at
/// creation.
#[cfg(unix)]
async fn ensure_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .await
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn ensure_private_dir(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    Ok(())
}

#[cfg(unix)]
async fn write_private_token(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await?;
    // `OpenOptions::mode` applies at creation only, and `truncate` does not reset
    // permissions — so on the repair path (this call found a file already on
    // disk, at some looser mode) the mode is still whatever it was until we
    // change it. That has to happen on the open file handle, before the write,
    // not after: setting it by path once the content is already written would
    // leave the fresh token briefly readable at the file's old mode, on exactly
    // the file the "no secrets in ~/.riabuild" invariant is being amended for.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .await?;
    file.write_all(contents.as_bytes()).await?;
    // tokio::fs::File::poll_write copies into an internal buffer and hands the
    // real write() off to a blocking-pool task, returning Ready before that
    // syscall has actually run. Without this flush, `write_all` completing is
    // not proof the token landed on disk — a caller that returns success right
    // after can race a reader against a write still in flight on another
    // thread. flush() blocks until the background write is done.
    file.flush().await?;
    Ok(())
}

#[cfg(not(unix))]
async fn write_private_token(path: &Path, contents: &str) -> Result<()> {
    tokio::fs::write(path, contents).await?;
    Ok(())
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
    use tempfile::TempDir;

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

    #[tokio::test]
    async fn a_server_keeps_its_session_in_a_file_it_owns() {
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        let keychain = FileKeychain::new(path.clone());

        assert_eq!(keychain.get().await.expect("read"), None);
        keychain.set("rb_live_token").await.expect("write");
        assert_eq!(
            keychain.get().await.expect("read"),
            Some("rb_live_token".to_string())
        );

        keychain.delete().await.expect("delete");
        assert_eq!(keychain.get().await.expect("read"), None);
        // Deleting what is already gone is not an error: `apply()` runs twice.
        keychain.delete().await.expect("delete again");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_session_file_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        FileKeychain::new(path.clone())
            .set("rb_live_token")
            .await
            .expect("write");

        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "session.token must be 0600");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_world_readable_session_file_is_repaired_on_write() {
        // Creating with mode 0600 does not fix a file that is already 0644 — the
        // token still works, it is just readable by every co-tenant on the
        // shared account. `set()` must repair the mode of a file that already
        // exists, not only of one it creates, and it must still write the new
        // token correctly while doing so.
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        std::fs::write(&path, "stale").expect("pre-create");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("loosen permissions");

        let keychain = FileKeychain::new(path.clone());
        keychain.set("rb_live_token").await.expect("write");

        assert_eq!(
            keychain.get().await.expect("read"),
            Some("rb_live_token".to_string()),
            "the repair path must still write the new token"
        );
        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "an existing 0644 file must be repaired"
        );
    }

    // NOTE ON WHAT THE TEST ABOVE DOES AND DOES NOT PROVE, for a reviewer
    // looking for a test that pins the *ordering* rather than the end state:
    //
    // It proves content and mode are both correct once `set()` returns. It
    // cannot prove there was never a moment in between where the new token sat
    // behind the file's old, looser mode — a black-box test can only observe
    // before-and-after, not the sequence of syscalls a single sequential async
    // task made in between. Making that window observable would need a second
    // task genuinely running in parallel with the write, and this crate
    // deliberately does not enable tokio's `rt-multi-thread` feature (see the
    // comment on the `tokio` dependency in Cargo.toml: "so a stray
    // `Runtime::new()` cannot quietly spawn a worker pool") — pulling that in
    // for one test, even a test-only one, would itself be inconsistent with
    // that constraint, and a race against real disk I/O timing would be
    // nondeterministic regardless: it could pass on a fast disk and fail on a
    // slow one, which is a worse property than an honest gap.
    //
    // So the ordering claim is not directly black-box testable here, and is
    // instead fixed by construction: `write_private_token` calls
    // `File::set_permissions` on the *already-open* handle and `.await`s it to
    // completion before calling `write_all` on that same handle, both in one
    // sequential task. There is no scheduling outcome under which the write
    // happens first — the two calls have a program-order happens-before
    // relationship, not merely a probable one. The bug this replaced had the
    // opposite shape (write the content, `drop` the handle, `chmod` the path
    // afterward), which is a real ordering a reviewer should keep checking for
    // by reading `write_private_token` itself, since no test can substitute for
    // that reading here.

    #[cfg(unix)]
    #[tokio::test]
    async fn a_world_readable_namespace_directory_is_repaired_on_write() {
        // The containing directory is part of the same security property: a
        // 0755 namespace directory lets anyone on the box list it and read the
        // session file inside, regardless of the file's own mode.
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let namespace = home.path().join("ns");
        std::fs::create_dir(&namespace).expect("pre-create namespace dir");
        std::fs::set_permissions(&namespace, std::fs::Permissions::from_mode(0o777))
            .expect("loosen permissions");

        FileKeychain::new(namespace.join("session.token"))
            .set("rb_live_token")
            .await
            .expect("write");

        let mode = tokio::fs::metadata(&namespace)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "an existing 0777 namespace dir must be repaired"
        );
    }

    #[tokio::test]
    async fn writing_the_token_twice_replaces_it() {
        let home = TempDir::new().expect("tempdir");
        let keychain = FileKeychain::new(home.path().join("session.token"));
        keychain.set("first").await.expect("write");
        keychain.set("second").await.expect("write");
        assert_eq!(
            keychain.get().await.expect("read"),
            Some("second".to_string())
        );
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

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
    file.write_all(contents.as_bytes()).await?;
    drop(file);
    // `OpenOptions::mode` applies at creation only, and `truncate` does not reset
    // permissions. A file left looser by an interrupted write would otherwise be
    // rewritten at its old mode — and this is the one file the "no secrets in
    // ~/.riabuild" invariant is being amended for.
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
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

/// Picks the right store for this machine.
///
/// Order matters. An explicit `RIABUILD_TOKEN` wins so automation can run with no
/// store at all. A server comes next, *before* any platform question: a macOS
/// server has `security(1)` and a login keychain an SSH session cannot unlock, so
/// asking the platform first would pick a store that always fails.
pub fn for_platform(
    runner: Arc<dyn CommandRunner>,
    session_token_file: Option<PathBuf>,
) -> Box<dyn Keychain> {
    if std::env::var("RIABUILD_TOKEN").is_ok_and(|value| !value.is_empty()) {
        return Box::new(EnvKeychain);
    }
    if let Some(path) = session_token_file {
        return Box::new(FileKeychain::new(path));
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
    use tempfile::TempDir;

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
        // exists, not only of one it creates.
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        std::fs::write(&path, "stale").expect("pre-create");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("loosen permissions");

        FileKeychain::new(path.clone())
            .set("rb_live_token")
            .await
            .expect("write");

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
}

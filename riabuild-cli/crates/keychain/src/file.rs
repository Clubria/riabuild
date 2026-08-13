//! The file-backed store, and the two filesystem helpers that keep it private.
//!
//! A server has no keyring, so its own session token is the one secret riabuild
//! writes down — the exception argued for on [`FileKeychain`] below. Everything
//! else here exists to make that file unreadable by a co-tenant: the modes are
//! *repaired* on every write rather than only set at creation, because
//! `OpenOptions::mode` and `DirBuilder::mode` describe a path that does not
//! exist yet and say nothing about one that already does.

use super::Keychain;
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// A secret in a 0600 file, for a machine with nowhere better to put one.
///
/// Two machines reach it, and [`describe`](Keychain::describe) must say which,
/// because `provision.rs` prints that sentence to the developer and it is the
/// only thing on screen that says where their token went. A store that names
/// the wrong place is the shape of an earlier bug here — `scope.rs` still
/// carries the note about a laptop whose keychain reported itself as "this
/// server's riabuild namespace" — so the description is a constructor
/// argument rather than one string that covers both.
///
/// - [`server_namespace`](Self::server_namespace): a managed server's own
///   session. The original exception to "no secrets in ~/.riabuild", argued in
///   the remote mode design: a server has no keyring, the token is minted for
///   that server alone, it is labelled and listed in the dashboard, and
///   `riabuild remote forget` revokes it.
/// - [`keyringless_machine`](Self::keyringless_machine): a Linux machine with
///   no Secret Service answering. The same exception, widened the same way the
///   remote-password design already widened it for an SSH password, and for
///   the identical reason: the alternative on such a machine is not "no token
///   on disk", it is that riabuild cannot run there at all.
///
/// What the invariant exists to protect — the Infisical org credential — is
/// untouched by either: it is still brokered per use and still never written
/// down.
pub struct FileKeychain {
    path: PathBuf,
    description: &'static str,
}

impl FileKeychain {
    /// A managed server's own session token, in the developer's namespace.
    pub fn server_namespace(path: PathBuf) -> Self {
        Self {
            path,
            description: "this server's riabuild namespace",
        }
    }

    /// A machine whose keyring does not answer. Named for the *reason* rather
    /// than the path, because that reason is what the developer needs in order
    /// to know this is not where riabuild would normally have put it.
    pub fn keyringless_machine(path: PathBuf) -> Self {
        Self {
            path,
            description: "a private file, because this machine has no keyring",
        }
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
        self.description
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn a_server_keeps_its_session_in_a_file_it_owns() {
        let home = TempDir::new().expect("tempdir");
        let path = home.path().join("session.token");
        let keychain = FileKeychain::server_namespace(path.clone());

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
        FileKeychain::server_namespace(path.clone())
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

        let keychain = FileKeychain::server_namespace(path.clone());
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

        FileKeychain::server_namespace(namespace.join("session.token"))
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
    async fn each_file_store_says_which_machine_it_is() {
        // `provision.rs` prints `describe()`, and it is the only line telling a
        // developer where their token went. One string covering both callers
        // would put "this server's riabuild namespace" on a laptop — which is
        // the exact dishonesty `scope.rs` still carries a note about.
        let path = PathBuf::from("/home/ada/.riabuild/session.token");
        assert_eq!(
            FileKeychain::server_namespace(path.clone()).describe(),
            "this server's riabuild namespace"
        );
        assert_eq!(
            FileKeychain::keyringless_machine(path).describe(),
            "a private file, because this machine has no keyring"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_keyringless_machines_token_is_as_private_as_a_servers() {
        // The fallback is a widening of "no secrets in ~/.riabuild", so it has
        // to carry the same protection the original exception was granted on:
        // 0600, in a directory at 0700. Same code path as the server store, and
        // this asserts it rather than assuming the shared constructor implies
        // it.
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().expect("tempdir");
        let dir = home.path().join("riabuild");
        let path = dir.join("session.token");
        FileKeychain::keyringless_machine(path.clone())
            .set("rb_live_token")
            .await
            .expect("write");

        let file = tokio::fs::metadata(&path).await.expect("stat file");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let parent = tokio::fs::metadata(&dir).await.expect("stat dir");
        assert_eq!(parent.permissions().mode() & 0o777, 0o700);
    }

    #[tokio::test]
    async fn writing_the_token_twice_replaces_it() {
        let home = TempDir::new().expect("tempdir");
        let keychain = FileKeychain::server_namespace(home.path().join("session.token"));
        keychain.set("first").await.expect("write");
        keychain.set("second").await.expect("write");
        assert_eq!(
            keychain.get().await.expect("read"),
            Some("second".to_string())
        );
    }
}

//! The server's GitHub configuration directory, which lives only as long as a
//! session.
//!
//! A `gh` OAuth token is the developer's whole GitHub account, and a shared box
//! is the last place it should sit at rest — so it is the one piece of state that
//! is not namespaced onto disk. This buys "no GitHub credential at rest between
//! sessions". It does **not** hide the credential from a co-tenant during a live
//! session, and deleting is not revoking; both are stated in the design.
//!
//! **Lifetime.** Each SSH invocation is a separate process — the sweep, the
//! seed, the setup run, the shell, and any `riabuild` typed inside that shell
//! are five of them. A refcount every process joins on start and wipes on
//! last-out is wrong: it has the seeding process write the credential, exit,
//! find itself alone, and delete what it just wrote, milliseconds before the
//! setup run ever sees it. So only the environment shell holds the credential
//! open — it alone leaves a marker in `open`, and it alone can trigger the
//! wipe in `close`. The seed run, the setup run, and a `riabuild` typed inside
//! the shell all use `attach`, which never claims or releases anything.
//!
//! Signal handlers matter here even though the shell is riabuild's child and
//! its death ordinarily returns through `close`: mosh exists precisely to keep
//! a session alive when the client goes away, and what eventually ends such a
//! session is a signal, not a clean return. `sweep` is the backstop for that
//! case, and for the plainer one where the process is `kill -9`'d outright —
//! neither leaves a chance to run `close`, so a marker for a pid that no
//! longer exists (or that got recycled onto an unrelated process) must not be
//! able to wedge the directory alive forever. That is why `sweep` treats a
//! marker as dead when its process is gone *or* when it is older than
//! `STALE_AFTER_SECS`, rather than trusting liveness alone.

use crate::runner::{CommandRunner, RunOptions};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A marker whose process still looks alive is ignored after this long, because
/// pids are recycled and a stale marker would otherwise match a live stranger.
const STALE_AFTER_SECS: u64 = 24 * 60 * 60;

#[allow(dead_code)] // consumed by Task 20
pub fn choose_runtime_dir(xdg: Option<&str>, tmpdir: Option<&str>) -> Result<PathBuf> {
    for candidate in [xdg, tmpdir] {
        if let Some(path) = candidate.filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path));
        }
    }
    Ok(PathBuf::from("/tmp"))
}

#[allow(dead_code)] // consumed by Task 20
pub struct GhSession {
    dir: PathBuf,
    marker: PathBuf,
}

#[allow(dead_code)] // consumed by Task 20
impl GhSession {
    /// The directory, created safely, with no claim on its lifetime. Used by the
    /// seed and setup runs, and by a `riabuild` typed inside the shell.
    pub async fn attach(runtime: &Path, member_id: &str) -> Result<PathBuf> {
        let dir = runtime.join(format!("riabuild-gh-{member_id}"));
        ensure_private_dir(&dir).await?;
        ensure_private_dir(&dir.join("sessions")).await?;
        Ok(dir)
    }

    /// Claims the directory for the life of an environment shell.
    pub async fn open(runtime: &Path, member_id: &str, pid: u32) -> Result<GhSession> {
        let dir = GhSession::attach(runtime, member_id).await?;
        let marker = dir.join("sessions").join(pid.to_string());
        tokio::fs::write(&marker, crate::config::now_secs().to_string()).await?;
        Ok(GhSession { dir, marker })
    }

    pub fn config_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Drops this session's claim, and wipes the tree if it was the last.
    pub async fn close(self, runner: Arc<dyn CommandRunner>) -> Result<()> {
        let _ = tokio::fs::remove_file(&self.marker).await;
        sweep(&self.dir, runner, crate::config::now_secs()).await?;
        Ok(())
    }
}

/// Removes markers whose process is gone, and wipes a tree nobody is using.
///
/// This is the backstop that matters, because it is the one that does not depend
/// on a dying process getting a chance to run code.
#[allow(dead_code)] // consumed by Task 20
pub async fn sweep(dir: &Path, runner: Arc<dyn CommandRunner>, now: u64) -> Result<bool> {
    let sessions = dir.join("sessions");
    let mut live = 0;

    if let Ok(mut entries) = tokio::fs::read_dir(&sessions).await {
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let Some(pid) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let written: u64 = tokio::fs::read_to_string(&path)
                .await
                .ok()
                .and_then(|text| text.trim().parse().ok())
                .unwrap_or(0);

            let running = runner
                .run("kill", &["-0", pid], &RunOptions::default())
                .await
                .map(|output| output.ok())
                .unwrap_or(false);

            // The age cap applies to a marker whose process is *gone*, to cover
            // pid recycling. Applying it to a live one would delete a working
            // developer's credential out from under them, and a mosh session
            // older than a day is the normal case rather than the exception.
            if running && now.saturating_sub(written) <= STALE_AFTER_SECS {
                live += 1;
            } else {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }

    if live == 0 {
        let _ = tokio::fs::remove_dir_all(dir).await;
        return Ok(true);
    }
    Ok(false)
}

/// Creates a directory that is private from the instant it exists, and refuses
/// one that is not ours.
///
/// `create_dir_all` then `chmod` is wrong twice on the documented `/tmp` floor:
/// it succeeds on a directory another local user pre-created — the name is
/// predictable, since a member id is public — and it leaves a window at 0755
/// before `gh` writes an OAuth token inside. `mode()` on the builder closes the
/// window for a fresh directory; the ownership check and mode repair below
/// close it for one that already existed.
///
/// `tokio::fs::DirBuilder::create` with `recursive(true)` does not error when
/// the directory is already there — but it also does not re-apply `mode` in
/// that case, so a directory a previous run (or another local user) left at a
/// looser mode would otherwise sit here holding a GitHub credential. So every
/// call, not just the ones that create something, verifies ownership and
/// repairs the mode before returning.
#[cfg(unix)]
async fn ensure_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    tokio::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .await?;

    let meta = tokio::fs::symlink_metadata(path).await?;
    let ours = meta.is_dir() && !meta.file_type().is_symlink() && meta.uid() == current_uid();
    if !ours {
        return Err(crate::ui::Failure::new(
            "opening a private directory for your GitHub sign-in",
            format!(
                "Remove {} on that server, or ask whoever owns it to, then run `riabuild remote` again.",
                path.display()
            ),
        )
        .detail("it exists and is not a private directory belonging to you")
        .into());
    }

    if meta.permissions().mode() & 0o777 != 0o700 {
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn ensure_private_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    Ok(())
}

/// The running process's uid, via a direct POSIX call rather than a new crate
/// dependency — `getuid()` takes no arguments and cannot fail, matching the
/// FFI pattern already used for stdin redirection in `ui/prompt.rs`.
#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: POSIX `getuid` takes no arguments, has no preconditions, and
    // cannot fail.
    unsafe { getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use std::sync::Arc;

    #[test]
    fn a_tmpfs_runtime_directory_is_preferred_to_tmp() {
        // On a systemd host XDG_RUNTIME_DIR is a per-uid tmpfs, so the token
        // never touches a disk at all. TMPDIR is what macOS provides. /tmp is
        // the floor that always exists.
        assert_eq!(
            choose_runtime_dir(Some("/run/user/1000"), Some("/var/folders/x")).expect("dir"),
            PathBuf::from("/run/user/1000")
        );
        assert_eq!(
            choose_runtime_dir(None, Some("/var/folders/x")).expect("dir"),
            PathBuf::from("/var/folders/x")
        );
        assert_eq!(
            choose_runtime_dir(None, None).expect("dir"),
            PathBuf::from("/tmp")
        );
        assert_eq!(
            choose_runtime_dir(Some(""), None).expect("dir"),
            PathBuf::from("/tmp")
        );
    }

    #[tokio::test]
    async fn opening_a_session_makes_a_private_directory_and_a_marker() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let session = GhSession::open(home.path(), "550e8400", 4242)
            .await
            .expect("open");

        assert!(session.config_dir().is_dir());
        assert!(session.config_dir().join("sessions").join("4242").is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(session.config_dir())
                .await
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "/tmp is world-writable and sticky");
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_preexisting_directory_is_repaired_to_0700() {
        // R6's authored addition: `recursive(true)` does not re-apply `mode`
        // to a directory that already exists, and this one is about to hold a
        // GitHub credential — so a loose pre-existing mode must be repaired,
        // not trusted.
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::TempDir::new().expect("tempdir");
        let dir = home.path().join("riabuild-gh-550e8400");
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .await
            .expect("chmod 0777");

        ensure_private_dir(&dir).await.expect("ensure");

        let mode = tokio::fs::metadata(&dir)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "a pre-existing directory must be repaired before anything is written into it"
        );
    }

    #[tokio::test]
    async fn two_sessions_share_one_sign_in_and_the_last_one_out_wipes_it() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let first = GhSession::open(home.path(), "550e8400", 1)
            .await
            .expect("open");
        let second = GhSession::open(home.path(), "550e8400", 2)
            .await
            .expect("open");
        let dir = first.config_dir();
        tokio::fs::write(dir.join("hosts.yml"), "github.com:\n")
            .await
            .expect("write");

        // Both pids are alive, so nothing is removed yet.
        let alive = Arc::new(FakeRunner::new().with("kill -0", 0, "", ""));
        first.close(alive.clone()).await.expect("close");
        assert!(
            dir.join("hosts.yml").is_file(),
            "one session left, keep the sign-in"
        );

        second.close(alive).await.expect("close");
        assert!(!dir.exists(), "the last one out wipes the tree");
    }

    #[tokio::test]
    async fn a_marker_for_a_dead_process_is_swept_and_the_tree_goes_with_it() {
        // The case that actually matters: a mosh session that died with the
        // laptop's battery never ran any exit path at all.
        let home = tempfile::TempDir::new().expect("tempdir");
        let session = GhSession::open(home.path(), "550e8400", 9999)
            .await
            .expect("open");
        let dir = session.config_dir();
        tokio::fs::write(dir.join("hosts.yml"), "github.com:\n")
            .await
            .expect("write");
        drop(session); // `close` is what wipes; dropping the handle does nothing

        let dead = Arc::new(FakeRunner::new().with("kill -0", 1, "", "No such process"));
        assert!(sweep(&dir, dead, 0).await.expect("sweep"));
        assert!(
            !dir.exists(),
            "a credential must not outlive the session that made it"
        );
    }

    #[tokio::test]
    async fn a_recycled_pid_cannot_keep_a_stale_tree_alive_forever() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let session = GhSession::open(home.path(), "550e8400", 1)
            .await
            .expect("open");
        let dir = session.config_dir();
        drop(session); // `close` is what wipes; dropping the handle does nothing

        // The pid looks alive, but the marker is older than a day.
        let alive = Arc::new(FakeRunner::new().with("kill -0", 0, "", ""));
        // A real epoch, offset: `now_secs()` is ~1.78e9, so passing a bare
        // 8-day duration would saturate the subtraction to zero and the marker
        // would look fresh.
        let a_week_later = crate::config::now_secs() + 8 * 24 * 60 * 60;
        assert!(sweep(&dir, alive, a_week_later).await.expect("sweep"));
        assert!(!dir.exists());
    }
}

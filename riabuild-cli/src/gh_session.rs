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
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A marker whose process still looks alive is ignored after this long, because
/// pids are recycled and a stale marker would otherwise match a live stranger.
const STALE_AFTER_SECS: u64 = 24 * 60 * 60;

/// The first candidate that **exists and is a directory this user can
/// write**: `XDG_RUNTIME_DIR` (a per-uid tmpfs on a systemd host, so the token
/// never touches a disk at all), then `TMPDIR` (what macOS provides), then
/// `/tmp`, the floor that ordinarily always exists.
///
/// Returning the first non-empty *string* was not the check the design asks
/// for, and the difference is the whole point of this module: `ensure_private_dir`
/// creates with `recursive(true)`, so a stale `XDG_RUNTIME_DIR` inherited from
/// a dead login session — naming a path on persistent disk — was silently
/// *created* rather than skipped, and the GitHub OAuth token landed at rest on
/// a disk while this file's own module doc promised the opposite.
///
/// If none of the three qualifies riabuild stops, rather than inventing a
/// fourth answer: there is nowhere left to put a credential that the promise
/// above can be honoured for.
pub async fn choose_runtime_dir(xdg: Option<&str>, tmpdir: Option<&str>) -> Result<PathBuf> {
    for candidate in [xdg, tmpdir, Some("/tmp")] {
        let Some(value) = candidate.filter(|value| !value.is_empty()) else {
            continue;
        };
        let path = PathBuf::from(value);
        if writable_dir(&path).await {
            return Ok(path);
        }
    }
    Err(crate::ui::Failure::new(
        "finding somewhere to keep this session's GitHub sign-in",
        "Point TMPDIR at a directory you can write to on that server, then run \
         `riabuild remote` again.",
    )
    .detail("none of XDG_RUNTIME_DIR, TMPDIR or /tmp is a directory this account can write")
    .into())
}

/// Does `path` already exist as a directory this account can create entries
/// in?
///
/// The write test is `access(2)`'s `W_OK | X_OK`, not a mode comparison: what
/// decides whether `ensure_private_dir` can make its subdirectory is this
/// uid's effective access, which depends on ownership and group membership as
/// much as on the mode bits. It runs on `spawn_blocking` — which, unlike
/// `block_in_place`, needs no `rt-multi-thread` and never stalls another
/// future (R6).
#[cfg(unix)]
async fn writable_dir(path: &Path) -> bool {
    if !matches!(tokio::fs::metadata(path).await, Ok(meta) if meta.is_dir()) {
        return false;
    }
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(c_path) = CString::new(owned.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `c_path` is a valid, NUL-terminated C string that outlives
        // this call. `access` only reads it, and reports through its return
        // value rather than through any pointer it writes.
        unsafe { libc::access(c_path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
    })
    .await
    .unwrap_or(false)
}

#[cfg(not(unix))]
async fn writable_dir(path: &Path) -> bool {
    matches!(tokio::fs::metadata(path).await, Ok(meta) if meta.is_dir())
}

pub struct GhSession {
    dir: PathBuf,
    marker: PathBuf,
}

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
///
/// A missing `sessions/` directory (never created, or already swept) reads as
/// zero live sessions. Any other `read_dir` failure — a permission problem, a
/// transient IO error — does not: treating it as "nothing is live" would wipe
/// a credential a session still holds out from under it because the sweep
/// happened to hit a bad moment, rather than because nobody needed it anymore.
pub async fn sweep(dir: &Path, runner: Arc<dyn CommandRunner>, now: u64) -> Result<bool> {
    let sessions = dir.join("sessions");
    let mut live = 0;

    match tokio::fs::read_dir(&sessions).await {
        Ok(mut entries) => {
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

                // The age cap applies to a marker whose process is *gone*, to
                // cover pid recycling. Applying it to a live one would delete
                // a working developer's credential out from under them, and a
                // mosh session older than a day is the normal case rather
                // than the exception.
                if running && now.saturating_sub(written) <= STALE_AFTER_SECS {
                    live += 1;
                } else {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", sessions.display()));
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
    tokio::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .await?;

    let owned = path.to_path_buf();
    // Blocking, not async: opening by descriptor and `fstat`/`fchmod`-ing it
    // are POSIX calls tokio has no async wrapper for, same as `current_uid`
    // below. `spawn_blocking` — unlike `block_in_place` — runs on tokio's
    // dedicated blocking-task pool rather than a reactor worker thread, so it
    // needs no `rt-multi-thread` and never stalls another future.
    tokio::task::spawn_blocking(move || verify_and_repair_mode(&owned)).await??;
    Ok(())
}

#[cfg(not(unix))]
async fn ensure_private_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    Ok(())
}

/// Verifies (and if needed, repairs) the mode of a directory that
/// `DirBuilder::create` reports already existed — by descriptor, not by name.
///
/// Statting the path and then, in a second call, `chmod`-ing the same path
/// leaves a window between the two where the name could be repointed at
/// something else — a symlink swapped in by a concurrent local process. Doing
/// both through one file descriptor opened with `O_NOFOLLOW | O_DIRECTORY`
/// closes that window: the fd names one fixed inode from open to close, there
/// is no second name lookup for an attacker to win, and a symlink at `path`
/// makes `open` itself fail rather than silently following it.
#[cfg(unix)]
fn verify_and_repair_mode(path: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("{} contains a NUL byte", path.display()))?;

    // SAFETY: `c_path` is a valid, NUL-terminated C string that outlives this
    // call. `O_NOFOLLOW` refuses to open a symlink at the final path
    // component instead of following it; `O_DIRECTORY` refuses anything that
    // is not actually a directory. Either failure surfaces as `open`
    // returning -1, handled below.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("opening {} to check it is private", path.display()));
    }

    let result = check_and_chmod(fd, path);

    // SAFETY: `fd` was returned by the `open` call above and is not used
    // again after this point on any path.
    unsafe {
        libc::close(fd);
    }
    result
}

/// The `fstat` + ownership check + `fchmod` performed against an already-open,
/// already-verified-to-be-a-real-directory descriptor. Split out of
/// `verify_and_repair_mode` so that function's `close` always runs, on every
/// return path from here, including an early `?`.
#[cfg(unix)]
fn check_and_chmod(fd: i32, path: &Path) -> Result<()> {
    // SAFETY: `stat` is zero-initialized before being handed to `fstat`,
    // which fills every field `libc::stat`'s layout defines; nothing here
    // reads a field `fstat` did not write.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and valid for the duration of this call, and
    // `&mut stat` is a valid, writable buffer of `fstat`'s expected layout.
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("checking who owns {}", path.display()));
    }

    if stat.st_uid != current_uid() {
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

    if stat.st_mode & 0o777 != 0o700 {
        // SAFETY: `fd` is open, valid, and known (by the `fstat` above) to
        // name a directory this process owns.
        if unsafe { libc::fchmod(fd, 0o700) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("repairing {} to mode 0700", path.display()));
        }
    }
    Ok(())
}

/// The running process's uid. `libc::getuid` takes no arguments and cannot
/// fail.
#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: POSIX `getuid` takes no arguments, has no preconditions, and
    // cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use std::sync::Arc;

    /// Two real directories standing in for `XDG_RUNTIME_DIR` and `TMPDIR`,
    /// plus their paths as strings.
    async fn two_candidates() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::TempDir::new().expect("tempdir");
        let xdg = base.path().join("run");
        let tmp = base.path().join("tmp");
        tokio::fs::create_dir_all(&xdg).await.expect("mkdir");
        tokio::fs::create_dir_all(&tmp).await.expect("mkdir");
        (base, xdg, tmp)
    }

    #[tokio::test]
    async fn a_tmpfs_runtime_directory_is_preferred_to_tmp() {
        // On a systemd host XDG_RUNTIME_DIR is a per-uid tmpfs, so the token
        // never touches a disk at all. TMPDIR is what macOS provides. /tmp is
        // the floor that ordinarily always exists.
        let (_base, xdg, tmp) = two_candidates().await;
        assert_eq!(
            choose_runtime_dir(Some(&xdg.to_string_lossy()), Some(&tmp.to_string_lossy()))
                .await
                .expect("dir"),
            xdg
        );
        assert_eq!(
            choose_runtime_dir(None, Some(&tmp.to_string_lossy()))
                .await
                .expect("dir"),
            tmp
        );
        assert_eq!(
            choose_runtime_dir(None, None).await.expect("dir"),
            PathBuf::from("/tmp")
        );
        assert_eq!(
            choose_runtime_dir(Some(""), None).await.expect("dir"),
            PathBuf::from("/tmp")
        );
    }

    #[tokio::test]
    async fn a_stale_runtime_directory_is_skipped_rather_than_created() {
        // The design asks for the first candidate "that exists and is a
        // directory this user can write". Returning the first non-empty
        // string instead meant an XDG_RUNTIME_DIR inherited from a dead login
        // session — naming a path on persistent disk — was silently *created*
        // by `ensure_private_dir`'s `recursive(true)`, putting the GitHub
        // OAuth token at rest on a disk. It must be fallen through, not
        // conjured.
        let (_base, xdg, tmp) = two_candidates().await;
        let stale = xdg.join("user").join("1000");

        let chosen =
            choose_runtime_dir(Some(&stale.to_string_lossy()), Some(&tmp.to_string_lossy()))
                .await
                .expect("dir");
        assert_eq!(chosen, tmp);
        assert!(
            !stale.exists(),
            "choosing a candidate must never be what creates it"
        );
    }

    #[tokio::test]
    async fn a_candidate_that_is_not_a_directory_is_skipped() {
        let (base, _xdg, tmp) = two_candidates().await;
        let file = base.path().join("not-a-directory");
        tokio::fs::write(&file, "").await.expect("write");

        assert_eq!(
            choose_runtime_dir(Some(&file.to_string_lossy()), Some(&tmp.to_string_lossy()))
                .await
                .expect("dir"),
            tmp
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_directory_this_account_cannot_write_is_skipped() {
        // "exists and is a directory" is not enough on its own: a read-only
        // directory would fail at the first `mkdir` inside it, after riabuild
        // had already committed to putting a credential there.
        use std::os::unix::fs::PermissionsExt;
        let (base, xdg, tmp) = two_candidates().await;
        tokio::fs::set_permissions(&xdg, std::fs::Permissions::from_mode(0o555))
            .await
            .expect("chmod 0555");

        let chosen = choose_runtime_dir(Some(&xdg.to_string_lossy()), Some(&tmp.to_string_lossy()))
            .await
            .expect("dir");
        // Restored before the TempDir's own cleanup, which cannot remove a
        // child of a directory it may not write.
        tokio::fs::set_permissions(&xdg, std::fs::Permissions::from_mode(0o700))
            .await
            .expect("chmod 0700");
        drop(base);
        assert_eq!(chosen, tmp);
    }

    #[tokio::test]
    async fn nowhere_writable_at_all_is_an_actionable_failure() {
        let base = tempfile::TempDir::new().expect("tempdir");
        let missing = base.path().join("gone");
        // `/tmp` is the third candidate and does exist here, so the "none
        // qualified" arm is reached by way of the two that do not — the
        // ordering, rather than the error, is what this pins. The error arm
        // itself is unreachable on any machine with a writable /tmp, which is
        // every machine riabuild targets.
        assert_eq!(
            choose_runtime_dir(
                Some(&missing.to_string_lossy()),
                Some(&missing.to_string_lossy())
            )
            .await
            .expect("falls through to /tmp"),
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
    #[cfg(unix)]
    async fn a_symlink_at_the_target_path_is_refused() {
        // The one branch that actually defends against a cross-account
        // attacker: the directory name is predictable (a member id is
        // public), so someone else on the box could pre-plant a symlink
        // there pointing wherever they like, hoping riabuild writes a GitHub
        // credential through it. `O_NOFOLLOW` is what refuses to open through
        // it at all, rather than trusting a `symlink_metadata` check that a
        // second, separate syscall could race.
        let home = tempfile::TempDir::new().expect("tempdir");
        let elsewhere = home.path().join("elsewhere");
        tokio::fs::create_dir_all(&elsewhere)
            .await
            .expect("mkdir elsewhere");
        let link = home.path().join("riabuild-gh-550e8400");
        tokio::fs::symlink(&elsewhere, &link)
            .await
            .expect("symlink");

        let result = ensure_private_dir(&link).await;
        assert!(
            result.is_err(),
            "a symlink standing in for the directory must be refused, not followed"
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

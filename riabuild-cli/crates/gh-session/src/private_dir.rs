//! Creating the directory the choice in `runtime_dir` named, private from the
//! instant it exists.
//!
//! Everything here is about one hostile case: the directory name is
//! predictable, because a member id is public, so another local account can
//! get there first — with a directory of its own, or with a symlink pointing
//! wherever it likes. The answers are a non-recursive create, a mode applied
//! at creation rather than after it, and an ownership check made through a
//! descriptor opened with `O_NOFOLLOW | O_DIRECTORY` so there is no second
//! name lookup to race.

use anyhow::Result;
use std::path::Path;

#[cfg(unix)]
use anyhow::Context;

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
/// The create is **not** recursive, and that is `choose_runtime_dir`'s check
/// surviving to the moment it is used. `recursive(true)` creates missing
/// parents, so a runtime directory that went away between `writable_dir`'s
/// `access(2)` and this call — a `/run/user/1000` torn down when the last login
/// session ended, a `TMPDIR` cleaned — was silently *conjured* here instead,
/// which is the "GitHub OAuth token at rest on persistent disk" failure that
/// whole function exists to prevent, through a smaller window. Non-recursive
/// makes the parent's absence `ENOENT`, which is the truth.
///
/// An already-existing directory is `EEXIST` rather than the silent success
/// `recursive(true)` gave, and is accepted for the same reason it was before —
/// but `mode` is not re-applied to one, so a directory a previous run (or
/// another local user) left at a looser mode would otherwise sit here holding a
/// GitHub credential. So every call, not just the ones that create something,
/// verifies ownership and repairs the mode before returning.
#[cfg(unix)]
pub(super) async fn ensure_private_dir(path: &Path) -> Result<()> {
    match tokio::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(false)
        .create(path)
        .await
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("creating {}", path.display()));
        }
    }

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
pub(super) async fn ensure_private_dir(path: &Path) -> Result<()> {
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
        return Err(riabuild_ui::Failure::new(
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
    // Gated with the tests: every test in this file is `#[cfg(unix)]`, so on
    // any other platform this glob would bring in nothing and be an unused
    // import — which `-D warnings` makes a build failure nobody here can see.
    #[cfg(unix)]
    use super::*;

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
}

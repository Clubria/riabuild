//! Deciding *where* a session's GitHub configuration directory goes.
//!
//! Only the choice lives here: walking the candidate list, and the `access(2)`
//! test that says a candidate is a directory this account can actually write.
//! Creating the directory the choice names is `private_dir`'s job, and the
//! session lifetime built on top of it is the parent module's.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// The first candidate that **exists and is a directory this user can
/// write**: `XDG_RUNTIME_DIR` (a per-uid tmpfs on a systemd host, so the token
/// never touches a disk at all), then `TMPDIR` (what macOS provides), then
/// `/tmp`, the floor that ordinarily always exists.
///
/// Returning the first non-empty *string* was not the check the design asks
/// for, and the difference is the whole point of this module: `ensure_private_dir`
/// used to create with `recursive(true)`, so a stale `XDG_RUNTIME_DIR` inherited
/// from a dead login session — naming a path on persistent disk — was silently
/// *created* rather than skipped, and the GitHub OAuth token landed at rest on
/// a disk while this file's own module doc promised the opposite. It now creates
/// non-recursively, so the check made here also holds at the moment it is used
/// rather than only when it was made.
///
/// If none of the three qualifies riabuild stops, rather than inventing a
/// fourth answer: there is nowhere left to put a credential that the promise
/// above can be honoured for.
pub async fn choose_runtime_dir(xdg: Option<&str>, tmpdir: Option<&str>) -> Result<PathBuf> {
    first_writable(&[xdg, tmpdir, Some("/tmp")]).await
}

/// The search itself, split out so its failure is reachable.
///
/// With `/tmp` hard-coded as the last candidate above, the "nothing qualified"
/// arm is dead on every machine riabuild targets — and an error nobody can
/// reach is an error nobody has read. This seam is what lets a test run the
/// list to the end.
async fn first_writable(candidates: &[Option<&str>]) -> Result<PathBuf> {
    for candidate in candidates {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn two_unusable_candidates_fall_through_to_the_tmp_floor() {
        // What this pins is the ordering: `/tmp` is the third candidate and
        // does exist, so a missing XDG_RUNTIME_DIR and a missing TMPDIR land
        // there rather than failing. (It used to be named for the failure it
        // never reached — the error arm is unreachable through this entry
        // point on any machine with a writable /tmp, which is every machine
        // riabuild targets. `first_writable` below is where that arm is
        // covered.)
        let base = tempfile::TempDir::new().expect("tempdir");
        let missing = base.path().join("gone");
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
    async fn nowhere_writable_at_all_is_an_actionable_failure() {
        // The arm that had no coverage anywhere in the suite. Reached through
        // `first_writable` rather than `choose_runtime_dir`, because the
        // latter appends `/tmp`, which qualifies on every machine riabuild
        // runs on — the alternative would be pretending to test it. What
        // matters is that it is a `Failure` with somewhere to go, not an
        // anonymous IO error: there is nowhere left to put a GitHub credential
        // that this module's promise can be honoured for, and the developer is
        // the only one who can change that.
        let base = tempfile::TempDir::new().expect("tempdir");
        let missing = base.path().join("gone").to_string_lossy().into_owned();
        let error = first_writable(&[Some(&missing), None, Some("")])
            .await
            .expect_err("nothing qualified");
        let failure = error
            .downcast_ref::<crate::ui::Failure>()
            .unwrap_or_else(|| panic!("must be the actionable Failure: {error}"));
        assert!(failure.action.contains("TMPDIR"), "{}", failure.action);
    }
}

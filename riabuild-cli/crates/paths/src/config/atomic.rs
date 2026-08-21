//! riabuild's one atomic write.
//!
//! Everything that replaces a file whose half-written state a reader could
//! observe lands through [`write_atomic`] rather than growing a temp-naming
//! convention of its own. The four properties it guarantees — whole or nothing,
//! private from the instant it exists, never written through a symlink, durable
//! — are set out on the function itself, so a caller does not have to re-derive
//! them.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

pub async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    write_atomic(path, format!("{text}\n").as_bytes()).await
}

/// Writes beside the target and renames over it, so a reader sees the whole old
/// file or the whole new one and never the gap between.
///
/// **This is riabuild's one atomic write.** Anything that replaces a file whose
/// half-written state a reader could observe calls this rather than growing a
/// fourth temp-naming convention of its own — `tasks::shims` and
/// `remote::store` already do. The properties it guarantees, so a caller does
/// not have to re-derive them:
///
/// - **Whole or nothing.** `tokio::fs::write` truncates and then writes, and an
///   interrupt inside that window leaves a truncated file. For `state.json`
///   that is harmless — a cache that will not parse means "check everything
///   again". For `config.json` it is not, and for a file holding a secret it is
///   worse still: a reader between the truncate and the write sees an empty
///   file and concludes there is no secret. Same reasoning as
///   `archive/staging.rs`, and the same requirement that the temporary share a
///   directory with its target so the rename is atomic rather than a copy.
/// - **Private from the instant it exists.** The temporary is created `0600`
///   rather than at the umask, so it is never briefly world-readable and the
///   file the rename leaves behind is `0600` too. A caller that needs another
///   mode sets it afterwards — every generated shim does, through
///   `archive::make_executable`.
/// - **Never written through a symlink.** The temporary is opened with
///   `create_new`, and `O_CREAT | O_EXCL` refuses a symlink at that name
///   instead of following it. A symlink at the *target* is replaced by the
///   rename rather than written through, which is the difference between this
///   and an `OpenOptions::open` on the target itself.
/// - **Durable.** The contents are `fsync`ed before the rename and the
///   containing directory after it.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("could not create {}", parent.display()))?;
    }

    let (temp, mut file) = create_temporary(path).await?;
    let written = async {
        file.write_all(bytes).await?;
        // Durable before the rename, so a power loss cannot leave the new name
        // pointing at blocks that were never written.
        file.sync_all().await
    }
    .await;
    drop(file);

    if let Err(error) = written {
        // Best effort: the error being returned says more than this could.
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error).with_context(|| format!("could not write {}", temp.display()));
    }

    tokio::fs::rename(&temp, path)
        .await
        .with_context(|| format!("could not replace {}", path.display()))?;

    if let Some(parent) = path.parent() {
        sync_directory(parent).await;
    }
    Ok(())
}

/// How many names `create_temporary` will try before giving up.
///
/// More than one because a temporary is only *probably* free: a crashed run of
/// an earlier process that happened to hold this pid can have left one behind,
/// and pids are reused. More than a handful would be pretending there is a
/// contention problem here rather than a leftover.
const TEMP_NAME_ATTEMPTS: usize = 4;

/// Creates the temporary, private from the instant it exists, and hands back
/// the name it settled on with the open handle.
///
/// `create_new` rather than `create`: `O_CREAT | O_EXCL` never follows a
/// symlink, so a name planted at the temporary path fails the open instead of
/// redirecting the write — and on a shared server, where every developer can
/// see the others' `riabuild` running, that path is guessable. The cost is that
/// a leftover from a crashed run is `EEXIST`, which is what the retry is for;
/// each attempt asks `temp_beside` for a fresh name rather than unlinking
/// anything, because unlinking a file we did not create is how you take over
/// somebody else's write rather than how you recover from one.
async fn create_temporary(path: &Path) -> Result<(std::path::PathBuf, tokio::fs::File)> {
    let mut taken = Vec::new();
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let temp = temp_beside(path);
        match create_private(&temp).await {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                taken.push(temp.display().to_string());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not create {}", temp.display()));
            }
        }
    }
    anyhow::bail!(
        "could not find a free temporary name beside {} — tried {}",
        path.display(),
        taken.join(", ")
    )
}

/// `O_CREAT | O_EXCL | O_WRONLY` at mode `0600`.
#[cfg(unix)]
async fn create_private(temp: &Path) -> std::io::Result<tokio::fs::File> {
    // `mode` is tokio's own, behind `cfg(unix)` — no `OpenOptionsExt` import,
    // which on this type would be an unused one and so a build failure under
    // `-D warnings`.
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp)
        .await
}

#[cfg(not(unix))]
async fn create_private(temp: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .await
}

/// `fsync`s the directory a rename just landed a name in.
///
/// `sync_all` on the file makes its *contents* durable; the directory entry
/// naming them is separate metadata, and a power loss between the two can leave
/// a directory listing neither the temporary nor the target — the gap the
/// comment above believed the file's own `fsync` had closed.
///
/// Best effort, and this is the only place in this file that swallows an error.
/// The rename has already succeeded, so every reader from this moment on sees
/// the new file; what an unsyncable directory costs is durability across a
/// power loss, and reporting that as a failed write would be a lie about a
/// write that landed. Not every filesystem lets a directory be opened at all —
/// and one that does not is not a machine riabuild should refuse to provision.
async fn sync_directory(dir: &Path) {
    if let Ok(handle) = tokio::fs::File::open(dir).await {
        let _ = handle.sync_all().await;
    }
}

/// `…/.state.json.4171-3.tmp`, in the target's own directory.
///
/// The counter is not decoration, for the same reason `archive/staging.rs`
/// carries one: keyed on the pid alone, two writes to one path from a single
/// process would compute the same temporary and unpack over each other.
fn temp_beside(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let call = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}-{call}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::State;
    use crate::{Paths, RealPaths};
    use tempfile::TempDir;

    #[tokio::test]
    async fn a_write_leaves_no_temporary_behind() {
        let home = TempDir::new().unwrap();
        let path = home.path().join("state.json");

        write_json(&path, &State::default()).await.expect("write");

        let mut entries = tokio::fs::read_dir(home.path()).await.expect("read_dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(
            names,
            vec!["state.json".to_string()],
            "the temporary must be renamed away, not left beside the target"
        );
    }

    /// before it writes, and `load` answers a truncated file with `Default`.
    #[tokio::test]
    async fn a_reader_never_observes_a_half_written_file() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("mkdir");

        let mut full = State::default();
        for n in 0..200 {
            full.mark_satisfied(&format!("task_{n}"), 1, "never_run");
        }
        write_json(&paths.state_file(), &full).await.expect("seed");

        let writer = {
            let paths = RealPaths::rooted_at(home.path());
            tokio::spawn(async move {
                for _ in 0..40 {
                    write_json(&paths.state_file(), &full).await.expect("write");
                    tokio::task::yield_now().await;
                }
            })
        };

        for _ in 0..40 {
            let seen = State::load(&paths).await;
            assert_eq!(
                seen.tasks.len(),
                200,
                "a reader saw a file that was neither the old one nor the new one"
            );
            tokio::task::yield_now().await;
        }

        writer.await.expect("join");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_temporary_is_never_at_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().unwrap();
        let target = home.path().join("state.json");

        // Asserted through `create_temporary` directly, because the temporary
        // is unobservable from outside: it is created and renamed away inside
        // one call, and a test that raced a reader against it would pass or
        // fail on disk timing. The mode the rename then carries onto the target
        // is what the test below pins.
        let (temp, file) = create_temporary(&target).await.expect("temporary");
        let mode = tokio::fs::metadata(&temp)
            .await
            .expect("stat")
            .permissions()
            .mode();
        drop(file);

        assert_eq!(
            mode & 0o777,
            0o600,
            "0666 & umask would leave a secret-bearing caller's bytes world-readable \
             for the length of the write"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn what_the_rename_leaves_behind_is_private_too() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().unwrap();
        let path = home.path().join("state.json");

        write_json(&path, &State::default()).await.expect("write");

        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the target inherits the temporary's mode, which is what lets \
             `FileKeychain` land a session token through this"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_planted_at_the_temporary_name_is_refused() {
        let home = TempDir::new().unwrap();
        let elsewhere = home.path().join("elsewhere");
        let planted = home.path().join("planted");
        tokio::fs::symlink(&elsewhere, &planted)
            .await
            .expect("symlink");

        let error = create_private(&planted).await.expect_err("must refuse");

        assert_eq!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists,
            "O_CREAT | O_EXCL reports the symlink itself, rather than following it"
        );
        assert!(
            !tokio::fs::try_exists(&elsewhere).await.unwrap(),
            "nothing may be written through a symlink somebody else planted"
        );
    }
}

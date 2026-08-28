//! Which of this laptop's windows are connected to a server right now.
//!
//! One person with a server open in three terminals is the ordinary way remote
//! mode is used, and until this existed nothing in riabuild could *count* them.
//! Every question that needed the answer got a worse one instead: the clipboard
//! channel asked "am I the one serving?" (see [`super::channel::lease`], which
//! answers a different question and answers it well), and `remote forget` asked
//! nothing at all — it revoked the server's session, pulled riabuild's key out
//! of `authorized_keys` and `rm -rf`'d the namespace while a colleague-shaped
//! stranger who was in fact the developer's own other terminal sat in a shell
//! that all three of those things were holding up.
//!
//! ## An `flock` per window, for the reason the lease is one
//!
//! A window is *present* for as long as its riabuild process is running, and
//! nothing else expresses that. A pid in a file outlives the process that wrote
//! it, so it needs a sweep, and a `kill -0` on a recycled pid says "still
//! connected" about a process that is somebody else's `vim`. An age cap cannot
//! be used either: a remote session outliving a day is the normal case, which
//! is exactly what `gh_session`'s cap is allowed to assume and this is not.
//!
//! The kernel drops an `flock` when the holding process exits — cleanly, on a
//! `SIGKILL`, or with the laptop's lid closed on it — so "that window has gone"
//! and "the lock is free" are one question and one syscall. `channel::lease`
//! sets the same argument out at length; this is the same mechanism counting
//! rather than choosing.
//!
//! ## What it is not
//!
//! Not a lock on anything, and never something a session waits for. Every
//! window takes its own file and none of them contend, so [`join`] cannot fail
//! for a reason another window caused. And not a record of the *server's*
//! sessions: it is this laptop's own windows, which is all `forget` needs to
//! know and all riabuild can honestly claim to know — a second laptop's session
//! to that server leaves nothing here, and the dashboard's session list is
//! where that one is visible.

use crate::Remote;
use anyhow::{Context, Result};
use riabuild_paths::Paths;
use riabuild_paths::filelock::FileLock;
use std::path::{Path, PathBuf};

/// Where this laptop records which of its windows have `remote` open.
///
/// Keyed by [`Remote::hash`], the same answer the SSH identity and the channel
/// lease are filed under, so two windows into one server meet and two windows
/// into two servers do not.
pub(crate) fn dir(paths: &dyn Paths, remote: &Remote) -> PathBuf {
    paths.root().join("remote-windows").join(remote.hash())
}

/// This window's place in the count, held for as long as the value is alive.
///
/// Dropping it — or the process ending however it ends — takes this window out
/// of the count. There is deliberately no `leave()`: a window that stops
/// existing without running one is the case that has to be right, and only the
/// kernel can promise that.
pub(crate) struct Present {
    _lock: FileLock,
}

/// Announces this window, and returns `None` when riabuild cannot.
///
/// `None` rather than an error, at every step. This is a courtesy to a *later*
/// `forget`, never a precondition of the session the developer asked for: a
/// laptop whose `~/.riabuild` cannot be written is not a laptop that should be
/// refused a shell over a counter. What that costs is a `forget` that fails to
/// warn, which is exactly where riabuild already was.
pub(crate) async fn join(paths: &dyn Paths, remote: &Remote) -> Option<Present> {
    let dir = dir(paths, remote);
    tokio::fs::create_dir_all(&dir).await.ok()?;
    // The pid is for whoever reads `~/.riabuild` with their eyes. What decides
    // whether the window is still there is the lock, never the name.
    let lock = FileLock::try_acquire(&dir.join(format!("{}.lock", std::process::id())))
        .await
        .ok()??;
    Some(Present { _lock: lock })
}

/// How many *other* windows of this laptop have `remote` open right now.
///
/// Counts the locks it cannot take, and clears the ones it can — a file whose
/// lock is free belongs to a window that has ended, and sweeping it here is
/// what keeps a long-lived server's directory from accumulating one file per
/// run for ever.
///
/// Never counts this process's own [`Present`]: `flock` is per open file
/// description, and this asks through a *new* one, so a lock this process
/// already holds is correctly reported as taken. Callers that hold one — none
/// today; `forget` does not open a session — would have to subtract it.
///
/// Best effort, and an unreadable directory is zero. The caller's use for this
/// is a sentence of warning, and a warning riabuild cannot compose is not a
/// reason to refuse the command the developer typed.
pub(crate) async fn live(paths: &dyn Paths, remote: &Remote) -> usize {
    count(&dir(paths, remote)).await.unwrap_or(0)
}

async fn count(dir: &Path) -> Result<usize> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };

    let mut live = 0;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().is_none_or(|kind| kind != "lock") {
            continue;
        }
        match FileLock::try_acquire(&path).await {
            // Free: that window has gone. Dropped before the unlink for
            // legibility — removing a file somebody holds open is ordinary on
            // Unix, and the lock is on the inode either way.
            Ok(Some(lock)) => {
                drop(lock);
                let _ = tokio::fs::remove_file(&path).await;
            }
            // Held: a window that is still open. Or a file riabuild could not
            // ask about, which is counted as present for the same reason the
            // issued agent's sweep leaves one alone — the direction that costs
            // a needless warning, never a broken session.
            Ok(None) | Err(_) => live += 1,
        }
    }
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// The count is of windows that are still open, and a window is open for
    /// exactly as long as its process is.
    #[tokio::test]
    async fn a_window_counts_while_it_is_open_and_not_afterwards() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());

        assert_eq!(live(&paths, &remote()).await, 0);

        // Two windows. `join` keys on this process's pid, so the second stands
        // in for the other terminal by taking its own file the same way.
        let first = join(&paths, &remote()).await.expect("joins");
        let dir = dir(&paths, &remote());
        let second = FileLock::try_acquire(&dir.join("99999.lock"))
            .await
            .expect("lock")
            .expect("free");
        assert_eq!(live(&paths, &remote()).await, 2);

        drop(second);
        assert_eq!(
            live(&paths, &remote()).await,
            1,
            "a window that ended must stop counting without anything being run"
        );
        drop(first);
        assert_eq!(live(&paths, &remote()).await, 0);
    }

    /// A window that ended leaves a file, and the next count clears it.
    ///
    /// Without this, a server somebody connects to twice a day accumulates a
    /// file per run for the life of the laptop — inert, but the kind of litter
    /// that eventually makes somebody delete the directory by hand while a
    /// window is open.
    #[tokio::test]
    async fn counting_sweeps_the_windows_that_have_gone() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dir = dir(&paths, &remote());
        let ended = dir.join("12345.lock");
        drop(
            FileLock::try_acquire(&ended)
                .await
                .expect("lock")
                .expect("free"),
        );
        assert!(tokio::fs::metadata(&ended).await.is_ok());

        assert_eq!(live(&paths, &remote()).await, 0);

        assert!(
            tokio::fs::metadata(&ended).await.is_err(),
            "a lock nobody holds is a window that has gone, and its file goes with it"
        );
    }

    /// One laptop, two boxes: a window into one is not a window into the other.
    #[tokio::test]
    async fn windows_into_two_servers_are_counted_apart() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let other = Remote {
            name: "build-02".into(),
            host: "build-02.fly.dev".into(),
            ..remote()
        };

        let _here = join(&paths, &remote()).await.expect("joins");

        assert_eq!(live(&paths, &remote()).await, 1);
        assert_eq!(live(&paths, &other).await, 0);
    }
}

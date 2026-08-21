//! One exclusive advisory lock, so two riabuilds on one machine take turns.
//!
//! riabuild can be running in two terminal windows, and both write the same
//! three JSON files. This is what makes the read-modify-write around them
//! atomic — see `config::State::update`.
//!
//! The locking is `std::fs::File::try_lock` and `lock`, which is what cargo
//! uses for the same job after migrating off `fs2` and `fs4`. On unix those are
//! `flock` with `LOCK_EX`, `LOCK_EX | LOCK_NB` and `LOCK_UN`; on Windows they
//! are `LockFileEx`. No dependency, no `unsafe`, and no platform arm here.

use anyhow::{Context, Result};
use std::path::Path;

/// A held lock. Dropping it releases the lock.
pub struct FileLock {
    /// `None` when the filesystem refused to lock at all, so every caller has
    /// one shape to handle whether or not locking was possible.
    _file: Option<std::fs::File>,
}

impl FileLock {
    /// Takes the lock, calling `on_wait` once if — and only if — another
    /// process holds it and this call is about to wait for them.
    ///
    /// `try_lock` first, then report, then block. The uncontended path costs one
    /// syscall and says nothing, and a wait is announced exactly once rather
    /// than once per poll of a retry loop. This is cargo's sequence, which
    /// developers here already meet as `Blocking waiting for file lock` when two
    /// worktrees build at once.
    pub async fn acquire(path: &Path, on_wait: impl FnOnce()) -> Result<Self> {
        let file = open_for_locking(path).await?;

        match file.try_lock() {
            Ok(()) => return Ok(Self { _file: Some(file) }),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) if refuses_to_lock(&error) => {
                // Fail open, and only here. riabuild is the first thing to run
                // on a machine nobody has characterised — a home directory on
                // NFS, an unusual container filesystem — and "cannot provision,
                // because cannot lock" is a worse answer for a provisioner than
                // the rare interleaving the lock guards against.
                return Ok(Self { _file: None });
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("could not lock {}", path.display()));
            }
        }

        on_wait();

        // `lock()` parks the thread until the holder releases it, and riabuild
        // runs its reactor on this one — so it goes to the blocking pool, for
        // the same reason `runner/pty.rs` never issues a blocking read. The file
        // moves in and comes back out; `std::fs::File` is `Send`.
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
            file.lock()?;
            Ok(file)
        })
        .await
        .context("the thread waiting for the lock did not finish")?
        .with_context(|| format!("could not lock {}", path.display()))?;

        Ok(Self { _file: Some(file) })
    }

    /// Takes the lock if it is free, and answers `None` rather than waiting when
    /// somebody else holds it.
    ///
    /// The sibling of [`acquire`](Self::acquire) for the one caller that must
    /// not queue: the clipboard channel's ownership. Every remote session to a
    /// server tries this, one of them wins, and the losers stand by and try
    /// again later — so a *wait* here would be a session queueing to own a
    /// channel it may never be asked to serve, holding a blocking-pool thread
    /// for as long as its sibling's shell is open.
    ///
    /// **What makes it the right primitive there is who releases it.** An
    /// `flock` is the kernel's, not a file's contents: it goes when the holding
    /// process exits, however it exits. A pid written into a file has to be
    /// swept by somebody, `kill -0` is a lie after the number is recycled, and
    /// the failure that costs is a channel that never comes back because a
    /// marker outlived the laptop that wrote it.
    pub async fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = open_for_locking(path).await?;

        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: Some(file) })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            // Fail open, as `acquire` does and for the same reason. Here that
            // means "you own it": a filesystem with no locking would otherwise
            // leave every session standing by for a holder that cannot exist,
            // and a channel nobody starts is worse than the two that briefly
            // race — the second pump is refused by the first and retries.
            Err(std::fs::TryLockError::Error(error)) if refuses_to_lock(&error) => {
                Ok(Some(Self { _file: None }))
            }
            Err(std::fs::TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("could not lock {}", path.display()))
            }
        }
    }
}

/// The handle both of the above lock, created on demand.
///
/// Opened through tokio and handed over, because the locking methods live on
/// `std::fs::File` and `tokio::fs::File` does not have them. This handle exists
/// to be locked: nothing is ever read or written through it. `read` as well as
/// `write` because Windows refuses to lock a handle opened append-only, and it
/// costs nothing on unix.
async fn open_for_locking(path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("could not create {}", parent.display()))?;
    }

    Ok(tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await
        .with_context(|| format!("could not open {}", path.display()))?
        .into_std()
        .await)
}

/// Whether the filesystem is saying it does not do locking at all.
///
/// Written as comparisons rather than an or-pattern on purpose. `ENOTSUP` and
/// `EOPNOTSUPP` are the same value on Linux and different values on macOS, so
/// `Some(libc::ENOTSUP | libc::EOPNOTSUPP)` is a correct match on one platform
/// and an unreachable-pattern warning on the other — which `-D warnings` turns
/// into a build that fails on Linux only.
fn refuses_to_lock(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::ENOSYS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[tokio::test]
    async fn an_uncontended_lock_is_taken_without_reporting_a_wait() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join(".state.lock");
        let waited = Arc::new(AtomicBool::new(false));

        let flag = Arc::clone(&waited);
        let _lock = FileLock::acquire(&path, move || flag.store(true, Ordering::SeqCst))
            .await
            .expect("acquire");

        assert!(
            !waited.load(Ordering::SeqCst),
            "an uncontended acquire must say nothing at all"
        );
        assert!(path.exists(), "the lock file is created on demand");
    }

    /// The very first riabuild on a machine locks before `~/.riabuild` exists.
    #[tokio::test]
    async fn the_parent_directory_is_created_when_it_is_missing() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join("riabuild").join(".state.lock");

        let _lock = FileLock::acquire(&path, || {}).await.expect("acquire");

        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_dropped_lock_can_be_taken_again() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join(".state.lock");

        let lock = FileLock::acquire(&path, || {}).await.expect("first");
        drop(lock);

        let waited = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&waited);
        let _second = FileLock::acquire(&path, move || flag.store(true, Ordering::SeqCst))
            .await
            .expect("second");

        assert!(
            !waited.load(Ordering::SeqCst),
            "a released lock is not contended"
        );
    }

    /// The point of the whole file: a second acquire waits for the first to let
    /// go, and says so exactly once.
    ///
    /// Runs on the current-thread runtime riabuild itself uses, which is the
    /// whole point: the waiting acquire parks a *blocking-pool* thread rather
    /// than the reactor, so this task can still sleep and then release. A
    /// blocking `lock()` on the reactor thread would deadlock here instead.
    #[tokio::test]
    async fn a_contended_lock_waits_for_the_holder_and_reports_it_once() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join(".state.lock");
        let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let waits = Arc::new(AtomicUsize::new(0));

        let held = FileLock::acquire(&path, || {}).await.expect("first");

        let second = {
            let path = path.clone();
            let order = Arc::clone(&order);
            let waits = Arc::clone(&waits);
            tokio::spawn(async move {
                let counter = Arc::clone(&waits);
                let lock = FileLock::acquire(&path, move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
                .await
                .expect("second acquire");
                order.lock().expect("order").push("second acquired");
                drop(lock);
            })
        };

        // Long enough for the spawned task to reach the blocking wait.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        order.lock().expect("order").push("first released");
        drop(held);

        second.await.expect("join");

        assert_eq!(
            *order.lock().expect("order"),
            vec!["first released", "second acquired"],
            "the second acquire completed before the first let go"
        );
        assert_eq!(
            waits.load(Ordering::SeqCst),
            1,
            "contention is reported once, not once per retry"
        );
    }

    /// The whole of what the channel's ownership needs: a held lock answers
    /// `None` instead of queueing, and a released one is takeable again.
    ///
    /// A *wait* here would be the bug rather than a slow path — a session
    /// standing by would park a blocking-pool thread for as long as its
    /// sibling's shell stayed open, and would then take the channel over at the
    /// moment that shell exited whether or not it was still there to serve it.
    #[tokio::test]
    async fn a_try_acquire_answers_rather_than_waiting_and_can_be_retaken() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join("owner.lock");

        let held = FileLock::try_acquire(&path)
            .await
            .expect("try")
            .expect("the first caller owns a free lock");

        assert!(
            FileLock::try_acquire(&path).await.expect("try").is_none(),
            "a lock somebody holds must be refused, never waited for"
        );

        drop(held);

        assert!(
            FileLock::try_acquire(&path).await.expect("try").is_some(),
            "an owner that let go leaves the lock free for the next session"
        );
    }
}

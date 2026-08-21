//! Which of this laptop's sessions is currently serving the channel to a server.
//!
//! Two terminals into one box must not both start a pump — the second finds the
//! first one's socket live and is refused, and reports a failure for a channel
//! that is working perfectly. So exactly one session serves, and the others
//! stand by.
//!
//! **"Stand by", not "start nothing", and that is the whole of this file's
//! history.** This used to be a `Claim`: a `sessions/<pid>` marker per session,
//! a `kill -0` sweep, and one question asked once — *am I the first?* A session
//! that answered no started nothing and never asked again. So when the owner's
//! laptop-side process ended, the survivor sat there for the rest of its life
//! naming a socket path that was correct and unbound, with paste, image paste
//! and `xdg-open` all dead, while riabuild's own binary was running in that very
//! terminal and could have taken the channel over in a second. Two sessions and
//! a closed lid is not an exotic case; it is a Tuesday.
//!
//! Ownership is therefore a **lease** that is taken and given back, and every
//! session keeps asking for it until its shell exits.
//!
//! ## Why an `flock` and not the markers it replaces
//!
//! A pid in a file is a claim about a process that somebody else has to check,
//! and every way of checking it is wrong somewhere:
//!
//! - a marker outlives the process that wrote it, so it needs a sweep, and a
//!   sweep that runs only at startup cannot see an owner that dies later;
//! - `kill -0` on a recycled pid says "alive" about a process that is not the
//!   one meant, and the old file said out loud that it accepted that risk
//!   because a marker was only ever read at startup — it is a much worse trade
//!   once the answer decides whether a channel comes back;
//! - and an age cap, which is how `gh_session` covers recycling, cannot be used
//!   here: a remote session outliving a day is the normal case.
//!
//! An `flock` has none of those questions in it. The kernel holds it, and the
//! kernel drops it when the holding process exits — cleanly, on a `SIGKILL`,
//! or with the laptop's lid closed on it. There is nothing to sweep, nothing to
//! recycle, and no file whose contents can be stale. What "the owner has gone"
//! means is exactly "the lock is free", and finding that out is one syscall.

use crate::Remote;
use anyhow::{Context, Result};
use riabuild_paths::Paths;
use riabuild_paths::filelock::FileLock;
use std::path::{Path, PathBuf};

/// Where this laptop records who is serving the channel to `remote`.
///
/// Keyed by [`Remote::hash`], the same answer the SSH identity is filed under,
/// so two sessions to one server meet and two sessions to two servers do not.
pub(super) fn dir(paths: &dyn Paths, remote: &Remote) -> PathBuf {
    paths.root().join("channel-sessions").join(remote.hash())
}

/// The lock file inside it. One per server, never per session.
fn lock_file(dir: &Path) -> PathBuf {
    dir.join("owner.lock")
}

/// The right to serve the channel, held for as long as this value is alive.
///
/// Dropping it gives the channel back, which is what lets a standing-by sibling
/// pick it up. Nothing here has a `close()`: an owner that returns without
/// running one — a panic, a process killed outright — has to release the lease
/// too, and only the kernel can promise that.
pub(super) struct Lease {
    _lock: FileLock,
}

/// Takes the lease if it is free, and answers `None` if another session on this
/// laptop is serving the channel to this server.
///
/// Never waits. A session standing by is not queueing for a turn: it is asking
/// again in a moment, and it wants to be free in between to notice its own
/// shell exiting.
pub(super) async fn try_take(dir: &Path) -> Result<Option<Lease>> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;

    Ok(FileLock::try_acquire(&lock_file(dir))
        .await?
        .map(|lock| Lease { _lock: lock }))
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

    /// One server, one server: the second session is told to stand by rather
    /// than starting a pump the first one's socket would refuse.
    #[tokio::test]
    async fn only_one_session_serves_a_server_at_a_time() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dir = dir(&paths, &remote());

        let first = try_take(&dir)
            .await
            .expect("take")
            .expect("the first session serves");
        assert!(
            try_take(&dir).await.expect("take").is_none(),
            "a second pump finds the first one's socket live and is refused"
        );
        drop(first);
    }

    /// The bug this file was rewritten for. The owner's session ends; the
    /// sibling that has been standing by all along takes the channel over, and
    /// the developer's paste comes back with nothing typed anywhere.
    ///
    /// Before this, ownership was decided once at startup and a session that
    /// lost sat out the rest of its life with a dead channel and riabuild
    /// running right there in the terminal.
    #[tokio::test]
    async fn a_session_that_stood_by_takes_over_when_the_owner_lets_go() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dir = dir(&paths, &remote());

        let owner = try_take(&dir).await.expect("take").expect("owns");
        assert!(try_take(&dir).await.expect("take").is_none());

        // The owner's shell exits.
        drop(owner);

        assert!(
            try_take(&dir).await.expect("take").is_some(),
            "the channel has to be takeable the moment its owner has gone"
        );
    }

    /// One laptop, two boxes: each gets its own channel, so neither session may
    /// see the other's lease.
    #[tokio::test]
    async fn two_servers_are_leased_separately() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let other = Remote {
            name: "build-02".into(),
            host: "build-02.fly.dev".into(),
            ..remote()
        };

        let _first = try_take(&dir(&paths, &remote()))
            .await
            .expect("take")
            .expect("owns");
        assert!(
            try_take(&dir(&paths, &other))
                .await
                .expect("take")
                .is_some(),
            "a different server is a different channel"
        );
    }

    /// A lease directory riabuild cannot make is an error, never a silent
    /// "nobody is here" — a session that read a fault that way would start the
    /// second pump this file exists to prevent.
    #[tokio::test]
    async fn a_directory_that_cannot_be_created_is_an_error_rather_than_a_free_lease() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dir = dir(&paths, &remote());
        tokio::fs::create_dir_all(dir.parent().expect("parent"))
            .await
            .expect("mkdir");
        tokio::fs::write(&dir, "not a directory")
            .await
            .expect("write");

        assert!(try_take(&dir).await.is_err());
    }
}

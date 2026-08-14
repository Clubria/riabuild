//! One marker per live session, so two terminals into one server share a
//! channel instead of fighting over it.
//!
//! Without this, the second `riabuild remote build-01` starts its own agent and
//! its own connection, and the pump on the far end finds the first one's socket
//! already live and refuses it. The second terminal reports a failure for a
//! channel that is working perfectly, which reads as "paste randomly stopped
//! working".
//!
//! This mirrors `gh_session`'s `sessions/<pid>` markers and its `kill -0`
//! sweep, and is a second implementation rather than a call into that one:
//! `gh_session`'s helpers are `pub(super)` and are shaped around the runtime
//! directory a GitHub credential lives in, while what is counted here is a
//! **laptop-side** process holding a tunnel to one named server. Two
//! differences follow from that, both deliberate:
//!
//! - the markers live on the laptop, keyed by [`Remote::hash`], because the
//!   laptop is where the supervisor runs;
//! - there is **no age cap**. `gh_session` drops a marker older than a day even
//!   when its pid is alive, to cover pid recycling. That trade runs the wrong
//!   way here: a mosh session outliving a day is the normal case, and a second
//!   terminal that decides a live session is stale starts the second tunnel
//!   this file exists to prevent. A recycled pid costs at worst "no clipboard",
//!   which is the one failure the channel is allowed to have.

use crate::Remote;
use anyhow::{Context, Result};
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, RunOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where this laptop records who is holding a channel to `remote` open.
///
/// Keyed by [`Remote::hash`], the same answer the SSH identity is filed under,
/// so two sessions to one server meet and two sessions to two servers do not.
pub(super) fn dir(paths: &dyn Paths, remote: &Remote) -> PathBuf {
    paths.root().join("channel-sessions").join(remote.hash())
}

pub(super) struct Claim {
    marker: PathBuf,
    /// Whether this session found no live sibling and is therefore the one
    /// that has to start the agent and the tunnel.
    pub(super) owner: bool,
}

impl Claim {
    /// Sweeps the dead, counts the living, and records this process.
    ///
    /// The sweep happens *before* this session's own marker is written, so the
    /// count it produces is siblings only — a marker written first would make
    /// every session look like a sibling of itself and nothing would ever start
    /// a tunnel.
    pub(super) async fn open(
        dir: &Path,
        pid: u32,
        runner: Arc<dyn CommandRunner>,
    ) -> Result<Claim> {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        let live = sweep(dir, runner).await?;
        let marker = dir.join(pid.to_string());
        // The timestamp is diagnostic only — nothing above reads it. It is
        // written anyway so somebody looking at `channel-sessions/` by hand can
        // tell a session that started this morning from one left by a laptop
        // that never came back.
        tokio::fs::write(&marker, riabuild_paths::config::now_secs().to_string())
            .await
            .with_context(|| format!("writing {}", marker.display()))?;
        Ok(Claim {
            marker,
            owner: live == 0,
        })
    }

    /// Gives the claim back. Best-effort on purpose: this runs as the
    /// developer's shell returns, and a marker that outlives its process is
    /// exactly what the next session's sweep is for.
    pub(super) async fn close(self) {
        let _ = tokio::fs::remove_file(&self.marker).await;
    }
}

/// How many markers belong to a process that is still running, with the rest
/// removed on the way past.
///
/// A `read_dir` that fails for any reason other than "not there" is an error
/// rather than a zero. Reading it as zero would say "nobody holds a channel
/// here" on the strength of a transient IO fault, and the session that believed
/// it would start the second tunnel that takes a colleague's — or its own other
/// terminal's — paste away.
async fn sweep(dir: &Path, runner: Arc<dyn CommandRunner>) -> Result<usize> {
    let mut live = 0;
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", dir.display()));
        }
    };

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let Some(pid) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let running = runner
            .run("kill", &["-0", pid], &RunOptions::default())
            .await
            .map(|output| output.ok())
            .unwrap_or(false);
        if running {
            live += 1;
        } else {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    #[tokio::test]
    async fn the_first_session_owns_the_channel_and_the_second_only_joins_it() {
        // The failure this prevents: the second terminal starts its own
        // `ssh -R`, whose StreamLocalBindUnlink unlinks the socket the first
        // one is serving, and both developers' paste dies silently.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let alive = Arc::new(FakeRunner::new().with("kill -0", 0, "", ""));
        let dir = dir(&paths, &remote());

        let first = Claim::open(&dir, 1, alive.clone()).await.expect("open");
        assert!(first.owner, "the first session has to start the tunnel");

        let second = Claim::open(&dir, 2, alive.clone()).await.expect("open");
        assert!(
            !second.owner,
            "a second tunnel to the same server unlinks the first one's socket"
        );

        // …and once both have gone, a later session starts one again.
        first.close().await;
        second.close().await;
        let third = Claim::open(&dir, 3, alive).await.expect("open");
        assert!(third.owner);
    }

    #[tokio::test]
    async fn a_marker_left_by_a_laptop_that_never_came_back_is_swept() {
        // A shell killed with the lid closed runs no exit path at all, and a
        // marker nobody removed would mean the channel never came back.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dir = dir(&paths, &remote());
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        tokio::fs::write(dir.join("4242"), "0")
            .await
            .expect("write");

        let dead = Arc::new(FakeRunner::new().with("kill -0", 1, "", "No such process"));
        let claim = Claim::open(&dir, 7, dead).await.expect("open");

        assert!(claim.owner, "a dead session holds nothing open");
        assert!(
            !dir.join("4242").exists(),
            "a marker whose process is gone is removed, not left to wedge the channel shut"
        );
    }

    #[tokio::test]
    async fn two_servers_are_counted_separately() {
        // One laptop, two boxes: each gets its own agent and its own tunnel, so
        // neither may see the other's marker.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let alive = Arc::new(FakeRunner::new().with("kill -0", 0, "", ""));
        let other = Remote {
            name: "build-02".into(),
            host: "build-02.fly.dev".into(),
            ..remote()
        };

        let first = Claim::open(&dir(&paths, &remote()), 1, alive.clone())
            .await
            .expect("open");
        let second = Claim::open(&dir(&paths, &other), 2, alive)
            .await
            .expect("open");

        assert!(first.owner);
        assert!(second.owner, "a different server is a different channel");
    }

    #[tokio::test]
    async fn a_directory_that_cannot_be_read_is_an_error_rather_than_an_empty_count() {
        // Reading a fault as "nobody is here" is how a session talks itself
        // into starting the second tunnel.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dir = dir(&paths, &remote());
        tokio::fs::create_dir_all(dir.parent().expect("parent"))
            .await
            .expect("mkdir");
        // A file where the marker directory should be: `create_dir_all` fails
        // on it rather than quietly producing an empty listing.
        tokio::fs::write(&dir, "not a directory")
            .await
            .expect("write");

        let alive = Arc::new(FakeRunner::new().with("kill -0", 0, "", ""));
        assert!(
            Claim::open(&dir, 1, alive).await.is_err(),
            "a marker directory riabuild cannot use is not an empty one"
        );
    }
}

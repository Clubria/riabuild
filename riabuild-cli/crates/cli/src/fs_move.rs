//! Moving a directory tree, including onto another filesystem.
//!
//! A failed move must never be a lost checkout. Every path through here either
//! leaves the source exactly where it was, or has already verified that a
//! complete copy arrived somewhere else.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Moves `from` to `to`.
pub async fn move_tree(from: &Path, to: &Path) -> Result<()> {
    match tokio::fs::rename(from, to).await {
        Ok(()) => Ok(()),
        // The one failure worth retrying differently. `rename` cannot cross a
        // filesystem boundary, and moving a checkout onto an external drive or
        // a second volume is exactly what this exists for. Every other error —
        // permissions, a destination in use — would fail a copy too, and is
        // better reported as itself than as a slow copy failure.
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_then_delete(from, to).await
        }
        Err(error) => Err(error)
            .with_context(|| format!("could not move {} to {}", from.display(), to.display())),
    }
}

/// Copies the tree, checks it arrived, and only then removes the original.
pub async fn copy_then_delete(from: &Path, to: &Path) -> Result<()> {
    if let Err(error) = copy_tree(from, to).await {
        // Whatever arrived is a fragment of a checkout, and leaving it behind
        // would make the next attempt refuse to run against an occupied path.
        let _ = tokio::fs::remove_dir_all(to).await;
        return Err(error);
    }

    if let Err(error) = verify_arrival(from, to).await {
        let _ = tokio::fs::remove_dir_all(to).await;
        return Err(error);
    }

    tokio::fs::remove_dir_all(from).await.with_context(|| {
        format!(
            "copied {} to {} but could not remove the original",
            from.display(),
            to.display(),
        )
    })
}

/// Recursively copies `from` to `to`, recreating symlinks rather than
/// following them.
///
/// Following them would be wrong twice over: pnpm fills `node_modules` with
/// links into a virtual store, so following one duplicates the store into the
/// destination, and `fs::copy` fails outright on a link whose target has gone.
///
/// The walk carries its own worklist rather than recursing, because an async
/// function that calls itself has to box its future at every level.
pub async fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let mut pending: Vec<(PathBuf, PathBuf)> = vec![(from.to_path_buf(), to.to_path_buf())];

    while let Some((from, to)) = pending.pop() {
        let meta = tokio::fs::symlink_metadata(&from)
            .await
            .with_context(|| format!("could not read {}", from.display()))?;

        if meta.is_symlink() {
            let target = tokio::fs::read_link(&from).await?;
            tokio::fs::symlink(&target, &to)
                .await
                .with_context(|| format!("could not link {}", to.display()))?;
            continue;
        }

        if meta.is_file() {
            tokio::fs::copy(&from, &to)
                .await
                .with_context(|| format!("could not copy {}", from.display()))?;
            continue;
        }

        tokio::fs::create_dir_all(&to)
            .await
            .with_context(|| format!("could not create {}", to.display()))?;
        let mut reader = tokio::fs::read_dir(&from).await?;
        while let Some(entry) = reader.next_entry().await? {
            pending.push((entry.path(), to.join(entry.file_name())));
        }
    }

    Ok(())
}

/// What a tree contains, at every depth.
///
/// `bytes` is what makes a *short* file visible. A disk that fills part-way
/// through `fs::copy` leaves a file that exists and is truncated, so counting
/// files alone would still call that copy complete.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally {
    dirs: u64,
    files: u64,
    links: u64,
    bytes: u64,
}

/// Whether a complete copy of `from` arrived at `to`.
///
/// This used to compare the number of entries at the *top level* of each
/// directory, which is the one measurement a partial copy passes: `copy_tree`
/// creates the top-level names first and walks downwards, so a copy that ran
/// out of space deep inside `.git/objects` has every top-level name in place
/// and nothing under them. `remove_dir_all(from)` then deleted the original,
/// against a header promising it "has already verified that a complete copy
/// arrived somewhere else".
///
/// A recursive count and a total byte size is not a checksum and is not meant
/// to be — a copy that silently corrupted bytes is a broken filesystem, not a
/// case this can catch. What it does catch is the whole class this exists for:
/// a copy that stopped early, in the middle, or wrote a file short.
async fn verify_arrival(from: &Path, to: &Path) -> Result<()> {
    let (copied, arrived) = (tally(from).await?, tally(to).await?);
    if copied == arrived {
        return Ok(());
    }
    bail!(
        "the copy in {} is not complete — {} holds {} file(s) in {} director(ies) totalling {} \
         bytes, and {} of {} bytes arrived. {} was left alone",
        to.display(),
        from.display(),
        copied.files,
        copied.dirs,
        copied.bytes,
        arrived.files,
        arrived.bytes,
        from.display(),
    )
}

/// Walks `dir` and totals what is in it.
///
/// The walk carries its own worklist for the same reason `copy_tree`'s does:
/// an async function that calls itself has to box its future at every level.
/// Symlinks are counted, never followed — following one would double-count
/// whatever it points at, and `copy_tree` recreates them rather than
/// resolving them.
async fn tally(dir: &Path) -> Result<Tally> {
    let mut total = Tally::default();
    let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];

    while let Some(path) = pending.pop() {
        let mut reader = tokio::fs::read_dir(&path)
            .await
            .with_context(|| format!("could not read {}", path.display()))?;
        total.dirs += 1;
        while let Some(entry) = reader.next_entry().await? {
            let meta = tokio::fs::symlink_metadata(entry.path())
                .await
                .with_context(|| format!("could not read {}", entry.path().display()))?;
            if meta.is_symlink() {
                total.links += 1;
            } else if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total.files += 1;
                total.bytes += meta.len();
            }
        }
    }

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_tasks::testing::write_file;
    use tempfile::TempDir;

    /// A checkout-shaped tree: a `.git`, a file, and a nested directory.
    async fn checkout(root: &Path) {
        write_file(&root.join(".git/HEAD"), "ref: refs/heads/main\n").await;
        write_file(&root.join("README.md"), "hello\n").await;
        write_file(&root.join("src/main.rs"), "fn main() {}\n").await;
    }

    #[tokio::test]
    async fn renames_within_a_filesystem() {
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        checkout(&from).await;
        tokio::fs::create_dir_all(to.parent().unwrap())
            .await
            .unwrap();

        move_tree(&from, &to).await.unwrap();

        assert!(!from.exists());
        assert!(to.join(".git/HEAD").exists());
        assert_eq!(
            tokio::fs::read_to_string(to.join("README.md"))
                .await
                .unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn copies_a_nested_tree() {
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        checkout(&from).await;

        copy_tree(&from, &to).await.unwrap();

        assert!(from.exists(), "copy_tree must not remove the source");
        assert_eq!(
            tokio::fs::read_to_string(to.join("src/main.rs"))
                .await
                .unwrap(),
            "fn main() {}\n"
        );
    }

    #[tokio::test]
    async fn recreates_symlinks_instead_of_following_them() {
        // pnpm fills node_modules with symlinks into a virtual store. Following
        // them would copy the whole store into the destination.
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        checkout(&from).await;
        tokio::fs::create_dir_all(from.join("node_modules"))
            .await
            .unwrap();
        tokio::fs::symlink("../src", from.join("node_modules/app"))
            .await
            .unwrap();

        copy_tree(&from, &to).await.unwrap();

        let link = to.join("node_modules/app");
        assert!(
            tokio::fs::symlink_metadata(&link)
                .await
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            tokio::fs::read_link(&link).await.unwrap(),
            PathBuf::from("../src")
        );
    }

    #[tokio::test]
    async fn a_dangling_symlink_does_not_fail_the_copy() {
        // `fs::copy` on a link whose target has gone fails outright, and a
        // checkout that has ever had a dependency removed contains one.
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        checkout(&from).await;
        tokio::fs::symlink("nowhere", from.join("dangling"))
            .await
            .unwrap();

        copy_tree(&from, &to).await.unwrap();

        assert!(
            tokio::fs::symlink_metadata(to.join("dangling"))
                .await
                .unwrap()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn a_copy_that_stopped_deep_in_the_tree_is_not_accepted() {
        // The bug: verification counted top-level entries only, and
        // `copy_tree` creates the top-level names first. A copy that ran out
        // of space inside `.git/objects` therefore matched, and
        // `remove_dir_all(from)` deleted the original.
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("volume/hub");
        checkout(&from).await;
        write_file(&from.join(".git/objects/pack/all.pack"), "0123456789").await;
        copy_tree(&from, &to).await.unwrap();

        // A file that arrived short, which is what a full disk leaves behind.
        tokio::fs::write(to.join(".git/objects/pack/all.pack"), b"012")
            .await
            .unwrap();

        let error = verify_arrival(&from, &to)
            .await
            .expect_err("a truncated file is not a complete copy");
        assert!(format!("{error:#}").contains("not complete"), "{error:#}");
    }

    #[tokio::test]
    async fn a_file_that_never_arrived_is_not_accepted_either() {
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("volume/hub");
        checkout(&from).await;
        copy_tree(&from, &to).await.unwrap();
        tokio::fs::remove_file(to.join("src/main.rs"))
            .await
            .unwrap();

        assert!(verify_arrival(&from, &to).await.is_err());
    }

    #[tokio::test]
    async fn a_whole_copy_verifies() {
        // The other direction, so the check cannot be satisfied by refusing
        // everything — including the symlinks `copy_tree` recreates rather
        // than follows, whose targets must not be counted twice.
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("volume/hub");
        checkout(&from).await;
        tokio::fs::create_dir_all(from.join("node_modules"))
            .await
            .unwrap();
        tokio::fs::symlink("../src", from.join("node_modules/app"))
            .await
            .unwrap();
        copy_tree(&from, &to).await.unwrap();

        verify_arrival(&from, &to).await.expect("a complete copy");
    }

    #[tokio::test]
    async fn a_copy_from_a_missing_source_leaves_nothing_behind() {
        let home = TempDir::new().unwrap();
        let to = home.path().join("work/hub");

        assert!(
            copy_then_delete(&home.path().join("nope"), &to)
                .await
                .is_err()
        );
        assert!(!to.exists(), "a failed copy must not leave a partial tree");
    }

    #[tokio::test]
    async fn the_source_survives_until_the_copy_is_verified() {
        let home = TempDir::new().unwrap();
        let from = home.path().join("code/hub");
        let to = home.path().join("volume/hub");
        checkout(&from).await;

        copy_then_delete(&from, &to).await.unwrap();

        assert!(!from.exists());
        assert!(to.join(".git/HEAD").exists());
        assert!(to.join("src/main.rs").exists());
    }
}

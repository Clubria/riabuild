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

    let (copied, arrived) = (entries(from).await?, entries(to).await?);
    if copied != arrived {
        let _ = tokio::fs::remove_dir_all(to).await;
        bail!(
            "only {arrived} of {copied} entries arrived in {} — {} was left alone",
            to.display(),
            from.display(),
        );
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

/// How many things sit directly inside a directory.
async fn entries(dir: &Path) -> Result<usize> {
    let mut reader = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("could not read {}", dir.display()))?;
    let mut count = 0;
    while reader.next_entry().await?.is_some() {
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::write_file;
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

//! Putting an unpacked distribution where it belongs, without ever clearing
//! the path where it stands.
//!
//! `target` lives under `paths::tools_root()`, **shared** by every developer
//! with an account on a server, so the tree being replaced is one a colleague's
//! `pnpm dev` may be running out of. Everything here exists to make that
//! replacement atomic: unpack into a staging directory beside `target`, then
//! `rename` it into place, so a reader sees a complete tree or none — never a
//! half-emptied one. Every failure path takes the staging copy with it, because
//! it is a complete ~130 MB tree in a directory nothing anywhere sweeps.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Unpacks a `node-v*.tar.gz` into `target` so that `target/bin/node` is the
/// binary: Node wraps everything in one `node-v22.23.1-darwin-arm64/` directory.
pub fn extract_node_tarball(bytes: &[u8], target: &Path) -> Result<()> {
    extract_tarball(bytes, target, 1)
}

/// pnpm has no wrapper directory: the `pnpm` launcher and the `dist/` tree it
/// loads sit at the root of the archive, and must stay beside each other.
pub fn extract_pnpm_tarball(bytes: &[u8], target: &Path) -> Result<()> {
    extract_tarball(bytes, target, 0)
}

/// Unpacks into `target` without ever clearing it where it stands.
///
/// `target` lives under `paths::tools_root()`, **shared** by every developer
/// with an account on a server — so it is not ours to delete. This used to open
/// with `remove_dir_all(target)` on the strength of "`apply()` starts from
/// nothing", which held while `tools_root()` and `root()` were one directory on
/// one laptop and became a way to delete the Node a colleague's `pnpm dev` is
/// running out of the moment they stopped being.
///
/// So the archive is unpacked into a sibling directory named for this call and
/// `rename`d into place — `remote::install::write_binary`'s idiom, for its
/// reason: two developers installing one version at once is the ordinary case
/// on a shared box, not the exotic one. A reader sees a complete tree or none.
///
/// Judging whether what is already at `target` is any good is the *caller's*
/// job (`tasks::toolchain` asks the binary its version).
fn extract_tarball(bytes: &[u8], target: &Path, strip_components: usize) -> Result<()> {
    let staging = staging_beside(target, "part");
    // Only ever this call's own leftovers from an interrupted earlier run —
    // never another developer's staging directory, and never `target`.
    remove_tree(&staging).with_context(|| format!("could not clear {}", staging.display()))?;
    if let Err(error) = unpack(bytes, &staging, strip_components) {
        let _ = remove_tree(&staging);
        return Err(error);
    }
    swap_into_place(&staging, target)
}

/// Removes whatever is at `path`, and says so if it could not.
///
/// `symlink_metadata` rather than `metadata`: a symlink to a directory has to
/// be unlinked, not walked. Current std happens to do that for
/// `remove_dir_all` too, but that is its fallback for `ENOTDIR` rather than
/// anything this file arranged, and the set-aside path *can* be a symlink —
/// `swap_into_place` renames whatever was at `target`, symlink included. A path
/// that is already absent is success, not an error.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `…/node/.22.23.1.4171-3.part`, beside the tree it is about to become: the
/// same directory, so the same filesystem, so the `rename` that installs it is
/// atomic rather than a copy.
///
/// The counter is not decoration: keyed on `std::process::id()` alone, two
/// staging trees prepared at once inside one process compute the same path and
/// unpack over each other — the round-2 finding that made `host_key::pin` stop
/// doing this. Nothing reaches here twice concurrently today (`apply_with` runs
/// `ensure_node` and `ensure_pnpm` in sequence, against different targets), so
/// this closes the hazard rather than a bug.
fn staging_beside(target: &Path, tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let call = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{}-{call}.{tag}", std::process::id()))
}

/// Installs a finished staging tree at `target` with a `rename`, so nothing
/// ever observes a partial one.
fn swap_into_place(staging: &Path, target: &Path) -> Result<()> {
    // `symlink_metadata`, not `exists()`, which follows symlinks: a *dangling*
    // symlink at `target` read as "nothing is there", and the rename below
    // then failed with `ENOTDIR` on a path riabuild could simply have
    // replaced — a permanent hard failure whose message named the wrong thing.
    if std::fs::symlink_metadata(target).is_err() {
        return install_staged(staging, target, None);
    }

    // Something is there and the caller judged it unusable. It still gets moved
    // aside rather than emptied where it stands, so that every lookup through
    // `target` resolves to a whole tree — the old one or the new one, never a
    // half-emptied one. For the process already running out of it that buys
    // only the descriptors it holds open, since `remove_tree(&stale)` below
    // unlinks the rest; but unlinking in place would break its later `open`s
    // too, which is how a colleague's `pnpm dev` died mid-command.
    let stale = staging_beside(target, "stale");
    let _ = remove_tree(&stale);
    if let Err(error) = std::fs::rename(target, &stale) {
        // Nothing was installed, so nothing may be left staged either.
        let _ = remove_tree(staging);
        return Err(error).with_context(|| format!("could not move {} aside", target.display()));
    }
    install_staged(staging, target, Some(&stale))
}

/// The `rename` that installs, and what to do when it does not.
///
/// `set_aside` names the tree moved out of the way, if there was one, and is
/// what goes back if this fails. The staging tree does not survive this call on
/// any path: it is a complete ~130 MB copy, `tools/` is shared by every
/// developer on the box, and nothing anywhere sweeps it.
fn install_staged(staging: &Path, target: &Path, set_aside: Option<&Path>) -> Result<()> {
    let error = match std::fs::rename(staging, target) {
        Ok(()) => {
            if let Some(stale) = set_aside {
                let _ = remove_tree(stale);
            }
            return Ok(());
        }
        Err(error) => error,
    };
    let _ = remove_tree(staging);

    // A co-tenant installing the same version won the race between the check
    // above and this rename — they found `target` free (or vacated by us) and
    // filled it. Their tree arrived the way ours would have, so the outcome we
    // wanted, a whole tree of this version at this path, is the one on disk.
    // Failing here instead cost that developer a hard error over work that had
    // already succeeded. Only a real directory counts: a *file* there is not a
    // toolchain, and accepting one would report an install that cannot run.
    if std::fs::symlink_metadata(target).is_ok_and(|meta| meta.is_dir()) {
        if let Some(stale) = set_aside {
            let _ = remove_tree(stale);
        }
        return Ok(());
    }

    // Put back what was there rather than leaving the shared path empty.
    if let Some(stale) = set_aside {
        let _ = std::fs::rename(stale, target);
    }
    Err(error).with_context(|| format!("could not install {}", target.display()))
}

fn unpack(bytes: &[u8], target: &Path, strip_components: usize) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);

    std::fs::create_dir_all(target)?;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut components = path.components();
        for _ in 0..strip_components {
            components.next();
        }
        let relative: std::path::PathBuf = components.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        let destination = target.join(relative);
        // `unpack` will not create the directories above a file, and an
        // archive is not obliged to carry an entry for every directory it
        // uses. Both Node and pnpm happen to carry them today.
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn a_node_archive_loses_its_wrapper_directory() {
        let bytes = tarball(&[("node-v22.23.1-darwin-arm64/bin/node", b"binary")]);
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        extract_node_tarball(&bytes, &target).unwrap();
        assert!(target.join("bin/node").exists());
    }

    #[test]
    fn a_pnpm_archive_keeps_its_launcher_beside_the_dist_tree() {
        // pnpm's archive has no wrapper directory. Stripping one anyway would
        // throw the launcher away and leave a `dist/` nothing can start — and
        // the launcher loads `dist/` from beside itself, so the two cannot be
        // separated either.
        let bytes = tarball(&[("pnpm", b"launcher"), ("dist/pnpm.mjs", b"module")]);
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("11.11.0");
        extract_pnpm_tarball(&bytes, &target).unwrap();
        assert!(target.join("pnpm").exists());
        assert!(target.join("dist/pnpm.mjs").exists());
    }

    #[test]
    fn a_failed_extraction_leaves_a_tree_another_developer_is_using_alone() {
        // `tools_root()` is shared by every developer on a server, so the tree
        // being unpacked over is one a co-tenant's `pnpm dev` may be running
        // out of. This used to open with `remove_dir_all(target)` and extract
        // afterwards, so a truncated archive — or a process killed between the
        // two — left the colleague with nothing at all. Unpacking into a
        // pid-suffixed staging directory first costs a failure that directory
        // and nothing else.
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        std::fs::create_dir_all(target.join("bin")).unwrap();
        std::fs::write(target.join("bin/node"), b"the node ada is running").unwrap();

        extract_node_tarball(b"not a gzip stream at all", &target).expect_err("corrupt archive");

        assert_eq!(
            std::fs::read(target.join("bin/node")).unwrap(),
            b"the node ada is running"
        );
        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn replacing_a_tree_swaps_the_whole_thing_rather_than_unpacking_over_it() {
        // The other half: when the caller *has* judged what is there unusable,
        // the new tree arrives whole. A file the archive does not carry cannot
        // survive as a leftover from the old one, which is what unpacking into
        // a live directory would leave behind — and no staging directory is
        // left lying beside it either.
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        std::fs::create_dir_all(target.join("bin")).unwrap();
        std::fs::write(target.join("bin/node"), b"a broken node").unwrap();
        std::fs::write(target.join("bin/leftover"), b"from the old tree").unwrap();

        let bytes = tarball(&[("node-v22.23.1-darwin-arm64/bin/node", b"a working node")]);
        extract_node_tarball(&bytes, &target).unwrap();

        assert_eq!(
            std::fs::read(target.join("bin/node")).unwrap(),
            b"a working node"
        );
        assert!(!target.join("bin/leftover").exists());
        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            Vec::<String>::new()
        );
    }

    /// A directory holding `bin/node` with these contents.
    fn tree(path: &std::path::Path, contents: &[u8]) {
        std::fs::create_dir_all(path.join("bin")).unwrap();
        std::fs::write(path.join("bin/node"), contents).unwrap();
    }

    #[test]
    fn losing_the_install_race_to_a_co_tenant_is_not_a_failure() {
        // Two developers on one server, both having judged the shared tree
        // stale. P1 moves it aside; P2, finding nothing at `target`, installs
        // its own there; P1's rename then fails with ENOTEMPTY. P1 used to get
        // a hard error over a version that *is* installed, and both trees —
        // P1's staging copy and the one it set aside — leaked into a shared
        // directory nothing ever sweeps.
        //
        // The interleaving itself is not schedulable inside one process, so
        // what is built here is the state it leaves: our staging tree, the
        // tree we set aside, and a co-tenant's complete tree back at `target`.
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        let staging = home.path().join(".22.23.1.part");
        let stale = home.path().join(".22.23.1.stale");
        tree(&staging, b"ours, ready to install");
        tree(&stale, b"the tree we judged unusable");
        tree(
            &target,
            b"a co-tenant's, installed while we were moving ours aside",
        );

        install_staged(&staging, &target, Some(&stale)).expect("a lost race is not a failure");

        assert_eq!(
            std::fs::read(target.join("bin/node")).unwrap(),
            b"a co-tenant's, installed while we were moving ours aside"
        );
        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            Vec::<String>::new(),
            "~130 MB per lost race, in a directory nothing sweeps"
        );
    }

    #[test]
    fn what_landed_at_the_target_has_to_be_a_tree_before_it_counts_as_a_win() {
        // The same lost race, except that what appeared at `target` is a file.
        // Accepting it would report an installed toolchain that cannot run, so
        // this is a failure — and the staging copy still must not survive it.
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        let staging = home.path().join(".22.23.1.part");
        let stale = home.path().join(".22.23.1.stale");
        tree(&staging, b"ours, ready to install");
        tree(&stale, b"the tree we set aside");
        std::fs::write(&target, b"not a directory").unwrap();

        install_staged(&staging, &target, Some(&stale)).expect_err("a file is not a toolchain");

        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            // The set-aside tree stays: with something squatting at `target`
            // riabuild cannot put it back, and it is the last copy of what was
            // installed — deleting it too would be the destructive answer.
            // `.22.23.1.part`, the ~130 MB this developer just unpacked, is
            // what must be gone.
            vec![".22.23.1.stale".to_string()]
        );
        assert_eq!(
            std::fs::read(stale.join("bin/node")).unwrap(),
            b"the tree we set aside"
        );
    }

    #[test]
    fn a_dangling_symlink_where_the_tree_goes_is_replaced_rather_than_failed_on() {
        // `exists()` follows symlinks, so a dangling one read as "nothing is
        // there" — and the rename then failed with ENOTDIR while the retry
        // saw `exists()` false again, so it became a permanent hard failure
        // with a message naming the wrong problem, plus a leaked staging tree.
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        std::os::unix::fs::symlink(home.path().join("gone"), &target).unwrap();

        let bytes = tarball(&[("node-v22.23.1-darwin-arm64/bin/node", b"a working node")]);
        extract_node_tarball(&bytes, &target).expect("a dangling link is not a tree");

        assert_eq!(
            std::fs::read(target.join("bin/node")).unwrap(),
            b"a working node"
        );
        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_live_symlink_where_the_tree_goes_leaves_nothing_set_aside_behind() {
        // The other symlink edge, and this one is a pin rather than a repair:
        // the round-3 review expected `remove_dir_all` to fail with ENOTDIR on
        // the symlink moved aside and leak it, but current std unlinks a
        // symlink here instead of walking it (checked directly, not assumed),
        // so this passed before `remove_tree` existed too. It stays because
        // `remove_tree` is what makes that outcome the file's own decision
        // rather than a std fallback, and because what the link *pointed at*
        // must survive untouched — it is not riabuild's to delete.
        let home = tempfile::TempDir::new().unwrap();
        let elsewhere = home.path().join("elsewhere");
        tree(&elsewhere, b"someone else's node");
        let target = home.path().join("22.23.1");
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();

        let bytes = tarball(&[("node-v22.23.1-darwin-arm64/bin/node", b"a working node")]);
        extract_node_tarball(&bytes, &target).expect("replaces the link with a real tree");

        assert_eq!(
            std::fs::read(target.join("bin/node")).unwrap(),
            b"a working node"
        );
        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            vec!["elsewhere".to_string()],
            "the set-aside symlink must not survive"
        );
    }

    #[test]
    fn two_staging_names_in_one_process_never_collide() {
        // Keyed on `std::process::id()` alone, these were the same path, and
        // two trees prepared at once in one process would unpack over each
        // other — the round-2 finding `host_key::pin` was restructured for.
        let target = std::path::Path::new("/tools/node/22.23.1");
        assert_ne!(
            staging_beside(target, "part"),
            staging_beside(target, "part")
        );
    }

    /// Everything in `dir` other than `keep` — the staging and set-aside
    /// directories, if any survived.
    fn leftovers_beside(dir: &std::path::Path, keep: &str) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != keep)
            .collect();
        found.sort();
        found
    }
}

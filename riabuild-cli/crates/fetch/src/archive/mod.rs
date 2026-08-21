//! Unpacking what riabuild downloads.
//!
//! Split out of `download.rs`, which fetches and verifies: by the time anything
//! here runs, the bytes have already been checked against a published digest.
//! Nothing in this file is async. Extraction is CPU work over an in-memory
//! buffer written through the synchronous `tar` and `zip` crates, so wrapping
//! the directory calls around it in `tokio::fs` would be theatre.
//!
//! Two shapes are needed, because upstream projects disagree about both:
//!
//! - **whole archive**, for Node and pnpm, which ship a tree riabuild keeps
//! - **one member**, for `gh` and `infisical`, which ship a binary alongside
//!   manpages, completions, and a licence riabuild has no use for
//!
//! And one project ships no container at all — see [`Kind::Raw`].
//!
//! Three files. `kind` is which container an asset arrived in, `member` is
//! lifting one named file out of one, and `staging` is how a finished tree
//! lands. What is left here is the async surface every caller reaches through,
//! and the guards — a path that may not leave its target, a disk that would
//! not take it.

mod kind;
mod member;

// Re-exported so a caller keeps naming `archive::Kind` and
// `archive::extract_single_file`. Which file each lives in is this module's
// business, and a caller that had to know would have to be edited the next
// time one moves.
pub use kind::Kind;
pub use member::extract_single_file;

use crate::{Failure, UPSTREAM_MOVED};
use anyhow::Result;
use member::{tar_member, zip_member};
use std::path::{Component, Path, PathBuf};

mod staging;

// The tarball extractors live in `staging` because *how* a tree lands matters
// as much as what is in it: `tools_root()` is shared by every developer with an
// account on a server, so a replacement has to be atomic rather than a delete
// followed by an unpack. This module used to carry a simpler pair that opened
// with `remove_dir_all(target)` — correct on a single-user laptop, and a way to
// delete the Node a colleague's `pnpm dev` is running out of anywhere else.
// They are reached through the `async` wrappers below and never directly.
// The same pair lands the one file `extract_member` writes. A binary is not a
// tree, but *how* it arrives is the same question and must not grow a second
// answer beside this one.
use staging::{install_staged, staging_beside};

/// Runs one unpacking job on tokio's blocking pool.
///
/// Unpacking *looks* like the CPU work `../../CLAUDE.md` exempts — the `tar`
/// crate reading an in-memory buffer — and it is not only that. Every entry is
/// a `create_dir_all` and a write, and landing the result is a `rename` over a
/// path under `tools_root()` plus a `remove_dir_all` of the tree it displaced:
/// ~130 MB of filesystem work against a shared directory, none of which that
/// exemption covers. Left on the reactor thread it stalls every other future in
/// the process for as long as the disk takes.
///
/// `spawn_blocking` rather than `block_in_place`: riabuild runs on a
/// current-thread runtime, where `block_in_place` is not available. Everything
/// the job touches is owned, because as far as the borrow checker is concerned
/// a blocking task outlives the future that spawned it.
///
/// A `JoinError` here can only be the job panicking — nothing cancels it — and
/// that is a bug in riabuild rather than something a developer can act on, so
/// it travels as the `anyhow` chain it is instead of wearing a `Failure`'s
/// remedy.
async fn off_the_reactor<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work).await?
}

/// Unpacks a `node-v*.tar.gz` into `target` so that `target/bin/node` is the
/// binary: Node wraps everything in one `node-v22.23.1-darwin-arm64/` directory.
pub async fn extract_node_tarball(bytes: Vec<u8>, target: PathBuf) -> Result<()> {
    off_the_reactor(move || staging::extract_node_tarball(&bytes, &target)).await
}

/// Unpacks the npm packages one tool is made of into `target` as a single
/// tree, stripping the `package/` directory npm wraps every tarball in.
///
/// pnpm is the caller and is one package — see `download::PNPM_PACKAGE`. More
/// than one is still accepted, and more than one is still meaningful: they are
/// unpacked in the order given, later entries overwriting earlier ones, and the
/// result lands in one `rename` so a co-tenant never reaches a half-assembled
/// tree.
pub async fn extract_npm_tarballs(parts: Vec<Vec<u8>>, target: PathBuf) -> Result<()> {
    off_the_reactor(move || staging::extract_npm_tarballs(&parts, &target)).await
}

/// Writes one file out of an archive to `destination`, executable.
///
/// `member` is matched against the *end* of each archived path, because the
/// prefix is usually the asset name and therefore carries the version:
/// `gh_2.97.0_macOS_arm64/bin/gh` is found by asking for `bin/gh`. Matching on
/// a full path would mean rebuilding the prefix from the version at every call
/// site, and getting it wrong would be a 404's quieter cousin — an archive that
/// downloaded and verified fine and yielded nothing.
///
/// The mode is set here rather than taken from the archive. Every caller wants
/// an executable, and a zip written on a machine that does not model the Unix
/// permission bits stores 0 for all of them.
///
/// It **lands by rename**, for `staging`'s reason rather than a second one of
/// its own. `install` reinstalls a version that is already there — `apply()` is
/// safe to run twice, and re-running it is the ordinary repair — so
/// `destination` is routinely a `gh`, `infisical`, `ngrok` or `grok` binary
/// that exists. Writing over it in place truncates a file a co-tenant may be
/// executing, which is `ETXTBSY` if riabuild is lucky and a half-written binary
/// that passes the next existence check if it is not. Written beside it and
/// renamed on, the running process keeps the inode it opened and every later
/// lookup finds a whole binary.
///
/// Owned arguments and a hop onto the blocking pool, for [`off_the_reactor`]'s
/// reason: reading the member is CPU work over a buffer, and landing it is not.
pub async fn extract_member(
    bytes: Vec<u8>,
    kind: Kind,
    member: &'static str,
    destination: PathBuf,
) -> Result<()> {
    off_the_reactor(move || extract_member_blocking(&bytes, kind, member, &destination)).await
}

/// The body, on the blocking pool.
fn extract_member_blocking(
    bytes: &[u8],
    kind: Kind,
    member: &str,
    destination: &Path,
) -> Result<()> {
    let found = match kind {
        Kind::TarGz => tar_member(bytes, member)?,
        Kind::Zip => zip_member(bytes, member)?,
        // Nothing to look inside, and nothing to look for: `member` names where
        // the binary lands under `~/.riabuild/<tool>/<version>/`, which is the
        // caller's business rather than this function's.
        Kind::Raw => Some(bytes.to_vec()),
    };
    let Some(found) = found else {
        return Err(Failure::new(
            format!(
                "installing {member} — the archive riabuild downloaded and verified does not \
                 contain it, so nothing was installed"
            ),
            UPSTREAM_MOVED,
        )
        .into());
    };

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| cannot_write(parent, &error))?;
    }
    let staging = staging_beside(destination, "part");
    std::fs::write(&staging, found).map_err(|error| cannot_write(&staging, &error))?;
    if let Err(error) = set_executable(&staging) {
        let _ = std::fs::remove_file(&staging);
        return Err(error);
    }
    install_staged(&staging, destination, None)
}

/// A disk riabuild could not write to: no space, no permission, a read-only
/// mount. Every one of those is something the developer can see and fix, and
/// none of them is a bug in riabuild.
fn cannot_write(path: &Path, error: &std::io::Error) -> anyhow::Error {
    Failure::new(
        format!("writing {}", path.display()),
        "Check that there is free disk space and that you can write to that directory, then run \
         `riabuild` again.",
    )
    .detail(format!("{error}"))
    .into()
}

/// A buffer that matched its published digest and is still not an archive.
///
/// One sentence for gzip, tar and zip alike, because the developer's position
/// is the same in all three and there is nothing in it they can change: the
/// bytes are the ones upstream published, and riabuild cannot read them.
pub(super) fn unreadable(error: &dyn std::fmt::Display) -> anyhow::Error {
    Failure::new(
        "unpacking a download that matched the checksum published for it and is still not a \
         readable archive",
        UPSTREAM_MOVED,
    )
    .detail(format!("{error}"))
    .into()
}

/// Joins an archived path onto a destination, refusing anything that would
/// land outside it.
///
/// The archives riabuild extracts are verified against a published digest
/// first, so this is not what stands between a developer and a hostile tarball.
/// It is here because "the digest was right" is the wrong thing for path
/// handling to rest on: it makes every future caller's safety depend on a check
/// somewhere else in the program, and the day one of these is extracted without
/// a digest, nothing says so.
fn safe_join(target: &Path, relative: &Path) -> Result<PathBuf> {
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Failure::new(
                    format!(
                        "unpacking an archive into {} — it contains an entry that would be \
                         written outside it, so riabuild refused to unpack it",
                        target.display()
                    ),
                    "Send this to your team lead — the archive riabuild downloaded is not the \
                     one it expected, and nothing has been installed.",
                )
                .detail(format!("the entry is {}", relative.display()))
                .into());
            }
        }
    }
    Ok(target.join(relative))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// The async form, for files riabuild writes itself rather than unpacks.
#[cfg(unix)]
pub async fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = tokio::fs::metadata(path).await?.permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(path, permissions).await?;
    Ok(())
}

#[cfg(not(unix))]
pub async fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

    fn zipball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (path, contents) in entries {
            writer.start_file(*path, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[tokio::test]
    async fn a_node_archive_loses_its_wrapper_directory() {
        let bytes = tarball(&[("node-v22.23.1-darwin-arm64/bin/node", b"binary")]);
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        extract_node_tarball(bytes, target.clone()).await.unwrap();
        assert!(target.join("bin/node").exists());
    }

    #[tokio::test]
    async fn an_npm_package_loses_its_package_wrapper() {
        // Every npm tarball wraps its contents in `package/`, and what pnpm
        // needs out the other side is `bin/pnpm.cjs` with `dist/` beside it.
        let package = tarball(&[
            ("package/bin/pnpm.cjs", b"the entry" as &[u8]),
            ("package/dist/pnpm.mjs", b"module"),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("11.11.0");

        extract_npm_tarballs(vec![package], target.clone())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(target.join("bin/pnpm.cjs")).unwrap(),
            b"the entry"
        );
        assert!(target.join("dist/pnpm.mjs").exists());
        assert!(!target.join("package").exists());
    }

    #[tokio::test]
    async fn several_npm_packages_land_as_one_tree_with_the_last_one_winning() {
        // The multi-part contract, kept because it is the only thing that makes
        // "a co-tenant never sees half a tool" true of a tool that needs two
        // packages. pnpm needed exactly this while riabuild installed its
        // platform executable, and the ordering rule is the half that is easy
        // to lose: `tar` unlinks and recreates rather than writing in place, so
        // the later layer wins cleanly.
        let first = tarball(&[
            ("package/shared", b"replace me" as &[u8]),
            ("package/only-in-first", b"kept"),
        ]);
        let second = tarball(&[("package/shared", b"the winner" as &[u8])]);
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("11.11.0");

        extract_npm_tarballs(vec![first, second], target.clone())
            .await
            .unwrap();

        assert_eq!(std::fs::read(target.join("shared")).unwrap(), b"the winner");
        assert_eq!(
            std::fs::read(target.join("only-in-first")).unwrap(),
            b"kept"
        );
    }

    #[test]
    fn an_entry_that_climbs_out_of_the_target_is_refused() {
        // Asserted against `safe_join` rather than a crafted archive, because
        // `tar::Builder` refuses to *write* an entry containing `..` at all —
        // which is itself the outer layer of this defence. The guard exists so
        // that path handling does not rest on the digest check and on the tar
        // crate's own opinion, both of which live somewhere else in the program.
        let target = Path::new("/home/ada/.riabuild/pnpm/11.11.0");
        assert!(safe_join(target, Path::new("dist/pnpm.mjs")).is_ok());

        for escape in ["../../etc/profile", "/etc/profile", "dist/../../../x"] {
            let error = safe_join(target, Path::new(escape))
                .expect_err(escape)
                .to_string();
            assert!(error.contains("outside"), "{escape}: {error}");
        }
    }

    #[tokio::test]
    async fn finds_a_binary_under_the_versioned_prefix_gh_uses() {
        // The prefix carries the version, which is why members are matched by
        // suffix rather than by full path.
        let bytes = tarball(&[
            ("gh_2.97.0_linux_amd64/LICENSE", b"licence" as &[u8]),
            ("gh_2.97.0_linux_amd64/share/man/man1/gh-api.1", b"manpage"),
            ("gh_2.97.0_linux_amd64/bin/gh", b"the binary"),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let destination = home.path().join("gh/2.97.0/bin/gh");
        extract_member(bytes, Kind::TarGz, "bin/gh", destination.clone())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"the binary");
    }

    #[tokio::test]
    async fn finds_a_binary_at_the_archive_root_without_matching_its_neighbours() {
        // Infisical's tarball has the binary at the root, beside completions
        // and manpages that all begin with the same word.
        let bytes = tarball(&[
            ("completions/infisical.bash", b"completions" as &[u8]),
            ("manpages/infisical.1.gz", b"manpage"),
            ("README.md", b"readme"),
            ("infisical", b"the binary"),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let destination = home.path().join("infisical/0.43.120/infisical");
        extract_member(bytes, Kind::TarGz, "infisical", destination.clone())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"the binary");
    }

    #[tokio::test]
    async fn reads_a_member_out_of_a_zip_the_way_gh_ships_macos() {
        let bytes = zipball(&[
            ("gh_2.97.0_macOS_arm64/LICENSE", b"licence" as &[u8]),
            ("gh_2.97.0_macOS_arm64/bin/gh", b"the binary"),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let destination = home.path().join("gh/2.97.0/bin/gh");
        extract_member(bytes, Kind::Zip, "bin/gh", destination.clone())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"the binary");
    }

    #[tokio::test]
    async fn an_extracted_binary_is_executable() {
        // Without this the install completes, the check runs the binary, and
        // the developer is told `gh` is missing on a machine where it is not.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bytes = tarball(&[("infisical", b"the binary")]);
            let home = tempfile::TempDir::new().unwrap();
            let destination = home.path().join("infisical");
            extract_member(bytes, Kind::TarGz, "infisical", destination.clone())
                .await
                .unwrap();
            let mode = std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn reinstalling_replaces_the_binary_rather_than_writing_over_it() {
        // `install` reinstalls a version that is already there — `apply()` is
        // safe to run twice and re-running it is the ordinary repair — so this
        // path routinely lands on a `gh` or `infisical` a co-tenant on a shared
        // box is executing out of `tools_root()`. `std::fs::write` truncates
        // that file in place: ETXTBSY if riabuild is lucky, and a half-written
        // binary that passes the next existence check if it is not.
        //
        // The old inode stands in for the running process here, held by a hard
        // link. A rename leaves it alone; a write through the path does not.
        let home = tempfile::TempDir::new().unwrap();
        let destination = home.path().join("gh/2.97.0/bin/gh");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"the gh a colleague is running").unwrap();
        let held_open = home.path().join("still-running");
        std::fs::hard_link(&destination, &held_open).unwrap();

        let bytes = tarball(&[("gh_2.97.0_linux_amd64/bin/gh", b"a newly downloaded gh")]);
        extract_member(bytes, Kind::TarGz, "bin/gh", destination.clone())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"a newly downloaded gh"
        );
        assert_eq!(
            std::fs::read(&held_open).unwrap(),
            b"the gh a colleague is running",
            "the binary a co-tenant is executing was written over in place"
        );

        // And the staging copy is not left lying beside it: `tools_root()` is
        // shared and nothing anywhere sweeps it.
        let mut beside: Vec<String> = std::fs::read_dir(destination.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        beside.sort();
        assert_eq!(beside, vec!["gh".to_string()]);
    }

    #[tokio::test]
    async fn a_missing_member_says_nothing_was_installed() {
        // The failure mode this guards is an upstream rename: the download
        // succeeds, the digest matches, and the binary never appears.
        let bytes = tarball(&[("gh_2.97.0_linux_amd64/LICENSE", b"licence" as &[u8])]);
        let home = tempfile::TempDir::new().unwrap();
        let error = extract_member(bytes, Kind::TarGz, "bin/gh", home.path().join("gh"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("bin/gh"), "{error}");
        assert!(error.contains("nothing was installed"), "{error}");
    }

    #[tokio::test]
    async fn every_unpacking_failure_is_one_a_developer_can_act_on() {
        // `main` downcasts to `Failure` and prints "Send this to your team lead
        // — it is a bug in riabuild" for anything else. Unpacking is where an
        // upstream rename, a hostile archive and a full disk all surface, and
        // none of those is a bug in riabuild.
        let home = tempfile::TempDir::new().unwrap();
        let errors = vec![
            Kind::of("gh_2.97.0_macOS_universal.pkg").expect_err("container"),
            extract_member(
                tarball(&[("gh_2.97.0_linux_amd64/LICENSE", b"licence" as &[u8])]),
                Kind::TarGz,
                "bin/gh",
                home.path().join("gh"),
            )
            .await
            .expect_err("member"),
            extract_single_file(b"not a gzip stream", "riabuild").expect_err("single file"),
            safe_join(home.path(), Path::new("../../etc/profile")).expect_err("traversal"),
        ];
        for error in errors {
            let failure = error
                .downcast_ref::<riabuild_ui::Failure>()
                .unwrap_or_else(|| panic!("not a Failure: {error:#}"));
            assert!(!failure.attempting.is_empty(), "{failure}");
            assert!(!failure.action.is_empty(), "{failure}");
        }
    }

    #[tokio::test]
    async fn a_raw_download_is_written_through_byte_for_byte() {
        // xAI serves an uncompressed executable, so what riabuild verified and
        // what it writes have to be the same bytes — a repack in between would
        // make the pinned digest describe riabuild's own output rather than
        // upstream's.
        let payload = b"\x7fELF not really a binary, but the bytes are the point";
        let home = tempfile::TempDir::new().unwrap();
        let destination = home.path().join("grok").join("1.0.5").join("grok");

        extract_member(payload.to_vec(), Kind::Raw, "grok", destination.clone())
            .await
            .expect("extract");

        assert_eq!(std::fs::read(&destination).unwrap(), payload);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode();
            // A download that is not executable reads as "grok is not
            // installed" on a laptop, not as a test failure here.
            assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
        }
    }
}

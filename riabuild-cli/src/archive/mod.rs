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

use anyhow::{Context, Result, anyhow};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// The container an upstream release happens to use.
///
/// `gh` publishes Linux as tar.gz and macOS as zip — there is no macOS tar.gz,
/// and the only other macOS asset is a `.pkg` installer that writes to
/// `/usr/local` with sudo. So the container is a property of the asset rather
/// than of riabuild, and both have to be supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    TarGz,
    Zip,
}

impl Kind {
    /// Picked from the asset name, so it stays wrong-proof when a new asset is
    /// added: the name and the container cannot drift apart.
    pub fn of(asset: &str) -> Result<Self> {
        if asset.ends_with(".tar.gz") || asset.ends_with(".tgz") {
            Ok(Kind::TarGz)
        } else if asset.ends_with(".zip") {
            Ok(Kind::Zip)
        } else {
            Err(anyhow!("riabuild does not know how to unpack {asset}"))
        }
    }
}

mod staging;

// The tarball extractors live in `staging` because *how* a tree lands matters
// as much as what is in it: `tools_root()` is shared by every developer with an
// account on a server, so a replacement has to be atomic rather than a delete
// followed by an unpack. This module used to carry a simpler pair that opened
// with `remove_dir_all(target)` — correct on a single-user laptop, and a way to
// delete the Node a colleague's `pnpm dev` is running out of anywhere else.
pub use staging::{extract_node_tarball, extract_pnpm_tarball};

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
pub fn extract_member(bytes: &[u8], kind: Kind, member: &str, destination: &Path) -> Result<()> {
    let found = match kind {
        Kind::TarGz => tar_member(bytes, member)?,
        Kind::Zip => zip_member(bytes, member)?,
    };
    let found = found.ok_or_else(|| {
        anyhow!(
            "the archive riabuild downloaded does not contain {member}, so nothing was installed"
        )
    })?;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(destination, found)
        .with_context(|| format!("could not write {}", destination.display()))?;
    set_executable(destination)?;
    Ok(())
}

fn tar_member(bytes: &[u8], member: &str) -> Result<Option<Vec<u8>>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        if !is_member(&path.to_string_lossy(), member) {
            continue;
        }
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents)?;
        return Ok(Some(contents));
    }
    Ok(None)
}

fn zip_member(bytes: &[u8], member: &str) -> Result<Option<Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .context("that download is not a readable zip archive")?;

    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        // `name()` is the raw archived path. It is only compared here, never
        // joined onto a destination, so a hostile entry cannot escape anywhere
        // — `extract_member` writes to the path its caller chose.
        if !is_member(file.name(), member) {
            continue;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        return Ok(Some(contents));
    }
    Ok(None)
}

/// Whether an archived path names the member being looked for.
///
/// Anchored to a path boundary so asking for `infisical` finds the binary at
/// the archive root without also matching `completions/infisical.bash` or
/// `manpages/infisical.1.gz`, both of which are in the same tarball.
fn is_member(path: &str, member: &str) -> bool {
    let path = path.trim_start_matches("./");
    path == member || path.ends_with(&format!("/{member}"))
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
                return Err(anyhow!(
                    "that archive contains an entry that would be written outside \
                     {} ({}), so riabuild refused to unpack it",
                    target.display(),
                    relative.display()
                ));
            }
        }
    }
    Ok(target.join(relative))
}

/// One named member of a gzipped tarball, returned in memory.
///
/// The sibling of [`extract_member`], for the one caller that wants bytes
/// rather than a file: the release tarball holds `riabuild` at its root, and
/// those bytes go straight down an SSH pipe to a server rather than landing on
/// this machine at all. Writing them out only to read them back would put a
/// second copy of the binary on the laptop for no reason.
pub fn extract_single_file(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let matches = path.file_name().is_some_and(|found| found == name);
        if matches {
            let mut buffer = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buffer)?;
            return Ok(buffer);
        }
    }
    anyhow::bail!("{name} is not in that archive")
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

    #[test]
    fn a_single_member_is_lifted_out_of_a_tarball() {
        // Built in memory, so the test needs no fixture file and no network.
        let mut archive = tar::Builder::new(Vec::new());
        let payload = b"\x7fELF fake binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "riabuild", &payload[..])
            .expect("append");
        let tar_bytes = archive.into_inner().expect("finish");

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar_bytes).expect("gzip");
        let gz = encoder.finish().expect("gzip");

        assert_eq!(
            extract_single_file(&gz, "riabuild").expect("extract"),
            payload
        );
        assert!(extract_single_file(&gz, "not-there").is_err());
    }

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

    #[test]
    fn the_container_is_read_off_the_asset_name() {
        assert_eq!(
            Kind::of("gh_2.97.0_linux_amd64.tar.gz").unwrap(),
            Kind::TarGz
        );
        assert_eq!(Kind::of("gh_2.97.0_macOS_arm64.zip").unwrap(), Kind::Zip);
        assert!(Kind::of("gh_2.97.0_macOS_universal.pkg").is_err());
    }

    #[test]
    fn finds_a_binary_under_the_versioned_prefix_gh_uses() {
        // The prefix carries the version, which is why members are matched by
        // suffix rather than by full path.
        let bytes = tarball(&[
            ("gh_2.97.0_linux_amd64/LICENSE", b"licence" as &[u8]),
            ("gh_2.97.0_linux_amd64/share/man/man1/gh-api.1", b"manpage"),
            ("gh_2.97.0_linux_amd64/bin/gh", b"the binary"),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let destination = home.path().join("gh/2.97.0/bin/gh");
        extract_member(&bytes, Kind::TarGz, "bin/gh", &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"the binary");
    }

    #[test]
    fn finds_a_binary_at_the_archive_root_without_matching_its_neighbours() {
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
        extract_member(&bytes, Kind::TarGz, "infisical", &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"the binary");
    }

    #[test]
    fn reads_a_member_out_of_a_zip_the_way_gh_ships_macos() {
        let bytes = zipball(&[
            ("gh_2.97.0_macOS_arm64/LICENSE", b"licence" as &[u8]),
            ("gh_2.97.0_macOS_arm64/bin/gh", b"the binary"),
        ]);
        let home = tempfile::TempDir::new().unwrap();
        let destination = home.path().join("gh/2.97.0/bin/gh");
        extract_member(&bytes, Kind::Zip, "bin/gh", &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"the binary");
    }

    #[test]
    fn an_extracted_binary_is_executable() {
        // Without this the install completes, the check runs the binary, and
        // the developer is told `gh` is missing on a machine where it is not.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bytes = tarball(&[("infisical", b"the binary")]);
            let home = tempfile::TempDir::new().unwrap();
            let destination = home.path().join("infisical");
            extract_member(&bytes, Kind::TarGz, "infisical", &destination).unwrap();
            let mode = std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
        }
    }

    #[test]
    fn a_missing_member_says_nothing_was_installed() {
        // The failure mode this guards is an upstream rename: the download
        // succeeds, the digest matches, and the binary never appears.
        let bytes = tarball(&[("gh_2.97.0_linux_amd64/LICENSE", b"licence" as &[u8])]);
        let home = tempfile::TempDir::new().unwrap();
        let error = extract_member(&bytes, Kind::TarGz, "bin/gh", &home.path().join("gh"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("bin/gh"), "{error}");
        assert!(error.contains("nothing was installed"), "{error}");
    }
}

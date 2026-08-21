//! Lifting one named file out of a container.
//!
//! Reading only: everything here returns bytes in memory and writes nothing.
//! Where those bytes then go — a destination chosen by the caller, landed by
//! rename — is the parent module's, and the split is what keeps "find it" and
//! "install it" from becoming one function with two jobs.

use super::unreadable;
use crate::{Failure, UPSTREAM_MOVED};
use anyhow::Result;
use std::io::Read;

pub(super) fn tar_member(bytes: &[u8], member: &str) -> Result<Option<Vec<u8>>> {
    read_tar_member(bytes, member).map_err(|error| unreadable(&error))
}

fn read_tar_member(bytes: &[u8], member: &str) -> std::io::Result<Option<Vec<u8>>> {
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

pub(super) fn zip_member(bytes: &[u8], member: &str) -> Result<Option<Vec<u8>>> {
    read_zip_member(bytes, member).map_err(|error| unreadable(&error))
}

fn read_zip_member(
    bytes: &[u8],
    member: &str,
) -> std::result::Result<Option<Vec<u8>>, zip::result::ZipError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;

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

/// One named member of a gzipped tarball, returned in memory.
///
/// The sibling of [`extract_member`], for the one caller that wants bytes
/// rather than a file: the release tarball holds `riabuild` at its root, and
/// those bytes go straight down an SSH pipe to a server rather than landing on
/// this machine at all. Writing them out only to read them back would put a
/// second copy of the binary on the laptop for no reason.
pub fn extract_single_file(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    if let Some(found) = read_tar_member_by_filename(bytes, name).map_err(|e| unreadable(&e))? {
        return Ok(found);
    }
    Err(Failure::new(
        format!("reading {name} out of the release tarball — it is not in there"),
        UPSTREAM_MOVED,
    )
    .into())
}

/// The whole-archive walk behind [`extract_single_file`], matched on the file
/// name alone rather than on a path boundary — the release tarball holds
/// `riabuild` at its root and nothing else is called that.
fn read_tar_member_by_filename(bytes: &[u8], name: &str) -> std::io::Result<Option<Vec<u8>>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path.file_name().is_some_and(|found| found == name) {
            let mut buffer = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buffer)?;
            return Ok(Some(buffer));
        }
    }
    Ok(None)
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
}

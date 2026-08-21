//! What the bytes have to hash to.
//!
//! Reading a digest out of the checksum files a project publishes, and taking
//! the sha256 of what actually arrived. The two are compared by the caller —
//! `tools::install` — before anything is written to the developer's machine.

use super::fetch_text;
use crate::Failure;
use anyhow::Result;

/// Finds the expected digest for one file in a `SHASUMS256.txt` body.
pub fn digest_for(shasums: &str, filename: &str) -> Option<String> {
    shasums.lines().find_map(|line| {
        let (digest, name) = line.split_once("  ")?;
        (name.trim() == filename).then(|| digest.trim().to_string())
    })
}

/// Finds a digest across several published checksum files, trying each in turn.
///
/// A list rather than a URL because Infisical publishes three files and the one
/// named after the release is not the one with the digests in it:
///
/// | File | Covers |
/// |---|---|
/// | `cli_<version>_checksums.txt` | one line, for `windows_amd64` |
/// | `checksums.txt` | everything **except** darwin |
/// | `checksums-darwin.txt` | the two darwin tarballs |
///
/// The darwin builds are produced separately — presumably notarised on a macOS
/// runner — and their digests never reach the main file. Reading only the file
/// named after the release finds nothing on any platform riabuild ships.
///
/// A digest in none of them is an error, never a skipped verification: an
/// unverified download of a credential tool is worse than no download.
pub async fn digest_from_any(urls: &[String], filename: &str) -> Result<String> {
    let mut failures = Vec::new();
    for url in urls {
        match fetch_text(url).await {
            Ok(body) => {
                if let Some(digest) = digest_for(&body, filename) {
                    return Ok(digest);
                }
                failures.push(format!("{url} does not list it"));
            }
            // A checksum file that 404s is ordinary — the list is deliberately
            // wider than any one release needs — so it only matters if every
            // entry fails.
            Err(error) => failures.push(format!("{url} could not be read: {error}")),
        }
    }
    Err(Failure::new(
        format!("finding the published checksum for {filename}"),
        "Send this to your team lead — riabuild will not install a download it cannot verify \
         against a checksum the project published.",
    )
    .detail(failures.join("; "))
    .into())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_digest_for_one_file_among_many() {
        let shasums = "\
aaaa1111  node-v22.23.1-linux-x64.tar.gz
bbbb2222  node-v22.23.1-darwin-arm64.tar.gz
cccc3333  node-v22.23.1-darwin-arm64.tar.xz
";
        assert_eq!(
            digest_for(shasums, "node-v22.23.1-darwin-arm64.tar.gz").as_deref(),
            Some("bbbb2222")
        );
        assert_eq!(digest_for(shasums, "node-v99.0.0-linux-x64.tar.gz"), None);
    }

    #[test]
    fn hashes_match_the_published_format() {
        // Lowercase hex, the same shape SHASUMS256.txt uses.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

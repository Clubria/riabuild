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

/// The sha512 of what actually arrived, to be compared against
/// [`npm_integrity_digest`].
///
/// sha512 rather than the sha256 everything else here uses, because the digest
/// it is compared against is not riabuild's choice: npm's `dist.integrity` is
/// what the registry recorded, and it is spelled sha512. The other field in
/// that document, `dist.shasum`, is a sha1 and is not something to verify a
/// toolchain against.
pub fn sha512(bytes: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA512, bytes)
        .as_ref()
        .to_vec()
}

/// The `sha512-<base64>` string npm would publish as `dist.integrity` for
/// these bytes.
///
/// For error messages only — what a mismatch is *decided* by is the byte
/// comparison in the caller, so that a base64 spelling npm changes one day
/// cannot present as tampering.
pub fn npm_integrity(bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(sha512(bytes))
    )
}

/// The digest an npm `dist.integrity` names.
///
/// `None` when it is not a sha512 riabuild can compute, which the caller
/// refuses as a **format** change rather than comparing against a sha512 it is
/// about to take: a digest in another algorithm would otherwise fail as a
/// mismatch, and a mismatch reads as tampering.
///
/// The comparison is on the decoded digest rather than on the string, so a
/// registry that dropped the base64 padding one day would still verify.
/// `dist.integrity` may in principle carry several space-separated entries;
/// the first sha512 among them is the one riabuild checks, and a document with
/// none is refused.
pub fn npm_integrity_digest(integrity: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    integrity
        .split_whitespace()
        .filter_map(|entry| entry.strip_prefix("sha512-"))
        .find_map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded))
                .ok()
                // A sha512 is 64 bytes. Anything else is not one, whatever it
                // is labelled.
                .filter(|digest| digest.len() == 64)
        })
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

    #[test]
    fn an_integrity_is_computed_the_way_npm_publishes_one() {
        // The NIST sha512 of "abc", base64'd — the same string `npm pack`
        // would put in `dist.integrity` for a tarball of these bytes. If this
        // is wrong every pnpm install fails as a checksum mismatch.
        assert_eq!(
            npm_integrity(b"abc"),
            "sha512-3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4\
             qmslPpUyknw=="
        );
        // And it round-trips: what riabuild computes is what it can read back.
        assert_eq!(
            npm_integrity_digest(&npm_integrity(b"abc")).as_deref(),
            Some(sha512(b"abc").as_slice())
        );
    }

    #[test]
    fn a_real_published_integrity_is_read_back_as_sixty_four_bytes() {
        // `@pnpm/linux-x64@11.11.0`'s `dist.integrity`, read from the registry
        // on 2026-08-21. The point is the shape: a padded base64 sha512 that
        // decodes to a 64-byte digest.
        let published = "sha512-rwMbNJR+PstRu+ymWoApei1CWrAnsnW3tm+3H8qOxbp8duiaj6u7DxlMzhKbVpFwylxcJdeGwZ5t\
             ReBFOVpsdw==";
        assert_eq!(npm_integrity_digest(published).map(|d| d.len()), Some(64));
    }

    #[test]
    fn an_integrity_riabuild_cannot_compute_is_unreadable_rather_than_wrong() {
        // Refused here so the caller can report a *format* change. Compared
        // against a sha512 instead, each of these would fail as a mismatch —
        // which reads as tampering and is not.
        for other in [
            "sha256-uu0Uc6dncf/8j5wcrJqCFYTfXlIH3IsgO5r9wRnaOZ0=",
            "sha1-hvfkN/qlp/zhXR3cuerq6jd2Z7g=",
            "",
            "sha512-",
            // Labelled sha512 and not one: a truncated digest must not be
            // padded out or accepted at whatever length it arrived.
            "sha512-3a81oZNherrMQXNJriBBMQ==",
            // Labelled sha512 and not base64 at all.
            "sha512-!!!!",
        ] {
            assert_eq!(npm_integrity_digest(other), None, "{other}");
        }
    }

    #[test]
    fn a_padding_change_is_not_a_mismatch() {
        // The comparison is on the decoded digest, so the same digest spelled
        // without base64 padding still verifies. Comparing the strings would
        // have made a registry formatting change look like tampering on every
        // laptop at once.
        let padded = npm_integrity(b"abc");
        let unpadded = padded.trim_end_matches('=').to_string();
        assert_ne!(padded, unpadded);
        assert_eq!(
            npm_integrity_digest(&unpadded),
            npm_integrity_digest(&padded)
        );
    }

    #[test]
    fn several_integrities_are_searched_for_the_one_riabuild_can_compute() {
        // `dist.integrity` is an `ssri` string and may carry more than one
        // entry. A sha512 among others is still a sha512.
        let mixed = format!(
            "sha1-hvfkN/qlp/zhXR3cuerq6jd2Z7g= {}",
            npm_integrity(b"abc")
        );
        assert_eq!(
            npm_integrity_digest(&mixed).as_deref(),
            Some(sha512(b"abc").as_slice())
        );
    }
}

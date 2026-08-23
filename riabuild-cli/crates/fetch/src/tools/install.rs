//! Downloading one tool, proving it is the right bytes, and unpacking it.
//!
//! The four steps the table next door describes as data, run in order: recall
//! or fetch the digest, download, compare, extract. Nothing is written to the
//! developer's machine until the comparison has passed.

use super::{Checksum, Release};
use crate::{Failure, archive, download};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// What the download has to hash to, fetched or recalled.
///
/// Split out of `install` so the pinned arm is assertable without a network:
/// reaching for a checksum file on the mirror would 404, because it publishes
/// none.
async fn expected_digest(release: &Release) -> Result<String> {
    match &release.checksum {
        Checksum::Published(urls) => download::digest_from_any(urls, &release.asset).await,
        Checksum::Pinned(digest) => Ok((*digest).to_string()),
    }
}

/// Downloads, verifies, and unpacks one tool into `~/.riabuild/<tool>/<version>`.
///
/// Safe to run twice: the destination is rewritten rather than appended to, and
/// a version that is already installed is simply reinstalled. Versioned
/// directories mean a bump installs beside the old copy rather than writing
/// over a binary that may be running.
pub async fn install(release: &Release, tool_dir: &Path) -> Result<PathBuf> {
    let expected = expected_digest(release).await?;

    let bytes = download::fetch_bytes(&release.url).await?;
    let actual = download::sha256_hex(&bytes);
    if actual != expected {
        return Err(Failure::new(
            format!(
                "installing {} — what riabuild downloaded does not match the checksum that was \
                 published for it, so nothing was installed",
                release.asset
            ),
            "Run `riabuild` again — a download mangled by a proxy is the usual cause. If it \
             happens twice, send this to your team lead before running it a third time.",
        )
        .detail(format!(
            "from {}: expected {expected}, got {actual}",
            release.url
        ))
        .into());
    }

    let binary = release.binary_in(tool_dir);
    archive::extract_member(bytes, release.kind()?, release.member, binary.clone()).await?;
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::super::ngrok;
    use super::*;

    #[tokio::test]
    async fn a_pinned_digest_is_answered_without_fetching_anything() {
        // `Pinned` exists to skip the network entirely: the expected digest is
        // already in the binary. Reaching for a checksum file here would 404 on
        // the mirror, which publishes none.
        let release = ngrok().unwrap();
        let Checksum::Pinned(digest) = release.checksum else {
            panic!("ngrok should be pinned");
        };
        assert_eq!(expected_digest(&release).await.unwrap(), digest);
    }
}

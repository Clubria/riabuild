//! Where the Node and pnpm archives come from, and what says they are the
//! right bytes.
//!
//! A trait rather than two functions so that what `apply()` decides *not* to
//! download is testable, and so that nothing above this line has to remember
//! to verify a digest.

use anyhow::Result;
use async_trait::async_trait;
use riabuild_fetch::download;
use riabuild_fetch::tools;
use riabuild_ui::Failure;

/// The two archives this task fetches, behind a trait so that what `apply()`
/// decides *not* to download is testable without pulling 50 MB over the
/// network — the same seam `remote::install` puts in front of the riabuild
/// release. Each returns bytes already checked against whatever the publisher
/// publishes, so nothing below this line has to remember to verify.
#[async_trait]
pub(super) trait Downloads: Send + Sync {
    async fn node(&self, version: &str) -> Result<Vec<u8>>;
    async fn pnpm(&self, version: &str, asset: &str) -> Result<Vec<u8>>;
}

pub(super) struct RealDownloads;

#[async_trait]
impl Downloads for RealDownloads {
    async fn node(&self, version: &str) -> Result<Vec<u8>> {
        let platform = download::node_platform()?;
        let filename = download::node_tarball_name(version, &platform);
        let shasums = download::fetch_text(&download::node_shasums_url(version)).await?;
        let expected = download::digest_for(&shasums, &filename).ok_or_else(|| {
            Failure::new(
                format!("downloading Node {version}"),
                "Ask your team lead to check the Node version pinned in the repo's .nvmrc.",
            )
            .detail(format!("nodejs.org does not publish {filename}"))
        })?;

        let bytes = download::fetch_bytes(&download::node_tarball_url(version, &platform)).await?;
        let actual = download::sha256_hex(&bytes);
        if actual != expected {
            // Never unpack an archive that is not the one nodejs.org published.
            return Err(Failure::new(
                format!("verifying the Node {version} download"),
                "Run `riabuild` again on a trusted network. If it keeps failing, tell your team lead.",
            )
            .detail(format!("expected sha256 {expected}, got {actual}"))
            .into());
        }
        Ok(bytes)
    }

    /// pnpm publishes no checksums file the way nodejs.org does, so the digest
    /// comes from the release itself: every asset uploaded to a GitHub release
    /// carries a `digest` GitHub computed over the stored bytes, and
    /// `tools::github_release_metadata` is the only place it is served.
    ///
    /// This used to download ~50 MB and hand it straight to the extractor, with
    /// a comment telling the next maintainer not to fix it. What it verifies
    /// now is the same class of thing `gh` and `infisical` are held to — a
    /// digest the release host publishes, fetched over HTTPS, checked against
    /// the complete buffer before anything is unpacked — rather than "the
    /// transfer completed".
    ///
    /// A **mirror** is what `../../../../CLAUDE.md` prescribes where a project
    /// publishes no digest, and it is the wrong tool here. ngrok and Grok Build
    /// are pinned to one version in this repository, so `Checksum::Pinned` can
    /// name their bytes; pnpm's version is read out of the checkout's
    /// `packageManager` at runtime, and a constant cannot describe a version
    /// nobody has chosen yet. Pinning pnpm to make the mirror possible would
    /// turn a `packageManager` bump into a fleet-wide install failure until a
    /// riabuild release caught up.
    ///
    /// It fails closed. A release old enough to predate GitHub recording asset
    /// digests carries none, and that is an error rather than a download —
    /// which is the whole difference between this and what it replaced.
    async fn pnpm(&self, version: &str, asset: &str) -> Result<Vec<u8>> {
        let metadata = tools::github_release_metadata("pnpm/pnpm", &format!("v{version}")).await?;
        let expected = pnpm_digest(&metadata, version, asset)?;

        let bytes = download::fetch_bytes(&download::pnpm_url(version, asset)).await?;
        let actual = download::sha256_hex(&bytes);
        if actual != expected {
            // Never unpack an archive that is not the one the pnpm release
            // holds. Same sentence as Node's, one line above.
            return Err(Failure::new(
                format!("verifying the pnpm {version} download"),
                "Run `riabuild` again on a trusted network. If it keeps failing, tell your team lead.",
            )
            .detail(format!("expected sha256 {expected}, got {actual}"))
            .into());
        }
        Ok(bytes)
    }
}

/// The sha256 a pnpm release records for one of its assets.
///
/// Split from the request so the parsing is testable without a network, which
/// is the half that goes wrong: an asset renamed upstream, a release whose
/// assets predate GitHub recording digests, and an answer that is not the
/// release at all all look the same from the call site otherwise.
///
/// `digest` is spelled `sha256:<hex>`. Anything else is refused rather than
/// compared against a sha256 riabuild is about to compute, because a digest in
/// an algorithm this does not implement would otherwise fail as a *mismatch* —
/// which reads as tampering and is not.
pub(super) fn pnpm_digest(metadata: &str, version: &str, asset: &str) -> Result<String> {
    let refuse = |detail: String| {
        Failure::new(
            format!("verifying the pnpm {version} download"),
            "Ask your team lead to check the pnpm version pinned in the repo's package.json — \
             riabuild will not install a download it cannot verify.",
        )
        .detail(detail)
    };

    let parsed: serde_json::Value = serde_json::from_str(metadata)
        .map_err(|error| refuse(format!("GitHub's answer is not JSON: {error}")))?;
    let assets = parsed
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| refuse("GitHub's answer lists no assets".to_string()))?;

    let found = assets
        .iter()
        .find(|entry| entry.get("name").and_then(serde_json::Value::as_str) == Some(asset))
        .ok_or_else(|| {
            refuse(format!(
                "the pnpm {version} release does not contain {asset}"
            ))
        })?;

    let digest = found
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            refuse(format!(
                "the pnpm {version} release records no digest for {asset} — releases from before \
                 GitHub started recording them carry none, so there is nothing to verify against"
            ))
        })?;

    digest
        .strip_prefix("sha256:")
        .map(str::to_string)
        .ok_or_else(|| refuse(format!("{asset} records `{digest}`, which is not a sha256")).into())
}

//! Where the Node and pnpm archives come from, and what says they are the
//! right bytes.
//!
//! A trait rather than two functions so that what `apply()` decides *not* to
//! download is testable, and so that nothing above this line has to remember
//! to verify a digest.

use anyhow::Result;
use async_trait::async_trait;
use riabuild_fetch::download;
use riabuild_ui::Failure;

/// The archives this task fetches, behind a trait so that what `apply()`
/// decides *not* to download is testable without pulling 50 MB over the
/// network — the same seam `remote::install` puts in front of the riabuild
/// release. Each returns bytes already checked against whatever the publisher
/// publishes, so nothing below this line has to remember to verify.
#[async_trait]
pub(super) trait Downloads: Send + Sync {
    async fn node(&self, version: &str) -> Result<Vec<u8>>;
    /// The `pnpm` npm package, at any version pnpm has published.
    ///
    /// One package, because what riabuild installs is pnpm's JavaScript rather
    /// than one of its platform executables — `download::PNPM_PACKAGE` is where
    /// the reason for that is written down, and it is a shared library rather
    /// than a preference.
    async fn pnpm(&self, version: &str) -> Result<Vec<u8>>;
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

    /// pnpm, from the **npm registry** rather than from its GitHub releases.
    ///
    /// pnpm publishes no checksums file the way nodejs.org does, so for a
    /// while the digest came from GitHub's REST API, which records one per
    /// release asset. That is a real digest and it is served on a budget a
    /// provisioner cannot depend on: sixty unauthenticated requests an hour
    /// *per address*, which one office behind one NAT exhausts, after which
    /// nobody there can provision anything. Both e2e jobs stopped at exactly
    /// that.
    ///
    /// npm answers the same question with no ceiling. Every published version
    /// carries `dist.integrity`, the sha512 the registry recorded over the
    /// stored tarball — the field every `npm install` already verifies
    /// against, with an SLSA provenance attestation beside it. It is a digest
    /// the *publisher* records, so this is the class of thing `gh` and
    /// `infisical` are held to rather than "the transfer completed".
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
    /// It fails closed at every step. A version the registry does not carry, a
    /// document with no `dist.integrity`, an integrity in an algorithm riabuild
    /// cannot compute, and bytes that do not match are each an error rather
    /// than an unverified download.
    ///
    /// One package, and never a platform one. `@pnpm/<platform>` is where the
    /// standalone executable lives and it cannot run on the machines this
    /// provisions — see `download::PNPM_PACKAGE`. Asking for `pnpm` also means
    /// there is no per-version branch left here to get wrong: the unscoped
    /// package is what every pnpm has been published as.
    async fn pnpm(&self, version: &str) -> Result<Vec<u8>> {
        npm_package(download::PNPM_PACKAGE, version).await
    }
}

/// One npm package's tarball, checked against the integrity the registry
/// published for it before it is handed back.
///
/// The digest is fetched from the version document and compared against the
/// **complete** buffer, before anything is unpacked and before anything is
/// written to the developer's machine — the same order the Node path above
/// uses, and the reason `download` hands back bytes rather than streaming to a
/// file.
///
/// Both requests report their own failures and neither is re-wrapped here. A
/// version the registry does not carry answers 404, which `download` already
/// reports as a pin that has to be updated. Wrapping both in one pnpm-shaped
/// message would have told a developer whose VPN was down to go and read
/// `package.json`.
async fn npm_package(package: &str, version: &str) -> Result<Vec<u8>> {
    let metadata = download::fetch_text(&download::npm_metadata_url(package, version)).await?;
    let published = published_integrity(&metadata, package, version)?;

    let bytes = download::fetch_bytes(&download::npm_tarball_url(package, version)).await?;
    verified(package, version, &published, bytes)
}

/// The comparison itself: the complete buffer against the integrity npm
/// published, before it is handed to anything that unpacks it.
///
/// A function of its own so that "a mismatch is refused" is assertable without
/// a network. The whole download is in memory precisely so this can happen
/// first — streaming to a file would mean writing unverified bytes into a
/// developer's toolchain directory and checking them afterwards.
///
/// The decoded digests are compared rather than the two strings, so a base64
/// spelling npm changes one day cannot present as tampering; the strings are
/// what the developer is shown, because they are what is on the registry page.
pub(super) fn verified(
    package: &str,
    version: &str,
    published: &str,
    bytes: Vec<u8>,
) -> Result<Vec<u8>> {
    if download::npm_integrity_digest(published) == Some(download::sha512(&bytes)) {
        return Ok(bytes);
    }
    // Never unpack an archive that is not the one npm published.
    Err(Failure::new(
        format!(
            "verifying the {package}@{version} download — what riabuild downloaded does not match \
             the integrity npm published for it, so nothing was installed"
        ),
        "Run `riabuild` again on a trusted network. If it keeps failing, tell your team lead.",
    )
    .detail(format!(
        "expected {published}, got {}",
        download::npm_integrity(&bytes)
    ))
    .into())
}

/// The integrity npm published for one version, as the `sha512-<base64>` string
/// the registry serves.
///
/// Split from the request so the parsing is testable without a network, which
/// is the half that goes wrong: a package renamed upstream, a document with no
/// `dist` in it, and an answer that is not a version document at all all look
/// the same from the call site otherwise.
///
/// An integrity `download::npm_integrity_digest` cannot read is refused **here**
/// rather than compared against a sha512 riabuild is about to compute, because
/// a digest in an algorithm this does not implement would otherwise fail as a
/// *mismatch* — which reads as tampering and is not.
pub(super) fn published_integrity(metadata: &str, package: &str, version: &str) -> Result<String> {
    let refuse = |detail: String| {
        Failure::new(
            format!("verifying the {package}@{version} download"),
            "Ask your team lead to check the pnpm version pinned in the repo's package.json — \
             riabuild will not install a download it cannot verify.",
        )
        .detail(detail)
    };

    let parsed: serde_json::Value = serde_json::from_str(metadata)
        .map_err(|error| refuse(format!("npm's answer is not JSON: {error}")))?;
    let integrity = parsed
        .get("dist")
        .and_then(|dist| dist.get("integrity"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            refuse(format!(
                "npm records no dist.integrity for {package}@{version}, so there is nothing to \
                 verify against"
            ))
        })?;

    if download::npm_integrity_digest(integrity).is_none() {
        return Err(refuse(format!(
            "{package}@{version} records `{integrity}`, which is not a sha512 riabuild can compute"
        ))
        .into());
    }
    Ok(integrity.to_string())
}

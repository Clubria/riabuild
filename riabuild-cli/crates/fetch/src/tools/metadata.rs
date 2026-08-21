//! Asking GitHub what a release contains.
//!
//! One route, for the digest GitHub itself computes over every asset uploaded
//! to a release. It reads *metadata* and never bytes that get installed: what
//! it carries is checked against a download `fetch_bytes` made separately, and
//! a mismatch is a hard failure at the call site.

use crate::Failure;
use anyhow::Result;

/// How long a metadata request is given. Small next to `download`'s, because
/// the body is a few kilobytes of JSON rather than a 130 MB toolchain, and a
/// developer waiting on a digest is a developer waiting on nothing visible.
const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One release's metadata from GitHub's REST API, as JSON text.
///
/// **What it is for.** Some projects publish a `checksums.txt` beside their
/// assets and some publish nothing, but every asset uploaded to a GitHub
/// release carries a `digest` GitHub itself computed over the stored bytes, and
/// the API is the only place it is served. pnpm is the case that needed it:
/// `tasks::toolchain` used to hand ~50 MB straight to the extractor with no
/// digest at all, because pnpm's releases carry no checksum file — and unlike
/// ngrok and Grok Build, mirroring is not the answer there, since the version
/// comes from the checkout's `packageManager` at runtime and a `Pinned` constant
/// cannot describe a version nobody chose yet.
///
/// **Why it is not `download::fetch_text`.** That client sends no `User-Agent`,
/// and `api.github.com` answers a request without one with a 403 before it
/// looks at the path. Artifact hosts require none, which is why the download
/// path has never had to send one.
///
/// **Why it returns text.** `riabuild-fetch` carries no JSON parser and is a
/// deliberately small crate — downloading bytes, hashing them, unpacking them.
/// The caller reads the field it wants. That seam is not free and it is smaller
/// than a serialiser in the crate that fetches executables.
///
/// This reads *metadata*, never bytes that get installed: the digest it carries
/// is checked against a download `fetch_bytes` made separately, and a mismatch
/// is a hard failure at the call site.
pub async fn github_release_metadata(repo: &str, tag: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/{tag}");
    let client = reqwest::Client::builder()
        .timeout(METADATA_TIMEOUT)
        // A name, not a version: this crate's `CARGO_PKG_VERSION` is `0.0.0`
        // for every riabuild ever released, and `riabuild-version` is not
        // reachable from here. GitHub asks for something identifying, and a
        // version that is always the same identifies nothing.
        .user_agent("riabuild")
        .build()
        .map_err(|error| metadata_failed(&url, &error.to_string()))?;

    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| metadata_failed(&url, &error.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(metadata_failed(&url, &format!("GitHub answered {status}")));
    }
    response
        .text()
        .await
        .map_err(|error| metadata_failed(&url, &error.to_string()))
}

/// GitHub would not say what the release contains, so nothing can be verified
/// and nothing is installed.
///
/// The action names the rate limit on purpose. An unauthenticated caller gets
/// sixty requests an hour *per address*, which on one laptop is unreachable and
/// on a shared server behind one NAT is not — and "run it again in a while" is
/// the entire remedy, which a developer will not guess from a 403.
fn metadata_failed(url: &str, detail: &str) -> anyhow::Error {
    Failure::new(
        format!("asking GitHub what {url} contains"),
        "Run `riabuild` again in a few minutes. GitHub allows sixty unauthenticated requests an \
         hour per address, so a shared network can run out of them; if it keeps failing, tell \
         your team lead.",
    )
    .detail(detail.to_string())
    .into()
}

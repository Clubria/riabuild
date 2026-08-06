//! Fetching and verifying the distributions riabuild owns.
//!
//! Where the bytes come from and whether they are the right bytes. Unpacking
//! them is `archive.rs`, which only ever sees a buffer that has already matched
//! a published digest.
//!
//! riabuild owns its Node rather than driving nvm: nvm is a bash function, not a
//! binary, so Rust cannot drive it without spawning a login shell, it does not
//! work in fish, and sourcing it costs every shell start 200 ms to 1 s. corepack
//! is not an option either — it was removed from Node.js 25+ distributions.
//! Owning the tarball is a few dozen lines and removes a class of
//! works-in-my-shell failures.
//!
//! The same reasoning extends to `gh` and `infisical` — see `tools.rs`, which
//! describes where their releases live and what the assets are called.

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// The ceiling ureq's `take()` used to enforce while streaming. reqwest buffers
/// the body in one call, so the cap is checked after the fact instead.
const MAX_DOWNLOAD: usize = 400 * 1024 * 1024;

/// The Node distribution name for this machine, e.g. `darwin-arm64`.
pub fn node_platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        other => return Err(anyhow!("riabuild does not support {other} yet")),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => return Err(anyhow!("riabuild does not support {other} CPUs yet")),
    };
    Ok(format!("{os}-{arch}"))
}

/// pnpm 11 and newer publish a tarball; 10 and older publish a bare executable.
///
/// The boundary is the pinned version rather than today's date, because GitHub
/// still serves each release exactly as it was published.
pub fn pnpm_ships_a_tarball(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        // An unparseable pin is likelier to be something new than something
        // ancient, and a tarball is what pnpm publishes now.
        .is_none_or(|major| major >= 11)
}

/// The asset name for a pnpm release, which changed shape at pnpm 11.
///
/// Up to pnpm 10 a release published bare executables named `pnpm-macos-arm64`.
/// pnpm 11 renamed macOS to `darwin` *and* switched to
/// `pnpm-darwin-arm64.tar.gz`, an archive holding a launcher and the `dist/`
/// tree it loads at startup — so it is no longer something that can be dropped
/// onto `PATH`. Asking for the old name against a new release is a 404, which
/// is how this was found.
pub fn pnpm_asset(version: &str) -> Result<String> {
    let tarball = pnpm_ships_a_tarball(version);
    let os = match std::env::consts::OS {
        "macos" if tarball => "darwin",
        "macos" => "macos",
        "linux" => "linux",
        other => return Err(anyhow!("riabuild does not support {other} yet")),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => return Err(anyhow!("riabuild does not support {other} CPUs yet")),
    };
    Ok(if tarball {
        format!("pnpm-{os}-{arch}.tar.gz")
    } else {
        format!("pnpm-{os}-{arch}")
    })
}

pub fn node_tarball_name(version: &str, platform: &str) -> String {
    format!("node-v{version}-{platform}.tar.gz")
}

pub fn node_tarball_url(version: &str, platform: &str) -> String {
    format!(
        "https://nodejs.org/dist/v{version}/{}",
        node_tarball_name(version, platform)
    )
}

pub fn node_shasums_url(version: &str) -> String {
    format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt")
}

pub fn pnpm_url(version: &str, asset: &str) -> String {
    format!("https://github.com/pnpm/pnpm/releases/download/v{version}/{asset}")
}

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
    Err(anyhow!(
        "riabuild could not find a published checksum for {filename}, so it \
         refused to install it ({})",
        failures.join("; ")
    ))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Reads a whole distribution into memory.
///
/// Deliberately not streamed to disk: the sha256 in `verify` is checked against
/// the complete buffer *before* anything is extracted. Streaming would mean
/// writing unverified bytes into a developer's toolchain directory and checking
/// them afterwards, which is a weaker property for a tool that installs
/// executables.
pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .with_context(|| format!("could not download {url}"))?
        .get(url)
        .send()
        .await
        .with_context(|| format!("could not download {url}"))?
        .error_for_status()
        .with_context(|| format!("could not download {url}"))?;

    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("download of {url} was cut short"))?;

    if bytes.len() > MAX_DOWNLOAD {
        return Err(anyhow!(
            "{url} is {} bytes, more than riabuild will download",
            bytes.len()
        ));
    }
    Ok(bytes.to_vec())
}

pub async fn fetch_text(url: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&fetch_bytes(url).await?).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_urls_node_actually_publishes() {
        assert_eq!(
            node_tarball_url("22.23.1", "darwin-arm64"),
            "https://nodejs.org/dist/v22.23.1/node-v22.23.1-darwin-arm64.tar.gz"
        );
        assert_eq!(
            node_shasums_url("22.23.1"),
            "https://nodejs.org/dist/v22.23.1/SHASUMS256.txt"
        );
        assert_eq!(
            pnpm_url("11.11.0", "pnpm-darwin-arm64.tar.gz"),
            "https://github.com/pnpm/pnpm/releases/download/v11.11.0/pnpm-darwin-arm64.tar.gz"
        );
    }

    #[test]
    fn pnpm_11_is_a_tarball_and_pnpm_10_is_not() {
        // Asking for the old asset name against a new release is a 404, which
        // is exactly how riabuild stopped being able to install pnpm at all.
        assert!(pnpm_ships_a_tarball("11.11.0"));
        assert!(pnpm_ships_a_tarball("12.0.0"));
        assert!(!pnpm_ships_a_tarball("10.20.0"));
        assert!(!pnpm_ships_a_tarball("9.15.9"));
        // Something unrecognisable is likelier to be new than ancient.
        assert!(pnpm_ships_a_tarball("next"));
    }

    #[test]
    fn the_asset_name_follows_the_pinned_version() {
        // The host decides the platform, so only the shape is asserted here.
        let modern = pnpm_asset("11.11.0").unwrap();
        assert!(modern.ends_with(".tar.gz"), "{modern}");
        assert!(
            !modern.contains("macos"),
            "pnpm 11 calls macOS darwin: {modern}"
        );

        let legacy = pnpm_asset("10.20.0").unwrap();
        assert!(!legacy.ends_with(".tar.gz"), "{legacy}");
        assert!(
            !legacy.contains("darwin"),
            "pnpm 10 calls macOS macos: {legacy}"
        );
    }

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

    /// Proves this build can resolve a name, complete a TLS handshake, and
    /// read a real body.
    ///
    /// Ignored by default because it needs the network. CI runs it against the
    /// musl artefact, where it is the only thing standing between us and a
    /// static binary that builds, links, reports its version, and then cannot
    /// reach anything on a developer's machine — the two ways that happens are
    /// `rustls-tls-native-roots` finding no certificate store and musl's
    /// resolver behaving differently from glibc's, and neither is visible
    /// without actually making a request.
    #[tokio::test]
    #[ignore = "requires network; pins TLS and DNS for this build"]
    async fn tls_and_dns_work_on_this_build() {
        let shasums = fetch_text(&node_shasums_url("22.23.1"))
            .await
            .expect("fetch");
        assert!(
            digest_for(&shasums, "node-v22.23.1-linux-x64.tar.gz").is_some(),
            "reached nodejs.org but the body was not SHASUMS256.txt"
        );
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
    fn platform_names_are_the_ones_upstream_publishes() {
        let platform = node_platform().unwrap();
        assert!(
            ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64"].contains(&platform.as_str()),
            "unexpected platform {platform}"
        );
    }
}

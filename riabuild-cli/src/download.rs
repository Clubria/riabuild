//! Fetching and verifying the Node tarball and the pnpm binary.
//!
//! riabuild owns its Node rather than driving nvm: nvm is a bash function, not a
//! binary, so Rust cannot drive it without spawning a login shell, it does not
//! work in fish, and sourcing it costs every shell start 200 ms to 1 s. corepack
//! is not an option either — it was removed from Node.js 25+ distributions.
//! Owning the tarball is a few dozen lines and removes a class of
//! works-in-my-shell failures.

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

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

/// The pnpm standalone binary name for this machine.
pub fn pnpm_asset() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        other => return Err(anyhow!("riabuild does not support {other} yet")),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => return Err(anyhow!("riabuild does not support {other} CPUs yet")),
    };
    Ok(format!("pnpm-{os}-{arch}"))
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

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let response = ureq::get(url)
        .timeout(Duration::from_secs(300))
        .call()
        .with_context(|| format!("could not download {url}"))?;

    let mut buffer = Vec::new();
    response
        .into_reader()
        .take(400 * 1024 * 1024)
        .read_to_end(&mut buffer)
        .with_context(|| format!("download of {url} was cut short"))?;
    Ok(buffer)
}

pub fn fetch_text(url: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&fetch_bytes(url)?).into_owned())
}

/// Unpacks a `node-v*.tar.gz` into `target`, stripping the archive's own
/// top-level directory so `target/bin/node` is the binary.
pub fn extract_node_tarball(bytes: &[u8], target: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);

    if target.exists() {
        // A half-extracted directory from an interrupted run must not be
        // mistaken for a working install — `apply()` starts from nothing.
        std::fs::remove_dir_all(target)
            .with_context(|| format!("could not clear {}", target.display()))?;
    }
    std::fs::create_dir_all(target)?;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut components = path.components();
        components.next(); // drop `node-v22.23.1-darwin-arm64/`
        let relative: std::path::PathBuf = components.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        entry.unpack(target.join(relative))?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
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
            pnpm_url("10.20.0", "pnpm-macos-arm64"),
            "https://github.com/pnpm/pnpm/releases/download/v10.20.0/pnpm-macos-arm64"
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

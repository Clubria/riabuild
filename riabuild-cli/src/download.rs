//! Fetching and verifying the Node and pnpm distributions.
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

/// Unpacks a `node-v*.tar.gz` into `target` so that `target/bin/node` is the
/// binary: Node wraps everything in one `node-v22.23.1-darwin-arm64/` directory.
pub fn extract_node_tarball(bytes: &[u8], target: &Path) -> Result<()> {
    extract_tarball(bytes, target, 1)
}

/// pnpm has no wrapper directory: the `pnpm` launcher and the `dist/` tree it
/// loads sit at the root of the archive, and must stay beside each other.
pub fn extract_pnpm_tarball(bytes: &[u8], target: &Path) -> Result<()> {
    extract_tarball(bytes, target, 0)
}

fn extract_tarball(bytes: &[u8], target: &Path, strip_components: usize) -> Result<()> {
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
        for _ in 0..strip_components {
            components.next();
        }
        let relative: std::path::PathBuf = components.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        let destination = target.join(relative);
        // `unpack` will not create the directories above a file, and an
        // archive is not obliged to carry an entry for every directory it
        // uses. Both Node and pnpm happen to carry them today.
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(destination)?;
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

    fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

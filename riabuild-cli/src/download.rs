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
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The ceiling ureq's `take()` used to enforce while streaming. reqwest buffers
/// the body in one call, so the cap is checked after the fact instead.
const MAX_DOWNLOAD: usize = 400 * 1024 * 1024;

const RELEASES: &str = "https://github.com/Clubria/riabuild/releases/download";

/// The Rust target triple a server's `uname -sm` corresponds to.
///
/// Remote mode provisions a server that is frequently a different platform
/// than the laptop driving it, so — unlike `node_platform` above — this takes
/// the platform as an argument rather than reading `std::env::consts` for the
/// host riabuild happens to be running on.
pub fn rust_target(uname_s: &str, uname_m: &str) -> Result<String> {
    let arch = match uname_m.trim() {
        "x86_64" | "amd64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => return Err(anyhow!("riabuild does not publish a build for {other}")),
    };
    match uname_s.trim() {
        "Darwin" => Ok(format!("{arch}-apple-darwin")),
        // musl rather than gnu: one Linux build then runs on any distribution
        // instead of only on distributions with a glibc at least as new as the
        // one the release runner happened to build against.
        "Linux" => Ok(format!("{arch}-unknown-linux-musl")),
        other => Err(anyhow!("riabuild does not publish a build for {other}")),
    }
}

/// The release asset name for a given version and target triple, e.g.
/// `riabuild-2026.08.06-aarch64-apple-darwin.tar.gz`. Matches the tarball
/// name `.github/workflows/release.yml`'s Package step produces.
pub fn riabuild_asset(version: &str, target: &str) -> String {
    format!("riabuild-{version}-{target}.tar.gz")
}

pub fn riabuild_asset_url(version: &str, target: &str) -> String {
    format!("{RELEASES}/v{version}/{}", riabuild_asset(version, target))
}

pub fn riabuild_checksums_url(version: &str) -> String {
    format!("{RELEASES}/v{version}/riabuild-{version}-checksums.txt")
}

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

/// Unpacks into `target` without ever clearing it where it stands.
///
/// `target` lives under `paths::tools_root()`, **shared** by every developer
/// with an account on a server — so it is not ours to delete. This used to open
/// with `remove_dir_all(target)` on the strength of "`apply()` starts from
/// nothing", which held while `tools_root()` and `root()` were one directory on
/// one laptop and became a way to delete the Node a colleague's `pnpm dev` is
/// running out of the moment they stopped being.
///
/// So the archive is unpacked into a sibling directory carrying this process's
/// pid and `rename`d into place — `remote::install::write_binary`'s idiom, for
/// its reason: two developers installing one version at once is the ordinary
/// case on a shared box, not the exotic one. A reader sees a complete tree or
/// none, and an interrupted run leaves its mess where nothing looks.
///
/// Judging whether what is already at `target` is any good is the *caller's*
/// job (`tasks::toolchain` asks the binary its version); a `target` that is
/// there is replaced by moving it aside, never by emptying it in place.
fn extract_tarball(bytes: &[u8], target: &Path, strip_components: usize) -> Result<()> {
    let staging = staging_beside(target, "part");
    // Only ever this pid's own leftovers from an interrupted earlier run —
    // never another developer's staging directory, and never `target`.
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("could not clear {}", staging.display()))?;
    }
    if let Err(error) = unpack(bytes, &staging, strip_components) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    swap_into_place(&staging, target)
}

/// `…/node/.22.23.1.4171.part`, beside the tree it is about to become: the same
/// directory, so the same filesystem, so the `rename` that installs it is
/// atomic rather than a copy.
fn staging_beside(target: &Path, tag: &str) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{}.{tag}", std::process::id()))
}

/// Installs a finished staging tree at `target` with a `rename`, so nothing
/// ever observes a partial one.
fn swap_into_place(staging: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        // Something is there and the caller judged it unusable. It still gets
        // moved aside rather than emptied where it stands: a process that
        // already holds files open under it keeps reading a whole, consistent
        // tree, which `remove_dir_all` in place cannot promise.
        let stale = staging_beside(target, "stale");
        let _ = std::fs::remove_dir_all(&stale);
        std::fs::rename(target, &stale)
            .with_context(|| format!("could not move {} aside", target.display()))?;
        if let Err(error) = std::fs::rename(staging, target) {
            // Put back what was there rather than leaving the shared path empty.
            let _ = std::fs::rename(&stale, target);
            return Err(error).with_context(|| format!("could not install {}", target.display()));
        }
        let _ = std::fs::remove_dir_all(&stale);
        return Ok(());
    }

    match std::fs::rename(staging, target) {
        Ok(()) => Ok(()),
        // A co-tenant installing the same version won the race between that
        // check and this rename. Their tree arrived the same way ours would
        // have, so it is as good as ours — and ours is the one that goes.
        Err(_) if target.exists() => {
            let _ = std::fs::remove_dir_all(staging);
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("could not install {}", target.display())),
    }
}

fn unpack(bytes: &[u8], target: &Path, strip_components: usize) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);

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

/// One named member of a gzipped tarball, in memory.
///
/// The release tarball holds `riabuild` at its root. The bytes are wanted rather
/// than a path, because they go straight down an SSH pipe to a server.
pub fn extract_single_file(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let matches = path.file_name().is_some_and(|found| found == name);
        if matches {
            let mut buffer = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buffer)?;
            return Ok(buffer);
        }
    }
    anyhow::bail!("{name} is not in that archive")
}

#[cfg(unix)]
pub async fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = tokio::fs::metadata(path).await?.permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(path, permissions).await?;
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
    fn a_failed_extraction_leaves_a_tree_another_developer_is_using_alone() {
        // `tools_root()` is shared by every developer on a server, so the tree
        // being unpacked over is one a co-tenant's `pnpm dev` may be running
        // out of. This used to open with `remove_dir_all(target)` and extract
        // afterwards, so a truncated archive — or a process killed between the
        // two — left the colleague with nothing at all. Unpacking into a
        // pid-suffixed staging directory first costs a failure that directory
        // and nothing else.
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        std::fs::create_dir_all(target.join("bin")).unwrap();
        std::fs::write(target.join("bin/node"), b"the node ada is running").unwrap();

        extract_node_tarball(b"not a gzip stream at all", &target).expect_err("corrupt archive");

        assert_eq!(
            std::fs::read(target.join("bin/node")).unwrap(),
            b"the node ada is running"
        );
        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn replacing_a_tree_swaps_the_whole_thing_rather_than_unpacking_over_it() {
        // The other half: when the caller *has* judged what is there unusable,
        // the new tree arrives whole. A file the archive does not carry cannot
        // survive as a leftover from the old one, which is what unpacking into
        // a live directory would leave behind — and no staging directory is
        // left lying beside it either.
        let home = tempfile::TempDir::new().unwrap();
        let target = home.path().join("22.23.1");
        std::fs::create_dir_all(target.join("bin")).unwrap();
        std::fs::write(target.join("bin/node"), b"a broken node").unwrap();
        std::fs::write(target.join("bin/leftover"), b"from the old tree").unwrap();

        let bytes = tarball(&[("node-v22.23.1-darwin-arm64/bin/node", b"a working node")]);
        extract_node_tarball(&bytes, &target).unwrap();

        assert_eq!(
            std::fs::read(target.join("bin/node")).unwrap(),
            b"a working node"
        );
        assert!(!target.join("bin/leftover").exists());
        assert_eq!(
            leftovers_beside(home.path(), "22.23.1"),
            Vec::<String>::new()
        );
    }

    /// Everything in `dir` other than `keep` — the staging and set-aside
    /// directories, if any survived.
    fn leftovers_beside(dir: &std::path::Path, keep: &str) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != keep)
            .collect();
        found.sort();
        found
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

    #[test]
    fn uname_output_maps_to_the_target_the_release_publishes() {
        // Captured from real `uname -sm` output. Apple's arm64 is Rust's aarch64,
        // and Linux binaries are musl so one build runs on every distribution rather
        // than on everything newer than the runner's glibc.
        assert_eq!(
            rust_target("Darwin", "arm64").expect("mac"),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            rust_target("Darwin", "x86_64").expect("mac"),
            "x86_64-apple-darwin"
        );
        assert_eq!(
            rust_target("Linux", "x86_64").expect("linux"),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            rust_target("Linux", "aarch64").expect("linux"),
            "aarch64-unknown-linux-musl"
        );
        // Some distributions report arm64 rather than aarch64.
        assert_eq!(
            rust_target("Linux", "arm64").expect("linux"),
            "aarch64-unknown-linux-musl"
        );
        // `uname` output arrives with a trailing newline.
        assert_eq!(
            rust_target("Linux\n", "x86_64\n").expect("linux"),
            "x86_64-unknown-linux-musl"
        );
    }

    #[test]
    fn an_unpublished_platform_is_an_error_rather_than_a_guess() {
        // Installing the wrong architecture produces an exec format error on the
        // server with nothing in it that names riabuild.
        assert!(rust_target("Linux", "i686").is_err());
        assert!(rust_target("Linux", "armv7l").is_err());
        assert!(rust_target("FreeBSD", "x86_64").is_err());
        assert!(rust_target("Darwin", "ppc").is_err());
    }

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

    #[test]
    fn asset_names_match_what_the_release_workflow_uploads() {
        // release.yml builds `riabuild-$version-$target.tar.gz` and appends each
        // digest to `riabuild-$version-checksums.txt`. If either is renamed there,
        // this test is what fails.
        assert_eq!(
            riabuild_asset("2026.08.06", "aarch64-apple-darwin"),
            "riabuild-2026.08.06-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            riabuild_asset_url("2026.08.06", "x86_64-unknown-linux-musl"),
            "https://github.com/Clubria/riabuild/releases/download/v2026.08.06/riabuild-2026.08.06-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            riabuild_checksums_url("2026.08.06"),
            "https://github.com/Clubria/riabuild/releases/download/v2026.08.06/riabuild-2026.08.06-checksums.txt"
        );
    }
}

//! The third-party CLIs riabuild owns: `gh` and `infisical`.
//!
//! riabuild already owns Node, pnpm, and Claude Code — it downloads them,
//! verifies them against a published digest, and keeps them under
//! `~/.riabuild/`. These two used to be the exception, installed with `brew
//! install` and, on a machine without Homebrew, not installed at all.
//!
//! That exception is what made Linux awkward, because there is no `brew` to
//! substitute. The alternatives were adding GitHub's apt repository and
//! Infisical's Cloudsmith repositories with `sudo`, or telling the developer to
//! install two CLIs by hand before riabuild could do anything. Both are worse
//! than the rule the rest of the codebase already follows.
//!
//! **Nothing on the developer's `PATH` is trusted.** Homebrew survives in one
//! place: distributing riabuild itself on macOS.

use crate::archive::{self, Kind};
use crate::download;
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

/// The pinned versions.
///
/// Constants rather than a `releases/latest` lookup at install time. What
/// riabuild puts on a laptop should be versioned, auditable, and shipped
/// through a signed release — not decided by whatever upstream published this
/// morning. It also keeps machines reproducible: two developers who ran
/// `riabuild` a week apart get the same `gh`, and a bug that reproduces on one
/// reproduces on the other.
///
/// Bumping either is a code change, and the task's `version()` goes up beside
/// it so every existing install converges.
pub const GH_VERSION: &str = "2.97.0";
pub const INFISICAL_VERSION: &str = "0.43.120";

/// Where each binary sits inside its archive — and, once installed, inside
/// `~/.riabuild/<tool>/<version>/`. One constant for both, so the path a task
/// runs and the path `install` writes cannot drift apart.
pub const GH_MEMBER: &str = "bin/gh";
pub const INFISICAL_MEMBER: &str = "infisical";

/// Everything needed to fetch one tool and find its binary afterwards.
#[derive(Debug, Clone)]
pub struct Release {
    /// Names the directory under `~/.riabuild/`.
    pub tool: &'static str,
    pub version: &'static str,
    pub asset: String,
    pub url: String,
    /// Tried in order. More than one because Infisical splits them — see
    /// `download::digest_from_any`.
    pub checksum_urls: Vec<String>,
    /// The binary's path inside the archive, and equally its path inside the
    /// tool's directory once installed.
    pub member: &'static str,
}

impl Release {
    pub fn kind(&self) -> Result<Kind> {
        Kind::of(&self.asset)
    }

    /// Where the binary ends up, given `~/.riabuild/<tool>/<version>`.
    pub fn binary_in(&self, tool_dir: &Path) -> PathBuf {
        tool_dir.join(self.member)
    }
}

/// Go's architecture words, which both projects use to name their assets.
///
/// Not Rust's: `amd64` and `arm64`, never `x86_64` or `aarch64`.
fn go_arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => Ok("arm64"),
        "x86_64" => Ok("amd64"),
        other => Err(anyhow!("riabuild does not support {other} CPUs yet")),
    }
}

/// The GitHub CLI.
///
/// macOS is published as a **zip** and Linux as tar.gz. There is no macOS
/// tar.gz; the only other macOS asset is a `.pkg` installer that writes to
/// `/usr/local` with sudo. Note the capitalisation — `macOS`, not `darwin` or
/// `macos`, which is the opposite of what Infisical does two functions down.
pub fn gh() -> Result<Release> {
    let arch = go_arch()?;
    let (os, extension) = match std::env::consts::OS {
        "macos" => ("macOS", "zip"),
        "linux" => ("linux", "tar.gz"),
        other => return Err(anyhow!("riabuild does not support {other} yet")),
    };
    let asset = format!("gh_{GH_VERSION}_{os}_{arch}.{extension}");
    Ok(Release {
        tool: "gh",
        version: GH_VERSION,
        url: format!("https://github.com/cli/cli/releases/download/v{GH_VERSION}/{asset}"),
        // One file, covering every platform.
        checksum_urls: vec![format!(
            "https://github.com/cli/cli/releases/download/v{GH_VERSION}/gh_{GH_VERSION}_checksums.txt"
        )],
        asset,
        // Both containers wrap the tree in a directory named after the asset,
        // so the version is in the prefix and the member is matched by suffix.
        member: GH_MEMBER,
    })
}

/// The Infisical CLI.
///
/// Published from **`Infisical/cli`**, not `Infisical/infisical` — the CLI
/// moved out of the monorepo, which stopped publishing it at
/// `infisical-cli/v0.41.90`. Building the URL against the old repository pins a
/// CLI a year out of date without anything failing.
///
/// The asset is named `cli_…`, the binary inside it is named `infisical`, and
/// it sits at the archive root beside completions, manpages, and a README.
pub fn infisical() -> Result<Release> {
    let arch = go_arch()?;
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        other => return Err(anyhow!("riabuild does not support {other} yet")),
    };
    let asset = format!("cli_{INFISICAL_VERSION}_{os}_{arch}.tar.gz");
    let base = format!("https://github.com/Infisical/cli/releases/download/v{INFISICAL_VERSION}");
    Ok(Release {
        tool: "infisical",
        version: INFISICAL_VERSION,
        url: format!("{base}/{asset}"),
        // Two files, and the third — `cli_<version>_checksums.txt`, the one
        // named after the release — is a decoy holding a single line for
        // `windows_amd64`. darwin is built separately and lands in its own file.
        checksum_urls: vec![
            format!("{base}/checksums.txt"),
            format!("{base}/checksums-darwin.txt"),
        ],
        asset,
        member: INFISICAL_MEMBER,
    })
}

/// Downloads, verifies, and unpacks one tool into `~/.riabuild/<tool>/<version>`.
///
/// Safe to run twice: the destination is rewritten rather than appended to, and
/// a version that is already installed is simply reinstalled. Versioned
/// directories mean a bump installs beside the old copy rather than writing
/// over a binary that may be running.
pub async fn install(release: &Release, tool_dir: &Path) -> Result<PathBuf> {
    let expected = download::digest_from_any(&release.checksum_urls, &release.asset).await?;

    let bytes = download::fetch_bytes(&release.url).await?;
    let actual = download::sha256_hex(&bytes);
    if actual != expected {
        return Err(anyhow!(
            "{} downloaded from {} does not match its published checksum \
             (expected {expected}, got {actual}), so riabuild refused to install it",
            release.asset,
            release.url,
        ));
    }

    let binary = release.binary_in(tool_dir);
    archive::extract_member(&bytes, release.kind()?, release.member, &binary)?;
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_asset_names_match_what_the_release_publishes() {
        // Captured from cli/cli v2.97.0 on 2026-08-06. The point of asserting
        // the exact strings is that an upstream rename fails here rather than
        // as a 404 on a laptop.
        let release = gh().unwrap();
        let expected = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "gh_2.97.0_macOS_arm64.zip",
            ("macos", "x86_64") => "gh_2.97.0_macOS_amd64.zip",
            ("linux", "aarch64") => "gh_2.97.0_linux_arm64.tar.gz",
            ("linux", "x86_64") => "gh_2.97.0_linux_amd64.tar.gz",
            (os, arch) => panic!("no expectation recorded for {os}/{arch}"),
        };
        assert_eq!(release.asset, expected);
        assert!(release.url.ends_with(expected), "{}", release.url);
        assert!(
            release
                .url
                .starts_with("https://github.com/cli/cli/releases/download/v2.97.0/"),
            "{}",
            release.url
        );
    }

    #[test]
    fn gh_is_a_zip_on_macos_and_a_tarball_on_linux() {
        // There is no macOS tar.gz, which is the whole reason `archive.rs`
        // learned to read zips.
        let release = gh().unwrap();
        let expected = if cfg!(target_os = "macos") {
            Kind::Zip
        } else {
            Kind::TarGz
        };
        assert_eq!(release.kind().unwrap(), expected);
    }

    #[test]
    fn infisical_comes_from_the_repository_it_actually_lives_in() {
        // `Infisical/infisical` froze the CLI at infisical-cli/v0.41.90. Asking
        // it for 0.43.120 is a 404; asking it for a version it does have pins
        // something a year old.
        let release = infisical().unwrap();
        assert!(
            release
                .url
                .starts_with("https://github.com/Infisical/cli/releases/download/v0.43.120/"),
            "{}",
            release.url
        );
        assert!(
            !release.url.contains("Infisical/infisical"),
            "{}",
            release.url
        );
    }

    #[test]
    fn infisical_asset_names_match_what_the_release_publishes() {
        // Captured from Infisical/cli v0.43.120 on 2026-08-06. Note the asset
        // is `cli_…` while the binary inside is `infisical`.
        let release = infisical().unwrap();
        let expected = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "cli_0.43.120_darwin_arm64.tar.gz",
            ("macos", "x86_64") => "cli_0.43.120_darwin_amd64.tar.gz",
            ("linux", "aarch64") => "cli_0.43.120_linux_arm64.tar.gz",
            ("linux", "x86_64") => "cli_0.43.120_linux_amd64.tar.gz",
            (os, arch) => panic!("no expectation recorded for {os}/{arch}"),
        };
        assert_eq!(release.asset, expected);
    }

    #[test]
    fn infisical_looks_in_both_checksum_files() {
        // darwin digests are only ever in checksums-darwin.txt, and the file
        // named after the release covers windows_amd64 alone.
        let release = infisical().unwrap();
        assert_eq!(release.checksum_urls.len(), 2);
        assert!(release.checksum_urls[0].ends_with("/checksums.txt"));
        assert!(release.checksum_urls[1].ends_with("/checksums-darwin.txt"));
        assert!(
            !release
                .checksum_urls
                .iter()
                .any(|url| url.contains("cli_0.43.120_checksums.txt")),
            "the file named after the release holds one line, for windows"
        );
    }

    #[test]
    fn the_two_projects_spell_macos_differently() {
        // gh says `macOS`, infisical says `darwin`. Sharing one spelling
        // between them 404s on whichever guessed wrong.
        if cfg!(target_os = "macos") {
            assert!(gh().unwrap().asset.contains("macOS"));
            assert!(infisical().unwrap().asset.contains("darwin"));
        }
    }

    #[test]
    fn the_binary_lands_where_the_check_will_look_for_it() {
        let tool_dir = Path::new("/Users/ada/.riabuild/gh/2.97.0");
        assert_eq!(
            gh().unwrap().binary_in(tool_dir),
            PathBuf::from("/Users/ada/.riabuild/gh/2.97.0/bin/gh")
        );
        assert_eq!(
            infisical()
                .unwrap()
                .binary_in(Path::new("/Users/ada/.riabuild/infisical/0.43.120")),
            PathBuf::from("/Users/ada/.riabuild/infisical/0.43.120/infisical")
        );
    }

    #[test]
    fn architectures_are_gos_words_not_rusts() {
        let arch = go_arch().unwrap();
        assert!(["amd64", "arm64"].contains(&arch), "{arch}");
    }
}

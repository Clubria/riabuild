//! The third-party CLIs riabuild owns: `gh`, `infisical`, `ngrok` and `grok`.
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

mod install;
mod metadata;

// Re-exported so every caller keeps naming `tools::install` and
// `tools::github_release_metadata`. Which file they live in is this module's
// business, and a caller that had to know would have to be edited the next time
// one moves.
pub use install::install;
pub use metadata::github_release_metadata;

use crate::archive::Kind;
use crate::{Failure, TELL_YOUR_LEAD};
use anyhow::Result;
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
pub const NGROK_VERSION: &str = "3.39.11";
pub const GROK_VERSION: &str = "1.0.5";

/// Where riabuild republishes the ngrok builds it verified.
///
/// ngrok is the one tool here whose project pins nothing. Equinox serves a
/// single floating build per platform, and the version in the path is
/// decorative — on 2026-08-18 `ngrok-v9.99.9-stable-linux-amd64.tgz` returned
/// the same 12,104,579 bytes as `ngrok-v3-stable-linux-amd64.tgz`. There is no
/// immutable URL to pin and no checksum file to verify against.
///
/// So a maintainer mirrors: `packaging/ngrok/mirror.sh` downloads the four
/// builds, prints the version each reports and its digest, and uploads them
/// under this tag. The digests below are what it printed. See
/// `docs/superpowers/specs/2026-08-18-ngrok-design.md`.
const NGROK_MIRROR: &str = "https://github.com/Clubria/riabuild/releases/download/ngrok-v3.39.11";

/// Where riabuild republishes the Grok Build builds it verified.
///
/// The second tool to need a mirror, and it needs one for a different half of
/// the same reason. ngrok's problem is the URL: Equinox serves one floating
/// build per platform and the version in the path is decorative. xAI's URLs are
/// honest — `x.ai/cli/grok-1.0.5-linux-x86_64` names a real version, and
/// `grok-9.99.9-linux-x86_64` is a 404 rather than the current bytes under
/// another name, checked on 2026-08-21. What xAI publishes nowhere is a
/// **digest**: `x.ai/cli/install.sh` downloads the binary, runs `--version`
/// against it, and installs it, and no checksum file exists at any spelling
/// beside the artifact.
///
/// So the rule in `CLAUDE.md` applies unchanged — where a project publishes no
/// digest, riabuild republishes the artifact rather than lowering the bar. A
/// maintainer runs `packaging/grok/mirror.sh`, which downloads the four builds,
/// prints each digest, and uploads them under this tag; the digests below are
/// what it printed.
///
/// Pinning riabuild's own digest against xAI's URL instead was the alternative,
/// and it fails in the worse direction. A version xAI re-cuts under the same
/// name becomes a checksum mismatch and a hard install failure on every laptop
/// at once, for bytes nobody can fetch any more — whereas a mirror riabuild
/// holds keeps working and can be re-verified against upstream at leisure.
///
/// The assets are large: 134 to 167 MB each, about 588 MB per mirrored version.
/// So mirror tags stay rare — one per version bump, which is a code change
/// anyway.
const GROK_MIRROR: &str = "https://github.com/Clubria/riabuild/releases/download/grok-v1.0.5";

/// Where each binary sits inside its archive — and, once installed, inside
/// `~/.riabuild/<tool>/<version>/`. One constant for both, so the path a task
/// runs and the path `install` writes cannot drift apart.
pub const GH_MEMBER: &str = "bin/gh";
pub const INFISICAL_MEMBER: &str = "infisical";
pub const NGROK_MEMBER: &str = "ngrok";
/// Grok Build's download is the binary itself, so there is nothing to look
/// inside — this only names where it lands under `~/.riabuild/grok/<version>/`.
pub const GROK_MEMBER: &str = "grok";

/// How riabuild learns what the download is supposed to hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    /// Fetched from the files the project publishes beside the artifact. Tried
    /// in order — more than one because Infisical splits them, see
    /// `download::digest_from_any`.
    Published(Vec<String>),
    /// Recorded in this repository, because upstream publishes none.
    ///
    /// Only for a tool riabuild mirrors itself. A digest that arrived over the
    /// network alongside the artifact proves less than one committed here, and
    /// a digest the *server* chose would pick which bytes execute on a laptop —
    /// which is the task manifest under another name. `Formula/riabuild.rb`
    /// pins riabuild's own releases exactly this way.
    Pinned(&'static str),
}

/// Everything needed to fetch one tool and find its binary afterwards.
#[derive(Debug, Clone)]
pub struct Release {
    /// Names the directory under `~/.riabuild/`.
    pub tool: &'static str,
    pub version: &'static str,
    pub asset: String,
    pub url: String,
    pub checksum: Checksum,
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
        other => Err(unsupported(format!("a {other} CPU"))),
    }
}

/// A machine none of these projects publishes a build for. The developer can do
/// nothing about it and a re-run will decide the same thing, so the one action
/// is to tell somebody.
fn unsupported(what: String) -> anyhow::Error {
    Failure::new(
        format!("choosing the tools for this machine — riabuild does not support {what} yet"),
        TELL_YOUR_LEAD,
    )
    .into()
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
        other => return Err(unsupported(format!("the {other} operating system"))),
    };
    let asset = format!("gh_{GH_VERSION}_{os}_{arch}.{extension}");
    Ok(Release {
        tool: "gh",
        version: GH_VERSION,
        url: format!("https://github.com/cli/cli/releases/download/v{GH_VERSION}/{asset}"),
        // One file, covering every platform.
        checksum: Checksum::Published(vec![format!(
            "https://github.com/cli/cli/releases/download/v{GH_VERSION}/gh_{GH_VERSION}_checksums.txt"
        )]),
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
        other => return Err(unsupported(format!("the {other} operating system"))),
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
        checksum: Checksum::Published(vec![
            format!("{base}/checksums.txt"),
            format!("{base}/checksums-darwin.txt"),
        ]),
        asset,
        member: INFISICAL_MEMBER,
    })
}

/// ngrok, from riabuild's own mirror.
///
/// The asset keeps the container upstream published it in — darwin a zip,
/// linux a tgz — so `Kind::of` reads it without a special case and nothing has
/// to trust a repacking step. Each archive holds one file, `ngrok`, at its
/// root.
///
/// The digest is per platform and pinned here. See [`NGROK_MIRROR`] for why
/// there is nothing to fetch it from.
pub fn ngrok() -> Result<Release> {
    let arch = go_arch()?;
    let (os, extension, digest) = match (std::env::consts::OS, arch) {
        ("macos", "arm64") => (
            "darwin",
            "zip",
            "9324a6552d74e25d5bdfdbedc4b32422c96f044fda37877498ad8ef10bddf7f7",
        ),
        ("macos", "amd64") => (
            "darwin",
            "zip",
            "c6b9b3d9184fc08c33fb8b181d9f241d8f5d61162a0be0521b6dfc1f11813a96",
        ),
        ("linux", "arm64") => (
            "linux",
            "tgz",
            "3b6ba05a9d9585c34157fa0819fa95cdb13839f5b506b9e63204705cf7f79e29",
        ),
        ("linux", "amd64") => (
            "linux",
            "tgz",
            "cec0b4997fcc5f529dfc74bac89050354d11a915f968720600039738fdf330cf",
        ),
        (os, arch) => return Err(unsupported(format!("{os} on {arch}"))),
    };
    let asset = format!("ngrok-{NGROK_VERSION}-{os}-{arch}.{extension}");
    Ok(Release {
        tool: "ngrok",
        version: NGROK_VERSION,
        url: format!("{NGROK_MIRROR}/{asset}"),
        checksum: Checksum::Pinned(digest),
        asset,
        member: NGROK_MEMBER,
    })
}

/// The four builds riabuild mirrors, and the digest each must hash to.
///
/// A table rather than four arms inside `grok()`, because the mistake worth
/// guarding against is not "this platform is missing" — that is a compile-time
/// `match` and a 404 — but *one digest copy-pasted across two rows*. That
/// survives every other test in this file: the release builds fine, the URL is
/// right, and the install fails with a checksum mismatch on exactly one kind of
/// laptop, which is the kind nobody in the room is holding. As data it can be
/// checked whole, on every host, by a test that does not care which platform it
/// is running on.
///
/// `(os, arch, sha256)`, in the platform words **xAI** uses — see `grok()`.
/// Regenerate with `packaging/grok/mirror.sh`.
const GROK_BUILDS: &[(&str, &str, &str)] = &[
    (
        "macos",
        "aarch64",
        "3dfa7f04fbb5427a8fbead286591543aaecb478b3a0ab222c4329eca1a3b2f86",
    ),
    (
        "macos",
        "x86_64",
        "21cbb063c6167175ba00a67f64ac638af8f79a44aef816cfd5b4915c77528e60",
    ),
    (
        "linux",
        "aarch64",
        "1c1fe67d7c35497fb09f44a451f57acc3787add4c9aea2c56f5c7c75dc5ffcf1",
    ),
    (
        "linux",
        "x86_64",
        "9ba87444e1819e8f6104adbbf4676a870c204380aa5c3e1c38a926c4ea677238",
    ),
];

/// Grok Build, from riabuild's own mirror.
///
/// The one tool here that ships **no container**: the download is an
/// uncompressed executable, which is why `Kind::Raw` exists. The mirrored asset
/// carries a `.bin` suffix so `Kind::of` can still read the container off the
/// name, and renaming a file does not change its bytes — the digest is still
/// the digest of what xAI served.
///
/// Note the platform words. gh, Infisical and ngrok all use Go's — `amd64` and
/// `arm64` — and this one uses Rust's, because Grok Build is a Rust program and
/// xAI names its artifacts after the target triple's halves: `linux-x86_64`,
/// `macos-aarch64`. Reaching for `go_arch()` here builds a URL that 404s on
/// every machine, and nothing else in the codebase would notice.
pub fn grok() -> Result<Release> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let &(_, _, digest) = GROK_BUILDS
        .iter()
        .find(|(build_os, build_arch, _)| *build_os == os && *build_arch == arch)
        .ok_or_else(|| unsupported(format!("{os} on {arch}")))?;

    let asset = format!("grok-{GROK_VERSION}-{os}-{arch}.bin");
    Ok(Release {
        tool: "grok",
        version: GROK_VERSION,
        url: format!("{GROK_MIRROR}/{asset}"),
        checksum: Checksum::Pinned(digest),
        asset,
        member: GROK_MEMBER,
    })
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
        let Checksum::Published(urls) = release.checksum else {
            panic!("infisical publishes checksum files");
        };
        assert_eq!(urls.len(), 2);
        assert!(urls[0].ends_with("/checksums.txt"));
        assert!(urls[1].ends_with("/checksums-darwin.txt"));
        assert!(
            !urls
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

    #[test]
    fn ngrok_comes_from_riabuilds_own_mirror() {
        // Equinox serves one floating build per platform: on 2026-08-18
        // `ngrok-v9.99.9-stable-linux-amd64.tgz` returned the same bytes as
        // `ngrok-v3-stable-linux-amd64.tgz`. Pointing a laptop at that URL pins
        // nothing, so riabuild republishes the artifact it verified.
        let release = ngrok().unwrap();
        assert!(
            release.url.starts_with(
                "https://github.com/Clubria/riabuild/releases/download/ngrok-v3.39.11/"
            ),
            "{}",
            release.url
        );
        assert!(!release.url.contains("equinox.io"), "{}", release.url);
    }

    #[test]
    fn ngrok_asset_names_match_the_mirror() {
        // The mirrored assets keep the container each upstream build arrives
        // in — darwin is a zip, linux a tgz — so `Kind::of` reads them without
        // a special case, and repacking never has to be trusted.
        let release = ngrok().unwrap();
        let expected = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "ngrok-3.39.11-darwin-arm64.zip",
            ("macos", "x86_64") => "ngrok-3.39.11-darwin-amd64.zip",
            ("linux", "aarch64") => "ngrok-3.39.11-linux-arm64.tgz",
            ("linux", "x86_64") => "ngrok-3.39.11-linux-amd64.tgz",
            (os, arch) => panic!("no expectation recorded for {os}/{arch}"),
        };
        assert_eq!(release.asset, expected);
        assert!(release.url.ends_with(expected), "{}", release.url);
    }

    #[test]
    fn ngrok_is_verified_against_a_digest_recorded_in_this_repository() {
        // The whole reason for the mirror. There is no checksum file to fetch,
        // and a digest riabuild-web could choose would select which bytes
        // execute on a laptop.
        let release = ngrok().unwrap();
        match release.checksum {
            Checksum::Pinned(digest) => {
                assert_eq!(digest.len(), 64, "{digest}");
                assert!(
                    digest
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                    "{digest}"
                );
            }
            Checksum::Published(urls) => panic!("ngrok publishes no checksums, got {urls:?}"),
        }
    }

    #[test]
    fn the_tools_that_publish_checksums_still_fetch_them() {
        // The enum must not quietly turn gh and infisical into pinned digests:
        // theirs move with every upstream release and are verified from the
        // files those projects publish.
        for release in [gh().unwrap(), infisical().unwrap()] {
            assert!(
                matches!(release.checksum, Checksum::Published(_)),
                "{} should still fetch its checksums",
                release.tool
            );
        }
    }

    #[test]
    fn grok_comes_from_riabuilds_own_mirror() {
        // xAI's URLs name a real version and 404 on one nobody published, so
        // unlike ngrok the problem is not a floating download — it is that no
        // digest is published anywhere beside the artifact. riabuild
        // republishes the bytes it verified rather than trusting a URL it
        // cannot check.
        let release = grok().unwrap();
        assert!(
            release
                .url
                .starts_with("https://github.com/Clubria/riabuild/releases/download/grok-v1.0.5/"),
            "{}",
            release.url
        );
        assert!(!release.url.contains("x.ai"), "{}", release.url);
    }

    #[test]
    fn grok_asset_names_use_rusts_platform_words_and_not_gos() {
        // The trap this exists to catch. Every other tool here is a Go program
        // published as `amd64`/`arm64`; Grok Build is a Rust program and xAI
        // names its artifacts `linux-x86_64` and `macos-aarch64`. Reaching for
        // `go_arch()` builds a URL that 404s on every machine, and nothing else
        // in the codebase would notice.
        let release = grok().unwrap();
        let expected = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "grok-1.0.5-macos-aarch64.bin",
            ("macos", "x86_64") => "grok-1.0.5-macos-x86_64.bin",
            ("linux", "aarch64") => "grok-1.0.5-linux-aarch64.bin",
            ("linux", "x86_64") => "grok-1.0.5-linux-x86_64.bin",
            (os, arch) => panic!("no expectation recorded for {os}/{arch}"),
        };
        assert_eq!(release.asset, expected);
        assert!(release.url.ends_with(expected), "{}", release.url);
        assert!(!release.asset.contains("amd64"), "{}", release.asset);
        assert!(!release.asset.contains("arm64"), "{}", release.asset);
    }

    #[test]
    fn grok_arrives_as_a_bare_binary_rather_than_in_a_container() {
        // The download *is* the executable. A `.tar.gz` or `.zip` here would
        // mean `mirror.sh` repacked it, and the pinned digest would then
        // describe riabuild's own output instead of the bytes xAI served.
        assert_eq!(grok().unwrap().kind().unwrap(), Kind::Raw);
    }

    #[test]
    fn grok_is_verified_against_a_digest_recorded_in_this_repository() {
        // xAI's own installer verifies nothing: it downloads, runs
        // `--version`, and installs. riabuild does not lower the bar to match.
        let release = grok().unwrap();
        match release.checksum {
            Checksum::Pinned(digest) => {
                assert_eq!(digest.len(), 64, "{digest}");
                assert!(
                    digest
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                    "{digest}"
                );
            }
            Checksum::Published(urls) => panic!("xAI publishes no checksums, got {urls:?}"),
        }
    }

    #[test]
    fn every_mirrored_platform_has_its_own_digest() {
        // One digest copy-pasted across two rows is the mistake that survives
        // every other test here: the release builds fine, the URL is right, and
        // the install fails with a checksum mismatch on exactly one kind of
        // laptop. Asserted over the table rather than through `grok()`, which
        // can only ever answer for the host it is running on.
        let digests: std::collections::BTreeSet<&str> =
            GROK_BUILDS.iter().map(|(_, _, digest)| *digest).collect();
        assert_eq!(digests.len(), GROK_BUILDS.len(), "{GROK_BUILDS:?}");

        let platforms: std::collections::BTreeSet<(&str, &str)> = GROK_BUILDS
            .iter()
            .map(|(os, arch, _)| (*os, *arch))
            .collect();
        assert_eq!(platforms.len(), GROK_BUILDS.len(), "{GROK_BUILDS:?}");

        // The four xAI publishes. A fifth row, or a missing one, means the
        // mirror and this table disagree about what was uploaded.
        assert_eq!(
            platforms,
            [
                ("linux", "aarch64"),
                ("linux", "x86_64"),
                ("macos", "aarch64"),
                ("macos", "x86_64"),
            ]
            .into_iter()
            .collect()
        );

        for (os, arch, digest) in GROK_BUILDS {
            assert_eq!(digest.len(), 64, "{os}/{arch}: {digest}");
            assert!(
                digest
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{os}/{arch}: {digest}"
            );
        }
    }

    #[test]
    fn the_grok_binary_lands_where_the_check_will_look_for_it() {
        assert_eq!(
            grok()
                .unwrap()
                .binary_in(Path::new("/Users/ada/.riabuild/grok/1.0.5")),
            PathBuf::from("/Users/ada/.riabuild/grok/1.0.5/grok")
        );
    }

    #[test]
    fn the_ngrok_binary_lands_where_the_check_will_look_for_it() {
        assert_eq!(
            ngrok()
                .unwrap()
                .binary_in(Path::new("/Users/ada/.riabuild/ngrok/3.39.11")),
            PathBuf::from("/Users/ada/.riabuild/ngrok/3.39.11/ngrok")
        );
    }
}

//! Where each release lives, and what its asset is called on this machine.
//!
//! Names and URLs only — nothing here reaches the network. Every upstream
//! project spells a platform its own way, and this is the one place riabuild
//! knows which: `macOS` against `darwin`, `arm64` against `aarch64`, and a
//! `uname -sm` from a server that is frequently not the laptop driving it.

use crate::{Failure, TELL_YOUR_LEAD};
use anyhow::Result;

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
        other => return Err(unsupported(format!("a {other} CPU"))),
    };
    match uname_s.trim() {
        "Darwin" => Ok(format!("{arch}-apple-darwin")),
        // musl rather than gnu: one Linux build then runs on any distribution
        // instead of only on distributions with a glibc at least as new as the
        // one the release runner happened to build against.
        "Linux" => Ok(format!("{arch}-unknown-linux-musl")),
        other => Err(unsupported(format!("the {other} operating system"))),
    }
}

/// A machine riabuild publishes nothing for.
///
/// A `Failure` rather than a bare error because the developer needs to know it
/// is their machine that is unsupported rather than riabuild that is broken —
/// and because the only thing they can do about it is tell somebody.
fn unsupported(what: String) -> anyhow::Error {
    Failure::new(
        format!("choosing the download for this machine — riabuild does not support {what} yet"),
        TELL_YOUR_LEAD,
    )
    .into()
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
        other => return Err(unsupported(format!("the {other} operating system"))),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => return Err(unsupported(format!("a {other} CPU"))),
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
        other => return Err(unsupported(format!("the {other} operating system"))),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => return Err(unsupported(format!("a {other} CPU"))),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_riabuild_has_no_build_for_is_told_so_in_words() {
        for error in [
            rust_target("Plan9", "x86_64").expect_err("os"),
            rust_target("Linux", "i686").expect_err("cpu"),
        ] {
            let failure = error.downcast_ref::<Failure>().expect("a Failure");
            assert!(failure.attempting.contains("does not support"), "{failure}");
            assert!(!failure.action.is_empty());
        }
    }

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

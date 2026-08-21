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

/// Where pnpm comes from.
///
/// **Not** `github.com/pnpm/pnpm/releases`, which is where riabuild used to
/// take it from. pnpm's releases carry no checksum file, so the only digest
/// GitHub has for them is the one its REST API records per asset — and that API
/// allows sixty unauthenticated requests an hour *per address*. A team behind
/// one NAT is the ordinary case for the office this tool provisions, and it
/// exhausts that budget; provisioning then stops for everyone with
/// `asking GitHub what … contains`, which is what both e2e jobs hit.
///
/// A mirror is what `../../../../CLAUDE.md` prescribes where a project
/// publishes no digest, and it cannot serve here: ngrok and Grok Build are
/// pinned to one version in this repository, so a `Checksum::Pinned` constant
/// can name their bytes, while pnpm's version is read out of the checkout's
/// `packageManager` at runtime and no constant can describe a version nobody
/// has chosen yet.
///
/// The npm registry answers both objections at once. Every published version
/// carries a `dist.integrity` — the sha512 npm itself recorded over the stored
/// tarball, the same field every `npm install` on earth already verifies
/// against, with an SLSA provenance attestation beside it — and it is served
/// with no API budget to run out of. It is a digest a **publisher** records,
/// not one riabuild-web supplies, so nothing here moves the choice of what
/// executes onto a server riabuild controls.
const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// The npm package holding the JavaScript bundle a pnpm 11 launcher loads.
///
/// Its own `pnpm` file is a placeholder reading *"This file intentionally left
/// blank"* — upstream's install script copies the platform binary over it — so
/// it is unpacked **first** and the platform package lands on top. Reversing
/// the order installs the placeholder as pnpm.
pub const PNPM_BUNDLE_PACKAGE: &str = "@pnpm/exe";

/// Whether the platform package needs [`PNPM_BUNDLE_PACKAGE`] beside it.
///
/// pnpm 10 and older publish one self-contained executable per platform:
/// `@pnpm/linux-x64@10.20.0` unpacks to a 65 MB binary that runs on its own.
/// pnpm 11 split the JavaScript out, so `@pnpm/linux-x64@11.11.0` is a Node
/// launcher that resolves `dist/pnpm.mjs` **beside itself** and exits with
/// `Cannot find module` when it is not there. The two halves have to land in
/// one directory.
///
/// The boundary is the version asked for rather than today's date, because the
/// registry still serves each published version exactly as it was published.
pub fn pnpm_needs_the_bundle(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        // An unparseable pin is likelier to be something new than something
        // ancient, and the split is what pnpm publishes now.
        .is_none_or(|major| major >= 11)
}

/// The `@pnpm/<platform>` package carrying the launcher for this machine.
///
/// Unversioned, unlike the GitHub asset name it replaces: npm has spelled
/// macOS `macos` at every version, where the release assets renamed it to
/// `darwin` at pnpm 11 and made asking for the old name a 404.
///
/// `linux-x64` rather than `linuxstatic-x64`, which the registry also carries:
/// the glibc build is the one the GitHub releases served and the one riabuild
/// has always installed, and swapping a developer's pnpm for a musl build is
/// not a change this is making.
pub fn pnpm_platform_package() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        other => return Err(unsupported(format!("the {other} operating system"))),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => return Err(unsupported(format!("a {other} CPU"))),
    };
    Ok(format!("@pnpm/{os}-{arch}"))
}

/// One version's document in the registry: a couple of kilobytes of JSON whose
/// `dist.integrity` is the digest riabuild verifies against.
///
/// The version is in the path rather than the whole packument being read and
/// filtered, because `@pnpm/macos-arm64` alone has 547 of them.
pub fn npm_metadata_url(package: &str, version: &str) -> String {
    format!("{NPM_REGISTRY}/{package}/{version}")
}

/// Where the registry stores one version's tarball. A scoped package drops its
/// scope from the filename: `@pnpm/macos-arm64` is served at
/// `…/@pnpm/macos-arm64/-/macos-arm64-11.11.0.tgz`.
///
/// Built here rather than read out of the metadata's `dist.tarball`, which
/// names the same URL. The digest and the bytes then come from one host this
/// file names, and a metadata document cannot redirect the download somewhere
/// riabuild never chose.
pub fn npm_tarball_url(package: &str, version: &str) -> String {
    let unscoped = package.rsplit_once('/').map_or(package, |(_, name)| name);
    format!("{NPM_REGISTRY}/{package}/-/{unscoped}-{version}.tgz")
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
    }

    #[test]
    fn builds_the_registry_urls_npm_actually_serves() {
        // Captured from the registry on 2026-08-21: the metadata document and
        // the `dist.tarball` it names. A scoped package drops its scope from
        // the filename, which is the half that is easy to get wrong.
        assert_eq!(
            npm_metadata_url("@pnpm/macos-arm64", "11.11.0"),
            "https://registry.npmjs.org/@pnpm/macos-arm64/11.11.0"
        );
        assert_eq!(
            npm_tarball_url("@pnpm/macos-arm64", "11.11.0"),
            "https://registry.npmjs.org/@pnpm/macos-arm64/-/macos-arm64-11.11.0.tgz"
        );
        assert_eq!(
            npm_tarball_url(PNPM_BUNDLE_PACKAGE, "11.11.0"),
            "https://registry.npmjs.org/@pnpm/exe/-/exe-11.11.0.tgz"
        );
        // An unscoped package keeps its whole name in the filename.
        assert_eq!(
            npm_tarball_url("pnpm", "11.11.0"),
            "https://registry.npmjs.org/pnpm/-/pnpm-11.11.0.tgz"
        );
    }

    #[test]
    fn nothing_riabuild_downloads_pnpm_from_is_a_rate_limited_api() {
        // The regression this whole path exists to prevent: the digest used to
        // come from `api.github.com`, which allows sixty unauthenticated
        // requests an hour per address, and a team behind one NAT ran out.
        for url in [
            npm_metadata_url("@pnpm/linux-x64", "11.11.0"),
            npm_tarball_url("@pnpm/linux-x64", "11.11.0"),
        ] {
            assert!(url.starts_with(NPM_REGISTRY), "{url}");
            assert!(!url.contains("api.github.com"), "{url}");
        }
    }

    #[test]
    fn pnpm_11_needs_the_bundle_beside_the_launcher_and_pnpm_10_does_not() {
        // pnpm 10's platform artifact is a self-contained 65 MB executable;
        // pnpm 11's is a launcher that resolves `dist/pnpm.mjs` beside itself
        // and exits with `Cannot find module` when it is not there.
        assert!(pnpm_needs_the_bundle("11.11.0"));
        assert!(pnpm_needs_the_bundle("11.0.0"));
        assert!(pnpm_needs_the_bundle("12.0.0"));
        assert!(!pnpm_needs_the_bundle("10.34.5"));
        assert!(!pnpm_needs_the_bundle("9.15.9"));
        // Something unrecognisable is likelier to be new than ancient.
        assert!(pnpm_needs_the_bundle("next"));
    }

    #[test]
    fn the_platform_package_is_one_npm_publishes_and_does_not_move_with_the_version() {
        // The host decides the platform, so the set is asserted rather than
        // one member of it. npm spells macOS `macos` at every version — the
        // GitHub assets renamed it to `darwin` at pnpm 11, and carrying that
        // rename over here would ask the registry for a package that has never
        // existed.
        let package = pnpm_platform_package().unwrap();
        assert!(
            [
                "@pnpm/macos-arm64",
                "@pnpm/macos-x64",
                "@pnpm/linux-arm64",
                "@pnpm/linux-x64",
            ]
            .contains(&package.as_str()),
            "unexpected package {package}"
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

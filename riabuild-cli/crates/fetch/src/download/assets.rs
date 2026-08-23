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

/// The npm package that **is** pnpm, and the file inside it that starts it.
///
/// One package, at every version pnpm has published, run on the Node riabuild
/// already owns — `exec "<node>" "<tree>/bin/pnpm.cjs" "$@"`. Not
/// `@pnpm/<platform>`, the standalone executable, and the reason is a shared
/// library rather than a preference.
///
/// **pnpm's glibc Linux build needs `libatomic.so.1`, and a stock Linux does
/// not have it.** Nothing in `debian:bookworm-slim`, `debian:12`, `ubuntu:22.04`
/// or `fedora:41` ships that file — it arrives with a toolchain, and the
/// machines this provisions are the ones with no toolchain on them yet. Node's
/// own binaries do not link it, so the failure is exquisitely misleading: Node
/// installs and answers `-v`, pnpm installs and exits **127** with `error while
/// loading shared libraries`, and `toolchain`'s `check()` reads a non-zero exit
/// as "pnpm is not installed yet". `apply()` then downloads 146 MB, unpacks it
/// perfectly, and the re-check says the same thing — the
/// apply-did-not-take-effect hard error, on every run, for ever, on a machine
/// where nothing is wrong except a library nobody named. That is the shape
/// `../../../../CLAUDE.md`'s Codex CLI section describes, one missing thing
/// over, and `e2e/remote/run.sh` hit it against its Debian container.
///
/// `@pnpm/linuxstatic-<arch>` is not the answer, however much the name reads
/// like one: it is built against **musl** and asks for
/// `/lib/ld-musl-x86_64.so.1`, so on the glibc distributions this is about it
/// does not fail to find a library, it fails to start at all. There is no
/// pnpm executable that runs on a bare Linux.
///
/// So riabuild runs pnpm's JavaScript on its own Node, which is the one
/// interpreter on that machine riabuild downloaded, verified and can vouch
/// for — the same thing `codex_cli` does with `@openai/codex`, and it holds
/// the same way on macOS. "riabuild owns every tool it installs" cannot
/// survive installing a binary whose runtime comes from `apt`.
///
/// `bin/pnpm.cjs` rather than `bin/pnpm.mjs`: the `.cjs` is the declared
/// `bin.pnpm` up to pnpm 10 and is kept as an entry point at 11 (where it does
/// nothing but `import('./pnpm.mjs')`), so it is the one filename every
/// published version answers to, and there is no version branch here to get
/// wrong. Either way pnpm's own launcher, not riabuild, is what reports a Node
/// too old for it — with the version it wants and a link.
pub const PNPM_PACKAGE: &str = "pnpm";

/// The file inside [`PNPM_PACKAGE`] that Node is pointed at. See there.
pub const PNPM_ENTRY: &str = "bin/pnpm.cjs";

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
        // An unscoped package keeps its whole name in the filename, which is
        // the case riabuild actually takes.
        assert_eq!(
            npm_tarball_url(PNPM_PACKAGE, "11.11.0"),
            "https://registry.npmjs.org/pnpm/-/pnpm-11.11.0.tgz"
        );
    }

    /// pnpm is one unscoped package at every version, and the same entry file
    /// at every version — no platform in the name and no version branch, which
    /// is the whole of why the standalone executable is gone.
    ///
    /// Captured from the registry on 2026-08-21: `pnpm@9.15.9` and
    /// `pnpm@10.20.0` declare `bin.pnpm` as `bin/pnpm.cjs`; `pnpm@11.22.0`
    /// declares `bin/pnpm.mjs` and ships `bin/pnpm.cjs` beside it as a
    /// one-line `import('./pnpm.mjs')`. So `.cjs` is the filename all three
    /// answer to, and `node bin/pnpm.cjs -v` prints `11.22.0` on the 11.
    #[test]
    fn pnpm_is_one_package_and_one_entry_file_at_every_version() {
        for version in ["9.15.9", "10.20.0", "11.22.0"] {
            assert_eq!(
                npm_tarball_url(PNPM_PACKAGE, version),
                format!("https://registry.npmjs.org/pnpm/-/pnpm-{version}.tgz")
            );
            assert_eq!(
                npm_metadata_url(PNPM_PACKAGE, version),
                format!("https://registry.npmjs.org/pnpm/{version}")
            );
        }
        // Relative, and inside the tree: it is joined onto `pnpm_dir(version)`
        // to name what Node is handed.
        assert!(!PNPM_ENTRY.starts_with('/'), "{PNPM_ENTRY}");
        assert!(!PNPM_ENTRY.contains(".."), "{PNPM_ENTRY}");
    }

    /// The regression that makes this whole item worth having: nothing riabuild
    /// installs may need a library the developer has to `apt-get`.
    ///
    /// `@pnpm/linux-x64@11.22.0` — the standalone executable riabuild used to
    /// install — is `NEEDED: libatomic.so.1`, which `debian:bookworm-slim`,
    /// `debian:12`, `ubuntu:22.04` and `fedora:41` all lack, and
    /// `@pnpm/linuxstatic-x64` asks for `/lib/ld-musl-x86_64.so.1` instead. A
    /// pnpm that is a *platform* package is a pnpm with a runtime riabuild did
    /// not install.
    #[test]
    fn the_pnpm_riabuild_installs_is_never_a_platform_executable() {
        assert!(!PNPM_PACKAGE.contains('/'), "{PNPM_PACKAGE}");
        for platform in ["linux", "linuxstatic", "macos", "win", "x64", "arm64"] {
            assert!(!PNPM_PACKAGE.contains(platform), "{PNPM_PACKAGE}");
        }
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

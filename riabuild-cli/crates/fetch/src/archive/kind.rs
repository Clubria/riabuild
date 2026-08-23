//! Which container an upstream release arrived in.
//!
//! One question, read off the asset name and nowhere else, so the name and the
//! container cannot drift apart. Everything that unpacks is told the answer
//! rather than guessing at it.

use crate::{Failure, TELL_YOUR_LEAD};
use anyhow::Result;

/// The container an upstream release happens to use.
///
/// `gh` publishes Linux as tar.gz and macOS as zip — there is no macOS tar.gz,
/// and the only other macOS asset is a `.pkg` installer that writes to
/// `/usr/local` with sudo. So the container is a property of the asset rather
/// than of riabuild, and both have to be supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    TarGz,
    Zip,
    /// No container: the download *is* the binary.
    ///
    /// Grok Build is the one that forces this. xAI serves an uncompressed
    /// executable straight from `https://x.ai/cli/grok-<version>-<platform>`,
    /// with no tarball, no zip, and nothing beside it. The two ways to avoid a
    /// third variant were both worse. Repacking it into a tarball in
    /// `packaging/grok/mirror.sh` would mean the digest pinned in `tools.rs` is
    /// the digest of *riabuild's repack* rather than of the bytes xAI served,
    /// which puts an unverifiable transformation between what a maintainer
    /// checked and what a laptop runs — the opposite of what pinning is for.
    /// Downloading it outside `tools::install` would put a second, unverified
    /// fetch path in the codebase.
    ///
    /// So the bytes are mirrored byte-for-byte and this reads them straight
    /// through. `member` is not consulted for a `Raw` asset, because there is
    /// no archive to look inside.
    Raw,
}

impl Kind {
    /// Picked from the asset name, so it stays wrong-proof when a new asset is
    /// added: the name and the container cannot drift apart.
    ///
    /// `Raw` is spelled `.bin` rather than inferred from "no extension I
    /// recognise". Inferring it would make every future typo and every asset in
    /// a container riabuild has not learned yet — a `.pkg`, a `.deb`, an
    /// `.xz` — install as though it were an executable, which fails at the
    /// moment the developer runs it rather than here. Renaming a file does not
    /// change its bytes, so the mirrored `.bin` still hashes to exactly what
    /// upstream served.
    pub fn of(asset: &str) -> Result<Self> {
        if asset.ends_with(".tar.gz") || asset.ends_with(".tgz") {
            Ok(Kind::TarGz)
        } else if asset.ends_with(".zip") {
            Ok(Kind::Zip)
        } else if asset.ends_with(".bin") {
            Ok(Kind::Raw)
        } else {
            Err(Failure::new(
                format!("unpacking {asset} — riabuild does not know what container that is"),
                TELL_YOUR_LEAD,
            )
            .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_is_read_off_the_asset_name() {
        assert_eq!(
            Kind::of("gh_2.97.0_linux_amd64.tar.gz").unwrap(),
            Kind::TarGz
        );
        assert_eq!(Kind::of("gh_2.97.0_macOS_arm64.zip").unwrap(), Kind::Zip);
        assert!(Kind::of("gh_2.97.0_macOS_universal.pkg").is_err());
    }

    #[test]
    fn a_bare_binary_is_recognised_by_its_name_and_nothing_else() {
        // The whole point of spelling `Raw` as `.bin`: a container riabuild has
        // not learned yet must fail here, not install as an executable and fail
        // when the developer runs it.
        assert_eq!(Kind::of("grok-1.0.5-linux-x86_64.bin").unwrap(), Kind::Raw);
        assert_eq!(
            Kind::of("ngrok-3.39.11-linux-amd64.tgz").unwrap(),
            Kind::TarGz
        );
        assert!(Kind::of("grok-1.0.5-linux-x86_64").is_err());
        assert!(Kind::of("grok.pkg").is_err());
        assert!(Kind::of("grok.deb").is_err());
    }
}

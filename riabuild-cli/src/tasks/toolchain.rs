//! Task 4 — riabuild-owned Node and pnpm.
//!
//! The versions come from the repo (`.nvmrc` and `packageManager`), which is why
//! this declares a dependency on `project` even though the design table shows
//! none: a check that reads files out of the checkout cannot run before the
//! checkout exists. An undeclared edge means running against stale state.

use super::{Ctx, Status, Task, TaskId};
use crate::download;
use crate::runner::RunOptions;
use crate::shims;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use std::path::Path;

/// Used when the repo pins nothing. Kept current with the repo's own `.nvmrc`.
const FALLBACK_NODE: &str = "22.23.1";
const FALLBACK_PNPM: &str = "11.11.0";

pub struct Toolchain;

/// Reads the Node version the repo asks for.
pub fn desired_node(project: Option<&Path>) -> String {
    project
        .map(|dir| dir.join(".nvmrc"))
        .and_then(|file| std::fs::read_to_string(file).ok())
        .map(|text| text.trim().trim_start_matches('v').to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| FALLBACK_NODE.to_string())
}

/// Reads the pnpm version out of `"packageManager": "pnpm@10.20.0"`.
pub fn desired_pnpm(project: Option<&Path>) -> String {
    let Some(text) = project
        .map(|dir| dir.join("package.json"))
        .and_then(|file| std::fs::read_to_string(file).ok())
    else {
        return FALLBACK_PNPM.to_string();
    };

    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|json| {
            json.get("packageManager")?
                .as_str()?
                .strip_prefix("pnpm@")
                .map(|version| version.split('+').next().unwrap_or(version).to_string())
        })
        .unwrap_or_else(|| FALLBACK_PNPM.to_string())
}

impl Task for Toolchain {
    fn id(&self) -> TaskId {
        "toolchain"
    }

    fn title(&self) -> &str {
        "Node and pnpm"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &["project"]
    }

    fn check(&self, ctx: &Ctx) -> Result<Status> {
        let project = ctx.project_dir();
        let node_version = desired_node(project.as_deref());
        let pnpm_version = desired_pnpm(project.as_deref());

        let node_bin = ctx.paths.node_dir(&node_version).join("bin").join("node");
        if !node_bin.exists() {
            return Ok(Status::needs(format!(
                "Node {node_version} is not installed yet"
            )));
        }

        let reported =
            ctx.runner
                .run(&node_bin.to_string_lossy(), &["-v"], &RunOptions::default())?;
        if !version::same(reported.trimmed(), &node_version) {
            return Ok(Status::needs(format!(
                "the Node in ~/.riabuild reports {} but the repo asks for {node_version}",
                reported.trimmed()
            )));
        }

        let pnpm_bin = ctx.paths.bin_dir().join("pnpm");
        if !pnpm_bin.exists() {
            return Ok(Status::needs("pnpm is not installed yet"));
        }
        let reported =
            ctx.runner
                .run(&pnpm_bin.to_string_lossy(), &["-v"], &RunOptions::default())?;
        if !version::same(reported.trimmed(), &pnpm_version) {
            return Ok(Status::needs(format!(
                "pnpm reports {} but the repo asks for {pnpm_version}",
                reported.trimmed()
            )));
        }

        Ok(Status::Satisfied)
    }

    fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let project = ctx.project_dir();
        let node_version = desired_node(project.as_deref());
        let pnpm_version = desired_pnpm(project.as_deref());

        install_node(ctx, &node_version)?;
        install_pnpm(ctx, &pnpm_version)?;

        ctx.config.node_version = Some(node_version);
        ctx.config.pnpm_version = Some(pnpm_version);
        ctx.config.save(ctx.paths.as_ref())?;
        Ok(())
    }
}

fn install_node(ctx: &mut Ctx, node_version: &str) -> Result<()> {
    let platform = download::node_platform()?;
    let filename = download::node_tarball_name(node_version, &platform);

    ctx.ui.note(&format!("Downloading Node {node_version}…"));
    let shasums = download::fetch_text(&download::node_shasums_url(node_version))?;
    let expected = download::digest_for(&shasums, &filename).ok_or_else(|| {
        Failure::new(
            format!("downloading Node {node_version}"),
            "Ask your team lead to check the Node version pinned in the repo's .nvmrc.",
        )
        .detail(format!("nodejs.org does not publish {filename}"))
    })?;

    let bytes = download::fetch_bytes(&download::node_tarball_url(node_version, &platform))?;
    let actual = download::sha256_hex(&bytes);
    if actual != expected {
        // Never unpack an archive that is not the one nodejs.org published.
        return Err(Failure::new(
            format!("verifying the Node {node_version} download"),
            "Run `riabuild` again on a trusted network. If it keeps failing, tell your team lead.",
        )
        .detail(format!("expected sha256 {expected}, got {actual}"))
        .into());
    }

    download::extract_node_tarball(&bytes, &ctx.paths.node_dir(node_version))?;
    Ok(())
}

fn install_pnpm(ctx: &mut Ctx, pnpm_version: &str) -> Result<()> {
    let asset = download::pnpm_asset(pnpm_version)?;
    ctx.ui.note(&format!("Downloading pnpm {pnpm_version}…"));
    // Unlike Node, pnpm publishes no checksums file, so there is no digest to
    // verify this against — HTTPS to github.com is the whole trust anchor.
    // Do not invent one; an unpublished digest checks nothing.
    let bytes = download::fetch_bytes(&download::pnpm_url(pnpm_version, &asset))?;

    let bin_dir = ctx.paths.bin_dir();
    std::fs::create_dir_all(&bin_dir)?;
    let target = bin_dir.join("pnpm");

    if download::pnpm_ships_a_tarball(pnpm_version) {
        // pnpm 11 is a launcher plus the `dist/` tree it loads from beside
        // itself, so it is installed as a tree and reached through a shim.
        let home = ctx.paths.pnpm_dir(pnpm_version);
        download::extract_pnpm_tarball(&bytes, &home)?;
        let launcher = home.join("pnpm");
        if !launcher.exists() {
            return Err(Failure::new(
                format!("installing pnpm {pnpm_version}"),
                "Ask your team lead to check the pnpm version pinned in the repo's package.json.",
            )
            .detail(format!(
                "{asset} unpacked without a `pnpm` launcher at its root"
            ))
            .into());
        }
        download::make_executable(&launcher)?;
        write_executable(&target, shims::pnpm_shim(&launcher).as_bytes())?;
    } else {
        write_executable(&target, &bytes)?;
    }
    Ok(())
}

/// Writes an executable via a staging file, so an interrupted run cannot leave
/// a half-written one behind that looks installed.
fn write_executable(target: &Path, bytes: &[u8]) -> Result<()> {
    let staging = target.with_extension("partial");
    std::fs::write(&staging, bytes)?;
    download::make_executable(&staging)?;
    std::fs::rename(&staging, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};

    #[test]
    fn reads_the_node_version_the_repo_pins() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(&dir.path().join(".nvmrc"), "v22.23.1\n");
        assert_eq!(desired_node(Some(dir.path())), "22.23.1");
    }

    #[test]
    fn falls_back_when_the_repo_pins_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(desired_node(Some(dir.path())), FALLBACK_NODE);
        assert_eq!(desired_pnpm(None), FALLBACK_PNPM);
    }

    #[test]
    fn reads_pnpm_out_of_package_manager() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(
            &dir.path().join("package.json"),
            r#"{"packageManager":"pnpm@10.20.0+sha512.abc"}"#,
        );
        assert_eq!(desired_pnpm(Some(dir.path())), "10.20.0");
    }

    /// Downloads the pinned pnpm and starts it through the shim riabuild
    /// writes.
    ///
    /// Ignored by default because it pulls ~50 MB from github.com; run it with
    /// `cargo test -- --ignored` whenever the pinned pnpm major moves. Nothing
    /// else catches a release-layout change: pnpm 11 renamed its macOS asset
    /// and stopped shipping a bare executable at all, and the first symptom was
    /// a 404 on a developer's first run.
    #[test]
    #[ignore = "downloads ~50 MB from github.com; pins pnpm's release layout"]
    fn the_pinned_pnpm_downloads_and_runs() {
        use crate::runner::{CommandRunner, RealRunner};

        let asset = download::pnpm_asset(FALLBACK_PNPM).unwrap();
        let bytes = download::fetch_bytes(&download::pnpm_url(FALLBACK_PNPM, &asset)).unwrap();

        let home = tempfile::TempDir::new().unwrap();
        let tree = home.path().join(FALLBACK_PNPM);
        download::extract_pnpm_tarball(&bytes, &tree).unwrap();
        let launcher = tree.join("pnpm");
        assert!(launcher.exists(), "{asset} has no launcher at its root");
        download::make_executable(&launcher).unwrap();

        let shim = home.path().join("pnpm");
        write_executable(&shim, shims::pnpm_shim(&launcher).as_bytes()).unwrap();

        let output = RealRunner
            .run(&shim.to_string_lossy(), &["-v"], &RunOptions::default())
            .expect("pnpm -v");
        assert!(output.ok(), "the shim could not start pnpm: {output:?}");
        assert_eq!(output.trimmed(), FALLBACK_PNPM);
    }

    #[test]
    fn a_missing_node_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new());
        assert!(matches!(Toolchain.check(&ctx).unwrap(), Status::Needs(_)));
    }

    #[test]
    fn a_node_of_the_wrong_version_is_detected() {
        // The case an existence check misses: the directory is there, the binary
        // runs, and it is the wrong Node.
        let (ctx, home) = ctx_with(FakeRunner::new());
        let node_bin = ctx.paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        write_file(&node_bin, "#!/bin/sh\n");
        let runner = FakeRunner::new().with(
            &format!("{} -v", node_bin.to_string_lossy()),
            0,
            "v20.11.0",
            "",
        );
        let (mut ctx, _home2) = ctx_with(runner);
        // Point the second context at the same tree.
        ctx.paths = std::sync::Arc::new(crate::paths::RealPaths::rooted_at(home.path()));
        let status = Toolchain.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("22.23.1"), "{status:?}");
    }

    #[test]
    fn a_complete_toolchain_is_satisfied() {
        let (ctx, home) = ctx_with(FakeRunner::new());
        let node_bin = ctx.paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        let pnpm_bin = ctx.paths.bin_dir().join("pnpm");
        write_file(&node_bin, "#!/bin/sh\n");
        write_file(&pnpm_bin, "#!/bin/sh\n");

        let runner = FakeRunner::new()
            .with(
                &format!("{} -v", node_bin.to_string_lossy()),
                0,
                &format!("v{FALLBACK_NODE}"),
                "",
            )
            // Reported through the constants, so bumping a fallback cannot
            // leave this test asserting a version nothing installs.
            .with(
                &format!("{} -v", pnpm_bin.to_string_lossy()),
                0,
                FALLBACK_PNPM,
                "",
            );
        let (mut ctx, _home2) = ctx_with(runner);
        ctx.paths = std::sync::Arc::new(crate::paths::RealPaths::rooted_at(home.path()));
        assert_eq!(Toolchain.check(&ctx).unwrap(), Status::Satisfied);
    }
}

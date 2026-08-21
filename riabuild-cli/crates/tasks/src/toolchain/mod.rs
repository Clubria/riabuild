//! Task 4 — riabuild-owned Node and pnpm.
//!
//! The versions come from the repo (`.nvmrc` and `packageManager`), which is why
//! this declares a dependency on `project` even though the design table shows
//! none: a check that reads files out of the checkout cannot run before the
//! checkout exists. An undeclared edge means running against stale state.

mod downloads;

use downloads::{Downloads, RealDownloads};

use super::{Ctx, Status, Task, TaskId};
use crate::shims;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_fetch::archive;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;
use std::path::Path;

/// Used when the repo pins nothing. Kept current with the repo's own `.nvmrc`.
const FALLBACK_NODE: &str = "22.23.1";
const FALLBACK_PNPM: &str = "11.11.0";

pub struct Toolchain;

/// Reads the Node version the repo asks for.
pub async fn desired_node(project: Option<&Path>) -> String {
    // A closure cannot be async, so the read has to leave the combinator chain.
    let Some(file) = project.map(|dir| dir.join(".nvmrc")) else {
        return FALLBACK_NODE.to_string();
    };
    let Ok(text) = tokio::fs::read_to_string(file).await else {
        return FALLBACK_NODE.to_string();
    };
    let text = text.trim().trim_start_matches('v').to_string();
    if text.is_empty() {
        return FALLBACK_NODE.to_string();
    }
    text
}

/// Reads the pnpm version out of `"packageManager": "pnpm@10.20.0"`.
pub async fn desired_pnpm(project: Option<&Path>) -> String {
    let Some(file) = project.map(|dir| dir.join("package.json")) else {
        return FALLBACK_PNPM.to_string();
    };
    let Ok(text) = tokio::fs::read_to_string(file).await else {
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

#[async_trait]
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

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let project = ctx.project_dir();
        let node_version = desired_node(project.as_deref()).await;
        let pnpm_version = desired_pnpm(project.as_deref()).await;

        let node_bin = ctx.paths.node_dir(&node_version).join("bin").join("node");
        match reported_version(ctx, &node_bin).await? {
            None => {
                return Ok(Status::needs(format!(
                    "Node {node_version} is not installed yet"
                )));
            }
            Some(found) if !version::same(&found, &node_version) => {
                return Ok(Status::needs(format!(
                    "the Node in ~/.riabuild reports {found} but the repo asks for {node_version}"
                )));
            }
            Some(_) => {}
        }

        let pnpm_bin = ctx.paths.bin_dir().join("pnpm");
        match reported_version(ctx, &pnpm_bin).await? {
            None => Ok(Status::needs("pnpm is not installed yet")),
            Some(found) if !version::same(&found, &pnpm_version) => Ok(Status::needs(format!(
                "pnpm reports {found} but the repo asks for {pnpm_version}"
            ))),
            Some(_) => Ok(Status::Satisfied),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        apply_with(ctx, &RealDownloads).await
    }
}

/// What the tool at `bin` answers `-v` with, `Ok(None)` when there is nothing
/// runnable there, and an error when riabuild could not find out.
///
/// The single definition of "is this the version the repo asks for": `check()`
/// turns the answer into a message and `apply()` turns it into a decision, and
/// the two must judge a tree identically or `apply()` re-runs forever, or —
/// worse on a shared server — re-installs a tree nothing was wrong with. The
/// binary is asked rather than the layout trusted: a directory that exists is
/// not evidence of a working install.
///
/// The three-way answer is the point. A `CommandRunner` error means riabuild
/// could not even *start* the binary — `EAGAIN` or `ENOMEM` under the process
/// and memory pressure a small shared box with several developers on it lives
/// in, or `ETXTBSY` — and that is no evidence at all about the tree. Folding it
/// into `None` made it read as "absent", and absent means replace: one
/// developer's transient spawn failure downloaded ~130 MB and swapped out the
/// Node a colleague's `pnpm dev` was executing from, then hard-errored anyway
/// when `check()` re-ran. A command that *did* run and answered badly — a
/// non-zero exit (`NODE_OPTIONS=--bogus node -v` exits 9 with empty stdout), or
/// output that is not a version — is real evidence about the tree, and still
/// means replace.
///
/// The probe deliberately names no directory, which is what stops pnpm
/// answering for one. `RunOptions::cwd` is `None`, so `RealRunner` runs it at
/// the filesystem root — see `FILESYSTEM_ROOT` in `runner/`, where the pnpm
/// version-handover this is guarding against is written up. Do **not** "fix"
/// this by pointing it at the checkout: the Clubria repo pins pnpm too, so a
/// probe run there would report the pin whatever binary riabuild had installed,
/// and `check()` would go green on a machine with the wrong pnpm on it.
async fn reported_version(ctx: &Ctx, bin: &Path) -> Result<Option<String>> {
    if !tokio::fs::try_exists(bin).await.unwrap_or(false) {
        return Ok(None);
    }
    let output = ctx
        .runner
        .run(&bin.to_string_lossy(), &["-v"], &RunOptions::default())
        .await?;
    Ok(output.ok().then(|| output.trimmed().to_string()))
}

async fn is_current(ctx: &Ctx, bin: &Path, wanted: &str) -> Result<bool> {
    Ok(matches!(reported_version(ctx, bin).await?, Some(found) if version::same(&found, wanted)))
}

async fn apply_with(ctx: &mut Ctx, downloads: &dyn Downloads) -> Result<()> {
    let project = ctx.project_dir();
    let node_version = desired_node(project.as_deref()).await;
    let pnpm_version = desired_pnpm(project.as_deref()).await;

    ensure_node(ctx, &node_version, downloads).await?;
    ensure_pnpm(ctx, &pnpm_version, downloads).await?;

    ctx.update_config(|config| {
        config.node_version = Some(node_version);
        config.pnpm_version = Some(pnpm_version);
    })
    .await?;
    Ok(())
}

/// Node, fetched only when the shared tree is not already the one the repo asks
/// for.
///
/// `paths::tools_root()` is shared by everyone with an account on a server,
/// while `bin_dir()` is one developer's alone — so `check()` can report drift
/// that has nothing to do with Node, and re-downloading it anyway would extract
/// over the tree a colleague's live session is running out of. That is why the
/// decision is made here, per tool, rather than by `apply()` doing both halves
/// unconditionally because it was asked to do anything at all.
async fn ensure_node(ctx: &Ctx, version: &str, downloads: &dyn Downloads) -> Result<()> {
    let tree = ctx.paths.node_dir(version);
    if is_current(ctx, &tree.join("bin").join("node"), version).await? {
        return Ok(());
    }
    ctx.ui.note(&format!("Downloading Node {version}…"));
    let bytes = downloads.node(version).await?;
    archive::extract_node_tarball(bytes, tree).await
}

/// pnpm, in the two halves it actually has.
///
/// The launcher and the `dist/` tree it loads from beside itself are shared
/// under `tools_root()`; the shim that starts them is this developer's own,
/// under `bin_dir()`. A co-tenant's first run on a server has exactly the
/// second missing and the first perfectly fine — writing the shim from the
/// launcher already there is the whole repair, and re-extracting 50 MB over a
/// tree a colleague is running out of is not.
///
/// One path for every pnpm, where there used to be two. Taking pnpm from npm
/// removed the branch: the GitHub releases published a bare executable up to
/// pnpm 10 and a tarball from 11 on, so the old code dropped a file straight
/// into `bin/` for one and unpacked a tree for the other. Every npm package is
/// a tarball, so both become a tree under the shared `tools_root()` reached
/// through a shim — which is also the layout the co-tenant repair above needs,
/// and pnpm 10 never had it.
async fn ensure_pnpm(ctx: &Ctx, version: &str, downloads: &dyn Downloads) -> Result<()> {
    let bin_dir = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin_dir).await?;

    let tree = ctx.paths.pnpm_dir(version);
    let launcher = tree.join("pnpm");
    if !is_current(ctx, &launcher, version).await? {
        ctx.ui.note(&format!("Downloading pnpm {version}…"));
        let parts = downloads.pnpm(version).await?;
        archive::extract_npm_tarballs(parts, tree).await?;
        if !tokio::fs::try_exists(&launcher).await.unwrap_or(false) {
            return Err(Failure::new(
                format!("installing pnpm {version}"),
                "Ask your team lead to check the pnpm version pinned in the repo's package.json.",
            )
            .detail(format!(
                "the npm packages for pnpm {version} unpacked without a `pnpm` launcher at their \
                 root"
            ))
            .into());
        }
        archive::make_executable(&launcher).await?;
    }
    write_executable(
        &bin_dir.join("pnpm"),
        shims::exec_shim(&launcher).as_bytes(),
    )
    .await
}

/// Writes an executable via a staging file, so an interrupted run cannot leave
/// a half-written one behind that looks installed.
async fn write_executable(target: &Path, bytes: &[u8]) -> Result<()> {
    let staging = target.with_extension("partial");
    tokio::fs::write(&staging, bytes).await?;
    archive::make_executable(&staging).await?;
    tokio::fs::rename(&staging, target).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::downloads::{published_integrity, verified};
    use super::*;
    use riabuild_fetch::download;
    use riabuild_paths::Paths;
    use riabuild_runner::FakeRunner;

    use crate::testing::{ctx_with, write_file};

    /// Trimmed from what `registry.npmjs.org/@pnpm/linux-x64/11.11.0` actually
    /// answered on 2026-08-21, keeping the field this reads.
    const VERSION_DOCUMENT: &str = r#"{
      "name": "@pnpm/linux-x64",
      "version": "11.11.0",
      "os": ["linux"],
      "cpu": ["x64"],
      "dist": {
        "shasum": "4263a680b5f6e9183d34aaa283e676900949d04d",
        "tarball": "https://registry.npmjs.org/@pnpm/linux-x64/-/linux-x64-11.11.0.tgz",
        "integrity": "sha512-rwMbNJR+PstRu+ymWoApei1CWrAnsnW3tm+3H8qOxbp8duiaj6u7DxlMzhKbVpFwylxcJdeGwZ5tReBFOVpsdw==",
        "attestations": {
          "provenance": { "predicateType": "https://slsa.dev/provenance/v1" }
        }
      }
    }"#;

    #[test]
    fn reads_the_integrity_npm_published_for_the_version() {
        let integrity =
            published_integrity(VERSION_DOCUMENT, "@pnpm/linux-x64", "11.11.0").unwrap();
        assert!(integrity.starts_with("sha512-"), "{integrity}");
        assert_eq!(
            download::npm_integrity_digest(&integrity).map(|digest| digest.len()),
            Some(64),
            "the digest riabuild will compare against has to be readable here"
        );
    }

    /// The state the GitHub-metadata path replaced was "download it anyway",
    /// and this must not walk back to it by another route. A document with no
    /// `dist.integrity` is an error, because verifying nothing and calling it
    /// verified is the failure `../../../../CLAUDE.md` forbids.
    #[test]
    fn a_version_that_records_no_integrity_is_refused_rather_than_downloaded() {
        for document in [
            r#"{"dist":{"shasum":"4263a680b5f6e9183d34aaa283e676900949d04d"}}"#,
            r#"{"dist":{"integrity":null}}"#,
            r#"{"name":"@pnpm/linux-x64","version":"9.15.9"}"#,
        ] {
            let error = published_integrity(document, "@pnpm/linux-x64", "9.15.9")
                .expect_err("nothing to verify against");
            assert!(
                format!("{error}").contains("@pnpm/linux-x64@9.15.9"),
                "{error}"
            );
        }
    }

    /// A digest in an algorithm riabuild does not compute must not be compared
    /// against a sha512 and reported as a mismatch: that reads as tampering and
    /// is a format change.
    #[test]
    fn an_integrity_that_is_not_a_sha512_is_refused_as_a_format_rather_than_a_mismatch() {
        let other =
            r#"{"dist":{"integrity":"sha256-uu0Uc6dncf/8j5wcrJqCFYTfXlIH3IsgO5r9wRnaOZ0="}}"#;
        let error = published_integrity(other, "@pnpm/linux-x64", "12.0.0")
            .expect_err("riabuild cannot compute that");
        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(failure.detail.contains("not a sha512"), "{failure:?}");
    }

    #[test]
    fn an_answer_that_is_not_a_version_document_is_refused() {
        // What the registry answers for a version it does not have is the
        // string `"version not found: 11.11.0"`, which is valid JSON and has
        // no `dist` in it.
        assert!(published_integrity("not json at all", "@pnpm/macos-x64", "11.11.0").is_err());
        assert!(
            published_integrity(
                r#""version not found: 11.11.0""#,
                "@pnpm/macos-x64",
                "11.11.0"
            )
            .is_err()
        );
    }

    #[test]
    fn bytes_that_match_the_published_integrity_are_handed_on() {
        let tarball = b"a tarball, for the purposes of a hash".to_vec();
        let published = download::npm_integrity(&tarball);
        assert_eq!(
            verified("@pnpm/linux-x64", "11.11.0", &published, tarball.clone()).unwrap(),
            tarball
        );
    }

    /// The whole point of the item: an unverified download is never installed.
    ///
    /// This is the last gate before `extract_npm_tarballs`, and it is a
    /// function rather than a branch inside the fetch so that it can be
    /// asserted with no network at all.
    #[test]
    fn bytes_that_do_not_match_are_refused_before_anything_is_unpacked() {
        let published = download::npm_integrity(b"what npm published");
        let error = verified(
            "@pnpm/linux-x64",
            "11.11.0",
            &published,
            b"what a proxy handed back".to_vec(),
        )
        .expect_err("a mismatch is never installed");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("something the developer can act on");
        assert!(
            failure.attempting.contains("nothing was installed"),
            "{failure}"
        );
        // Both sides are shown, in the spelling the registry page uses.
        assert!(failure.detail.contains(&published), "{failure:?}");
        assert!(
            failure
                .detail
                .contains(&download::npm_integrity(b"what a proxy handed back")),
            "{failure:?}"
        );
    }

    /// One npm tarball in memory: everything under the `package/` wrapper npm
    /// puts around every published package.
    fn npm_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            // 0644, the mode npm actually stores the launcher with — which is
            // why `ensure_pnpm` has to make it executable itself.
            header.set_mode(0o644);
            builder
                .append_data(&mut header, format!("package/{path}"), *contents)
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// A pnpm whose parts are already verified, and a Node that must never be
    /// asked for.
    struct FixedPnpm(Vec<Vec<u8>>);

    #[async_trait]
    impl Downloads for FixedPnpm {
        async fn node(&self, _version: &str) -> Result<Vec<u8>> {
            panic!("must not download Node on this path");
        }
        async fn pnpm(&self, _version: &str) -> Result<Vec<Vec<u8>>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn the_npm_packages_land_the_launcher_where_check_looks_for_it() {
        // The layout `tools::install` and `check()` both assume: `package/` is
        // gone, the launcher is at the root of the shared tree with `dist/`
        // beside it, it is executable — npm stores it 0644 — and this
        // developer's own `bin/pnpm` starts it.
        let server = tempfile::TempDir::new().unwrap();
        let paths = riabuild_paths::RealPaths::rooted_at(server.path());
        let (mut ctx, _laptop) = ctx_with(FakeRunner::new()).await;
        ctx.paths = std::sync::Arc::new(paths);

        let parts = vec![
            npm_tarball(&[
                ("pnpm", b"This file intentionally left blank" as &[u8]),
                ("dist/pnpm.mjs", b"the bundle"),
            ]),
            npm_tarball(&[("pnpm", b"#!/bin/sh\n" as &[u8])]),
        ];
        ensure_pnpm(&ctx, FALLBACK_PNPM, &FixedPnpm(parts))
            .await
            .expect("installs pnpm");

        let launcher = ctx.paths.pnpm_dir(FALLBACK_PNPM).join("pnpm");
        assert_eq!(
            tokio::fs::read_to_string(&launcher).await.unwrap(),
            "#!/bin/sh\n",
            "the platform launcher has to land on top of the bundle's placeholder"
        );
        assert!(
            ctx.paths
                .pnpm_dir(FALLBACK_PNPM)
                .join("dist/pnpm.mjs")
                .exists(),
            "the launcher loads dist/ from beside itself"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(&launcher)
                .await
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "npm stores it 0644; mode was {mode:o}");
        }
        let shim = tokio::fs::read_to_string(ctx.paths.bin_dir().join("pnpm"))
            .await
            .expect("the developer's own bin/pnpm is what check() runs");
        assert!(
            shim.contains(&launcher.to_string_lossy().into_owned()),
            "{shim}"
        );
    }

    #[tokio::test]
    async fn a_pnpm_10_install_is_the_same_tree_and_the_same_shim() {
        // pnpm 10 and older are one self-contained npm package rather than
        // two, and that is the only difference. The old GitHub path dropped a
        // bare executable straight into `bin/`, which is why a co-tenant on a
        // server could never inherit one.
        let server = tempfile::TempDir::new().unwrap();
        let paths = riabuild_paths::RealPaths::rooted_at(server.path());
        let (mut ctx, _laptop) = ctx_with(FakeRunner::new()).await;
        ctx.paths = std::sync::Arc::new(paths);

        let parts = vec![npm_tarball(&[("pnpm", b"#!/bin/sh\n" as &[u8])])];
        ensure_pnpm(&ctx, "10.20.0", &FixedPnpm(parts))
            .await
            .expect("installs pnpm");

        let launcher = ctx.paths.pnpm_dir("10.20.0").join("pnpm");
        assert_eq!(
            tokio::fs::read_to_string(&launcher).await.unwrap(),
            "#!/bin/sh\n"
        );
        assert!(ctx.paths.bin_dir().join("pnpm").exists());
    }

    #[tokio::test]
    async fn packages_that_unpack_without_a_launcher_are_a_failure_rather_than_an_install() {
        // The failure mode an upstream layout change produces: everything
        // downloads, everything verifies, and there is no `pnpm` at the root.
        // Reported here rather than left for `check()`, which would say "pnpm
        // is not installed yet" about a machine riabuild had just written to.
        let server = tempfile::TempDir::new().unwrap();
        let paths = riabuild_paths::RealPaths::rooted_at(server.path());
        let (mut ctx, _laptop) = ctx_with(FakeRunner::new()).await;
        ctx.paths = std::sync::Arc::new(paths);

        let parts = vec![npm_tarball(&[("dist/pnpm.mjs", b"the bundle" as &[u8])])];
        let error = ensure_pnpm(&ctx, FALLBACK_PNPM, &FixedPnpm(parts))
            .await
            .expect_err("half of pnpm is not pnpm");
        assert!(format!("{error}").contains("installing pnpm"), "{error}");
        assert!(!ctx.paths.bin_dir().join("pnpm").exists());
    }

    #[tokio::test]
    async fn reads_the_node_version_the_repo_pins() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(&dir.path().join(".nvmrc"), "v22.23.1\n").await;
        assert_eq!(desired_node(Some(dir.path())).await, "22.23.1");
    }

    #[tokio::test]
    async fn falls_back_when_the_repo_pins_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(desired_node(Some(dir.path())).await, FALLBACK_NODE);
        assert_eq!(desired_pnpm(None).await, FALLBACK_PNPM);
    }

    #[tokio::test]
    async fn reads_pnpm_out_of_package_manager() {
        let dir = tempfile::TempDir::new().unwrap();
        write_file(
            &dir.path().join("package.json"),
            r#"{"packageManager":"pnpm@10.20.0+sha512.abc"}"#,
        )
        .await;
        assert_eq!(desired_pnpm(Some(dir.path())).await, "10.20.0");
    }

    /// Downloads the pinned pnpm through the real registry, verifies it, and
    /// starts it through the shim riabuild writes.
    ///
    /// Ignored by default because it pulls ~50 MB from registry.npmjs.org; run
    /// it with `cargo test -- --ignored` whenever the pinned pnpm major moves.
    /// Nothing else catches an upstream layout change, and pnpm has made two:
    /// it renamed its macOS release asset and stopped shipping a bare
    /// executable at 11, and it splits `dist/` out of the platform package from
    /// 11 on. The first symptom of each was a laptop, not a test.
    ///
    /// It goes through `RealDownloads` rather than fetching by hand, so the
    /// integrity check is part of what is being exercised: a registry that
    /// stopped publishing `dist.integrity` has to fail here rather than install.
    #[tokio::test]
    #[ignore = "downloads ~50 MB from registry.npmjs.org; pins pnpm's published layout"]
    async fn the_pinned_pnpm_downloads_and_runs() {
        use riabuild_runner::{CommandRunner, RealRunner};

        let parts = RealDownloads.pnpm(FALLBACK_PNPM).await.expect("verified");
        assert_eq!(
            parts.len(),
            2,
            "pnpm 11 is the bundle plus the platform launcher"
        );

        let home = tempfile::TempDir::new().unwrap();
        let tree = home.path().join(FALLBACK_PNPM);
        archive::extract_npm_tarballs(parts, tree.clone())
            .await
            .unwrap();
        let launcher = tree.join("pnpm");
        assert!(
            tokio::fs::try_exists(&launcher).await.unwrap_or(false),
            "the npm packages for pnpm {FALLBACK_PNPM} have no launcher at their root"
        );
        archive::make_executable(&launcher).await.unwrap();

        let shim = home.path().join("pnpm");
        write_executable(&shim, shims::exec_shim(&launcher).as_bytes())
            .await
            .unwrap();

        let output = RealRunner
            .run(&shim.to_string_lossy(), &["-v"], &RunOptions::default())
            .await
            .expect("pnpm -v");
        assert!(output.ok(), "the shim could not start pnpm: {output:?}");
        assert_eq!(output.trimmed(), FALLBACK_PNPM);
    }

    #[tokio::test]
    async fn a_missing_node_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            Toolchain.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn a_node_of_the_wrong_version_is_detected() {
        // The case an existence check misses: the directory is there, the binary
        // runs, and it is the wrong Node.
        let (ctx, home) = ctx_with(FakeRunner::new()).await;
        let node_bin = ctx.paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        write_file(&node_bin, "#!/bin/sh\n").await;
        let runner = FakeRunner::new().with(
            &format!("{} -v", node_bin.to_string_lossy()),
            0,
            "v20.11.0",
            "",
        );
        let (mut ctx, _home2) = ctx_with(runner).await;
        // Point the second context at the same tree.
        ctx.paths = std::sync::Arc::new(riabuild_paths::RealPaths::rooted_at(home.path()));
        let status = Toolchain.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("22.23.1"), "{status:?}");
    }

    /// A `Downloads` that panics if called — for proving a path fetches
    /// nothing at all, rather than merely happening not to under this stub.
    /// The same device `remote::install`'s tests use.
    struct UnreachableDownloads;

    #[async_trait]
    impl Downloads for UnreachableDownloads {
        async fn node(&self, _version: &str) -> Result<Vec<u8>> {
            panic!("must not download Node on this path");
        }
        async fn pnpm(&self, _version: &str) -> Result<Vec<Vec<u8>>> {
            panic!("must not download pnpm on this path");
        }
    }

    /// One fixed Node archive, and a pnpm that must never be asked for.
    struct FixedNode(Vec<u8>);

    #[async_trait]
    impl Downloads for FixedNode {
        async fn node(&self, _version: &str) -> Result<Vec<u8>> {
            Ok(self.0.clone())
        }
        async fn pnpm(&self, _version: &str) -> Result<Vec<Vec<u8>>> {
            panic!("must not download pnpm on this path");
        }
    }

    /// A `node-v*.tar.gz` in memory: one wrapper directory, as nodejs.org
    /// publishes, so `extract_node_tarball`'s strip is exercised too.
    fn node_archive(contents: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        builder
            .append_data(
                &mut header,
                format!("node-v{FALLBACK_NODE}-darwin-arm64/bin/node"),
                contents,
            )
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// A server's `Paths`: state under this developer's own namespace, tools
    /// under the `~/.riabuild` every account on the box shares — the shape
    /// Task 6 introduced and the shape this whole hazard lives in.
    fn co_tenant_paths(server: &std::path::Path, member: &str) -> riabuild_paths::RealPaths {
        riabuild_paths::RealPaths::with_root(
            server,
            riabuild_paths::remote_namespace(server, member),
        )
    }

    #[tokio::test]
    async fn a_co_tenants_missing_shim_is_repaired_without_touching_the_shared_trees() {
        // Ada is logged into a server with a live session running `pnpm dev`.
        // Bob runs `riabuild remote <that same server>`: the shared Node and
        // pnpm are exactly where Ada left them, and the only thing missing is
        // the `pnpm` shim in Bob's own namespace, which never existed. An
        // `apply()` that reinstalls both tools because *something* was missing
        // extracts over the trees Ada's session is running out of — and that
        // is deterministic, not a race.
        //
        // `UnreachableDownloads` makes "fetches nothing" a property of the
        // path rather than a coincidence of this stub.
        let server = tempfile::TempDir::new().unwrap();
        let paths = co_tenant_paths(server.path(), "bob-member-id");
        let node_bin = paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        let launcher = paths.pnpm_dir(FALLBACK_PNPM).join("pnpm");
        write_file(&node_bin, "ada's node\n").await;
        write_file(&launcher, "ada's pnpm\n").await;

        let runner = FakeRunner::new()
            .with(
                &format!("{} -v", node_bin.to_string_lossy()),
                0,
                &format!("v{FALLBACK_NODE}"),
                "",
            )
            .with(
                &format!("{} -v", launcher.to_string_lossy()),
                0,
                FALLBACK_PNPM,
                "",
            );
        let (mut ctx, _laptop) = ctx_with(runner).await;
        ctx.paths = std::sync::Arc::new(paths);

        apply_with(&mut ctx, &UnreachableDownloads)
            .await
            .expect("writes the shim from what is already there");

        assert!(
            tokio::fs::try_exists(ctx.paths.bin_dir().join("pnpm"))
                .await
                .unwrap_or(false),
            "the missing shim is what apply() was for"
        );
        // Byte for byte: a re-extract would have replaced both of these, and
        // Ada's session would have died mid-command.
        assert_eq!(
            tokio::fs::read_to_string(&node_bin).await.unwrap(),
            "ada's node\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(&launcher).await.unwrap(),
            "ada's pnpm\n"
        );
    }

    #[tokio::test]
    async fn a_shared_tree_that_is_already_the_pinned_version_is_left_alone() {
        // Narrower than the co-tenant case above and stated on its own,
        // because it is the property the whole shared `tools_root()` rests on:
        // a Node that already answers for the version the repo pins is not
        // re-fetched and not re-extracted, whatever else `apply()` was called
        // to fix.
        let server = tempfile::TempDir::new().unwrap();
        let paths = co_tenant_paths(server.path(), "bob-member-id");
        let node_bin = paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        write_file(&node_bin, "ada's node\n").await;

        let runner = FakeRunner::new().with(
            &format!("{} -v", node_bin.to_string_lossy()),
            0,
            &format!("v{FALLBACK_NODE}"),
            "",
        );
        let (mut ctx, _laptop) = ctx_with(runner).await;
        ctx.paths = std::sync::Arc::new(paths);

        ensure_node(&ctx, FALLBACK_NODE, &UnreachableDownloads)
            .await
            .expect("nothing to do");

        assert_eq!(
            tokio::fs::read_to_string(&node_bin).await.unwrap(),
            "ada's node\n"
        );
    }

    #[tokio::test]
    async fn a_node_that_reports_the_wrong_version_is_still_replaced() {
        // The other side of the same fix, and the reason the skip asks the
        // binary rather than statting the directory: drift `check()` reports
        // has to be something `apply()` can actually repair. Skipping on mere
        // existence would trade a destructive bug for a machine that can never
        // be fixed, since `check()` runs again straight afterwards and its
        // failure is a hard error.
        let server = tempfile::TempDir::new().unwrap();
        let paths = riabuild_paths::RealPaths::rooted_at(server.path());
        let node_bin = paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        write_file(&node_bin, "the wrong node\n").await;

        let runner = FakeRunner::new().with(
            &format!("{} -v", node_bin.to_string_lossy()),
            0,
            "v20.11.0",
            "",
        );
        let (mut ctx, _laptop) = ctx_with(runner).await;
        ctx.paths = std::sync::Arc::new(paths);

        ensure_node(
            &ctx,
            FALLBACK_NODE,
            &FixedNode(node_archive(b"the right node\n")),
        )
        .await
        .expect("replaces the wrong tree");

        assert_eq!(
            tokio::fs::read_to_string(&node_bin).await.unwrap(),
            "the right node\n"
        );
    }

    /// A `CommandRunner` that cannot start anything — `EAGAIN` under a process
    /// limit, `ENOMEM`, `ETXTBSY`. `FakeRunner` cannot express this: every
    /// stub, and every unstubbed call, returns `Ok` with an exit code.
    struct CannotSpawn;

    #[async_trait]
    impl riabuild_runner::CommandRunner for CannotSpawn {
        async fn run(
            &self,
            program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<riabuild_runner::CommandOutput> {
            Err(anyhow::anyhow!(
                "could not start `{program}`: Resource temporarily unavailable (os error 11)"
            ))
        }
        // The same refusal as `run`: what this double models is a machine that
        // cannot spawn *anything*, so answering one entry point and not the
        // others would make the double disagree with its own premise.
        async fn run_bytes(
            &self,
            program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<riabuild_runner::BytesOutput> {
            Err(anyhow::anyhow!(
                "could not start `{program}`: Resource temporarily unavailable (os error 11)"
            ))
        }
        async fn run_forking(
            &self,
            program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<i32> {
            Err(anyhow::anyhow!(
                "could not start `{program}`: Resource temporarily unavailable (os error 11)"
            ))
        }
        async fn spawn(
            &self,
            program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<Box<dyn riabuild_runner::ChildHandle>> {
            Err(anyhow::anyhow!(
                "could not start `{program}`: Resource temporarily unavailable (os error 11)"
            ))
        }
        async fn run_interactive(
            &self,
            _program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<i32> {
            unreachable!("this task never runs anything interactively")
        }
        fn which(&self, _program: &str) -> Option<std::path::PathBuf> {
            None
        }
    }

    #[tokio::test]
    async fn a_node_that_will_not_start_is_not_evidence_that_it_is_missing() {
        // A small shared box under process or memory pressure fails `spawn`
        // with EAGAIN/ENOMEM while every tree on it is perfectly fine. Read as
        // "not installed", that fetched ~130 MB and swapped out the Node a
        // colleague's live session was executing from — and then hard-errored
        // anyway when `check()` re-ran. `UnreachableDownloads` makes
        // "downloads nothing" a property of the path, and the byte-for-byte
        // assertion makes "touches nothing" one too.
        let server = tempfile::TempDir::new().unwrap();
        let paths = co_tenant_paths(server.path(), "bob-member-id");
        let node_bin = paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        write_file(&node_bin, "ada's node\n").await;

        let (mut ctx, _laptop) = ctx_with(FakeRunner::new()).await;
        ctx.paths = std::sync::Arc::new(paths);
        ctx.runner = std::sync::Arc::new(CannotSpawn);

        let error = ensure_node(&ctx, FALLBACK_NODE, &UnreachableDownloads)
            .await
            .expect_err("riabuild could not start the binary, which says nothing about the tree");
        assert!(
            error.to_string().contains("could not start"),
            "the spawn failure is what should surface: {error}"
        );
        assert_eq!(
            tokio::fs::read_to_string(&node_bin).await.unwrap(),
            "ada's node\n"
        );
    }

    #[tokio::test]
    async fn a_check_that_cannot_start_node_is_an_error_not_a_missing_node() {
        // The same distinction on the reporting side: `check()` reporting
        // "Node is not installed yet" is what sends `apply()` at the shared
        // tree in the first place, so it must not say that about a machine it
        // failed to ask.
        let (ctx, home) = ctx_with(FakeRunner::new()).await;
        let node_bin = ctx.paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        write_file(&node_bin, "#!/bin/sh\n").await;

        let (mut ctx, _home2) = ctx_with(FakeRunner::new()).await;
        ctx.paths = std::sync::Arc::new(riabuild_paths::RealPaths::rooted_at(home.path()));
        ctx.runner = std::sync::Arc::new(CannotSpawn);

        Toolchain
            .check(&ctx)
            .await
            .expect_err("a machine riabuild could not ask is not a machine without Node");
    }

    #[tokio::test]
    async fn a_node_that_runs_and_exits_non_zero_is_still_replaced() {
        // The behaviour the three-way answer must not lose: a binary that
        // *did* start and answered badly — `NODE_OPTIONS=--bogus node -v`
        // exits 9 with empty stdout — is real evidence about the tree, and
        // still means reinstall.
        let server = tempfile::TempDir::new().unwrap();
        let paths = riabuild_paths::RealPaths::rooted_at(server.path());
        let node_bin = paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        write_file(&node_bin, "a node that will not run\n").await;

        let runner = FakeRunner::new().with(
            &format!("{} -v", node_bin.to_string_lossy()),
            9,
            "",
            "node: --bogus is not allowed in NODE_OPTIONS",
        );
        let (mut ctx, _laptop) = ctx_with(runner).await;
        ctx.paths = std::sync::Arc::new(paths);

        ensure_node(
            &ctx,
            FALLBACK_NODE,
            &FixedNode(node_archive(b"the right node\n")),
        )
        .await
        .expect("replaces a tree that cannot answer for itself");

        assert_eq!(
            tokio::fs::read_to_string(&node_bin).await.unwrap(),
            "the right node\n"
        );
    }

    /// A machine riabuild has already provisioned correctly, whose `pnpm`
    /// answers for the directory it is standing in rather than for itself.
    ///
    /// Not a contrivance — this is what pnpm 11 does. `switchCliVersion` reads
    /// the nearest `package.json` at or above pnpm's working directory and,
    /// when a `packageManager` field names another pnpm, downloads that version
    /// and re-execs the command through it. `pnpm -v` therefore reports the
    /// pin, not the binary.
    ///
    /// `FakeRunner` cannot express this: its stubs are keyed on the invocation,
    /// and the invocation is identical whichever directory the probe runs in.
    /// That is exactly why the bug survived a green suite.
    ///
    /// Where the probe lands is resolved through
    /// `runner::directory_for_riabuild`, not guessed here, so this cannot go on
    /// agreeing with a rule the real runner has stopped applying.
    struct ProvisionedMachine {
        node: String,
        /// The pnpm actually installed under `~/.riabuild`.
        pnpm: String,
    }

    impl ProvisionedMachine {
        /// pnpm's own lookup: the nearest `packageManager` at or above `dir`.
        async fn pinned_at_or_above(dir: &Path) -> Option<String> {
            for dir in dir.ancestors() {
                let Ok(text) = tokio::fs::read_to_string(dir.join("package.json")).await else {
                    continue;
                };
                let pinned = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|json| {
                        Some(
                            json.get("packageManager")?
                                .as_str()?
                                .strip_prefix("pnpm@")?
                                .to_string(),
                        )
                    });
                if pinned.is_some() {
                    return pinned;
                }
            }
            None
        }
    }

    #[async_trait]
    impl riabuild_runner::CommandRunner for ProvisionedMachine {
        async fn run(
            &self,
            program: &str,
            args: &[&str],
            options: &RunOptions,
        ) -> Result<riabuild_runner::CommandOutput> {
            assert_eq!(args, ["-v"], "this task only ever asks for a version");
            let stdout = if program.ends_with("node") {
                // Node reads no manifest and is the control in this test: it
                // answers the same wherever it is started.
                format!("v{}", self.node)
            } else {
                let dir = riabuild_runner::directory_for_riabuild(options.cwd.as_deref());
                Self::pinned_at_or_above(dir)
                    .await
                    .unwrap_or_else(|| self.pnpm.clone())
            };
            Ok(riabuild_runner::CommandOutput {
                code: Some(0),
                stdout,
                stderr: String::new(),
            })
        }
        async fn run_bytes(
            &self,
            _program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<riabuild_runner::BytesOutput> {
            unreachable!("this task only ever asks a binary for its version");
        }
        async fn run_forking(
            &self,
            _program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<i32> {
            unreachable!("this task only ever asks a binary for its version");
        }
        async fn spawn(
            &self,
            _program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<Box<dyn riabuild_runner::ChildHandle>> {
            unreachable!("this task only ever asks a binary for its version");
        }
        async fn run_interactive(
            &self,
            _program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<i32> {
            unreachable!("this task never runs anything interactively");
        }
        fn which(&self, _program: &str) -> Option<std::path::PathBuf> {
            None
        }
    }

    #[tokio::test]
    async fn the_version_probe_is_not_pointed_at_a_repo_that_pins_pnpm() {
        // What `check()` compares is the binary against the repo's pin, and the
        // two are only one directory apart: run the probe anywhere a
        // `package.json` can answer for it and pnpm reports the pin instead, so
        // the comparison becomes one string read twice.
        //
        // Standing in the developer's *own* project was the incident — riabuild
        // reported drift `apply()` could not repair, and every retry failed
        // identically. That half is gone at the runner: a command riabuild runs
        // for itself no longer inherits anywhere. This pins the tempting repair,
        // which is worse because it is silent — pointing the probe at the
        // checkout "so pnpm can see the repo" would make `check()` go green on a
        // machine with the wrong pnpm installed.
        let checkout = tempfile::TempDir::new().unwrap();
        write_file(
            &checkout.path().join("package.json"),
            r#"{"name":"ai-builders-hub","packageManager":"pnpm@10.20.0"}"#,
        )
        .await;

        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.project_path = Some(checkout.path().to_string_lossy().into_owned());
        write_file(
            &ctx.paths.node_dir(FALLBACK_NODE).join("bin").join("node"),
            "#!/bin/sh\n",
        )
        .await;
        write_file(&ctx.paths.bin_dir().join("pnpm"), "#!/bin/sh\n").await;
        ctx.runner = std::sync::Arc::new(ProvisionedMachine {
            node: FALLBACK_NODE.to_string(),
            pnpm: FALLBACK_PNPM.to_string(),
        });

        let status = Toolchain.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains(&format!("pnpm reports {FALLBACK_PNPM}")),
            "the probe answered for the checkout instead of for the binary: {status:?}"
        );
    }

    #[tokio::test]
    async fn a_complete_toolchain_is_satisfied() {
        let (ctx, home) = ctx_with(FakeRunner::new()).await;
        let node_bin = ctx.paths.node_dir(FALLBACK_NODE).join("bin").join("node");
        let pnpm_bin = ctx.paths.bin_dir().join("pnpm");
        write_file(&node_bin, "#!/bin/sh\n").await;
        write_file(&pnpm_bin, "#!/bin/sh\n").await;

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
        let (mut ctx, _home2) = ctx_with(runner).await;
        ctx.paths = std::sync::Arc::new(riabuild_paths::RealPaths::rooted_at(home.path()));
        assert_eq!(Toolchain.check(&ctx).await.unwrap(), Status::Satisfied);
    }
}

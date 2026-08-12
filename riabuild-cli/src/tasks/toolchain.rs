//! Task 4 — riabuild-owned Node and pnpm.
//!
//! The versions come from the repo (`.nvmrc` and `packageManager`), which is why
//! this declares a dependency on `project` even though the design table shows
//! none: a check that reads files out of the checkout cannot run before the
//! checkout exists. An undeclared edge means running against stale state.

use super::{Ctx, Status, Task, TaskId};
use crate::archive;
use crate::download;
use crate::runner::RunOptions;
use crate::shims;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use async_trait::async_trait;
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

/// The two archives this task fetches, behind a trait so that what `apply()`
/// decides *not* to download is testable without pulling 50 MB over the
/// network — the same seam `remote::install` puts in front of the riabuild
/// release. Each returns bytes already checked against whatever the publisher
/// publishes, so nothing below this line has to remember to verify.
#[async_trait]
trait Downloads: Send + Sync {
    async fn node(&self, version: &str) -> Result<Vec<u8>>;
    async fn pnpm(&self, version: &str, asset: &str) -> Result<Vec<u8>>;
}

struct RealDownloads;

#[async_trait]
impl Downloads for RealDownloads {
    async fn node(&self, version: &str) -> Result<Vec<u8>> {
        let platform = download::node_platform()?;
        let filename = download::node_tarball_name(version, &platform);
        let shasums = download::fetch_text(&download::node_shasums_url(version)).await?;
        let expected = download::digest_for(&shasums, &filename).ok_or_else(|| {
            Failure::new(
                format!("downloading Node {version}"),
                "Ask your team lead to check the Node version pinned in the repo's .nvmrc.",
            )
            .detail(format!("nodejs.org does not publish {filename}"))
        })?;

        let bytes = download::fetch_bytes(&download::node_tarball_url(version, &platform)).await?;
        let actual = download::sha256_hex(&bytes);
        if actual != expected {
            // Never unpack an archive that is not the one nodejs.org published.
            return Err(Failure::new(
                format!("verifying the Node {version} download"),
                "Run `riabuild` again on a trusted network. If it keeps failing, tell your team lead.",
            )
            .detail(format!("expected sha256 {expected}, got {actual}"))
            .into());
        }
        Ok(bytes)
    }

    /// Unlike Node, pnpm publishes no checksums file, so there is no digest to
    /// verify this against — HTTPS to github.com is the whole trust anchor.
    /// Do not invent one; an unpublished digest checks nothing.
    async fn pnpm(&self, version: &str, asset: &str) -> Result<Vec<u8>> {
        download::fetch_bytes(&download::pnpm_url(version, asset)).await
    }
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
    archive::extract_node_tarball(&bytes, &tree)
}

/// pnpm, in the two halves it actually has.
///
/// The launcher and the `dist/` tree it loads from beside itself are shared
/// under `tools_root()`; the shim that starts them is this developer's own,
/// under `bin_dir()`. A co-tenant's first run on a server has exactly the
/// second missing and the first perfectly fine — writing the shim from the
/// launcher already there is the whole repair, and re-extracting 50 MB over a
/// tree a colleague is running out of is not.
async fn ensure_pnpm(ctx: &Ctx, version: &str, downloads: &dyn Downloads) -> Result<()> {
    let bin_dir = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin_dir).await?;
    let target = bin_dir.join("pnpm");
    let asset = download::pnpm_asset(version)?;

    if !download::pnpm_ships_a_tarball(version) {
        // pnpm 10 and older are a single executable, and it goes straight into
        // this developer's own bin/ — there is no shared tree to be careful
        // with, and rewriting a file nobody else can see costs nothing.
        ctx.ui.note(&format!("Downloading pnpm {version}…"));
        let bytes = downloads.pnpm(version, &asset).await?;
        return write_executable(&target, &bytes).await;
    }

    // pnpm 11 is a launcher plus the `dist/` tree it loads from beside itself,
    // so it is installed as a tree and reached through a shim.
    let tree = ctx.paths.pnpm_dir(version);
    let launcher = tree.join("pnpm");
    if !is_current(ctx, &launcher, version).await? {
        ctx.ui.note(&format!("Downloading pnpm {version}…"));
        let bytes = downloads.pnpm(version, &asset).await?;
        archive::extract_pnpm_tarball(&bytes, &tree)?;
        if !tokio::fs::try_exists(&launcher).await.unwrap_or(false) {
            return Err(Failure::new(
                format!("installing pnpm {version}"),
                "Ask your team lead to check the pnpm version pinned in the repo's package.json.",
            )
            .detail(format!(
                "{asset} unpacked without a `pnpm` launcher at its root"
            ))
            .into());
        }
        archive::make_executable(&launcher).await?;
    }
    write_executable(&target, shims::exec_shim(&launcher).as_bytes()).await
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
    use super::*;
    use crate::paths::Paths;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};

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

    /// Downloads the pinned pnpm and starts it through the shim riabuild
    /// writes.
    ///
    /// Ignored by default because it pulls ~50 MB from github.com; run it with
    /// `cargo test -- --ignored` whenever the pinned pnpm major moves. Nothing
    /// else catches a release-layout change: pnpm 11 renamed its macOS asset
    /// and stopped shipping a bare executable at all, and the first symptom was
    /// a 404 on a developer's first run.
    #[tokio::test]
    #[ignore = "downloads ~50 MB from github.com; pins pnpm's release layout"]
    async fn the_pinned_pnpm_downloads_and_runs() {
        use crate::runner::{CommandRunner, RealRunner};

        let asset = download::pnpm_asset(FALLBACK_PNPM).unwrap();
        let bytes = download::fetch_bytes(&download::pnpm_url(FALLBACK_PNPM, &asset))
            .await
            .unwrap();

        let home = tempfile::TempDir::new().unwrap();
        let tree = home.path().join(FALLBACK_PNPM);
        archive::extract_pnpm_tarball(&bytes, &tree).unwrap();
        let launcher = tree.join("pnpm");
        assert!(
            tokio::fs::try_exists(&launcher).await.unwrap_or(false),
            "{asset} has no launcher at its root"
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
        ctx.paths = std::sync::Arc::new(crate::paths::RealPaths::rooted_at(home.path()));
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
        async fn pnpm(&self, _version: &str, _asset: &str) -> Result<Vec<u8>> {
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
        async fn pnpm(&self, _version: &str, _asset: &str) -> Result<Vec<u8>> {
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
    fn co_tenant_paths(server: &std::path::Path, member: &str) -> crate::paths::RealPaths {
        crate::paths::RealPaths::with_root(server, crate::paths::remote_namespace(server, member))
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
        let paths = crate::paths::RealPaths::rooted_at(server.path());
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
    impl crate::runner::CommandRunner for CannotSpawn {
        async fn run(
            &self,
            program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<crate::runner::CommandOutput> {
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
        ) -> Result<crate::runner::BytesOutput> {
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
        ) -> Result<Box<dyn crate::runner::ChildHandle>> {
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
        ctx.paths = std::sync::Arc::new(crate::paths::RealPaths::rooted_at(home.path()));
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
        let paths = crate::paths::RealPaths::rooted_at(server.path());
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
        ctx.paths = std::sync::Arc::new(crate::paths::RealPaths::rooted_at(home.path()));
        assert_eq!(Toolchain.check(&ctx).await.unwrap(), Status::Satisfied);
    }
}

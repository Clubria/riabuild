//! Wrangler, Cloudflare's command-line interface for Workers.
//!
//! An npm package, installed into riabuild's own Node prefix the way the
//! TypeScript language server and the Codex CLI are, so `wrangler` is on the
//! `PATH` of the environment shell without the developer having had a Node, an
//! npm, or a global prefix of their own. `shell::environment` puts
//! `~/.riabuild/node/<version>/bin` in front, which is the whole of how this
//! ends up being the `wrangler` they get.
//!
//! [`crate::cf_cli`] installs the other half of this: `cf`, the technical
//! preview of what Wrangler is being rebuilt into. Two tasks rather than one on
//! purpose — that module says why.
//!
//! riabuild does **not** sign anyone in. A `wrangler login` is a developer's
//! own Cloudflare account — nothing riabuild brokers, and nothing the
//! onboarding path is blocked on — and it lands in their own
//! `~/.config/.wrangler`, which is also where the version probe below writes a
//! log line. That directory is deliberately not redirected under
//! `~/.riabuild/`: unlike Codex's nine profiles there is no per-account
//! separation to arrange here, and moving a developer's Cloudflare credentials
//! somewhere riabuild owns would make riabuild responsible for a secret it has
//! no business holding.

use super::{Ctx, Resource, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;
use std::path::Path;

const PACKAGE: &str = "wrangler";

/// The exact version riabuild installs.
///
/// A constant for the reason `codex_cli::PACKAGE_VERSION` is one: what riabuild
/// puts on a laptop is versioned and auditable, rather than whatever npm called
/// `latest` the morning a developer happened to run this. Naming the version is
/// also what makes npm's integrity check a statement about the software instead
/// of about the download — npm resolves an exact version to one packument entry
/// and verifies the tarball against that entry's `dist.integrity`, a sha512 the
/// registry publishes and neither riabuild nor riabuild-web supplies. `wrangler`
/// carries an npm provenance attestation beside it.
///
/// Its `engines` floor is Node 22, which `toolchain::FALLBACK_NODE` clears. A
/// repository whose `.nvmrc` pins something older is the one case this pin
/// cannot serve, and it is a Wrangler fact rather than a riabuild one.
///
/// Bumping this is a code change, and `version()` goes up beside it so every
/// existing install converges.
const PACKAGE_VERSION: &str = "4.134.0";

pub struct Wrangler;

#[async_trait]
impl Task for Wrangler {
    fn id(&self) -> TaskId {
        "wrangler"
    }

    fn title(&self) -> &str {
        "Wrangler"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // Installed with the Node riabuild owns, into that Node's prefix — so a
        // toolchain that moved has to re-run this rather than leave `wrangler`
        // behind in a prefix nothing puts on the `PATH` any more.
        &["toolchain"]
    }

    // An npm global install reads and rewrites the same Node prefix as the
    // Claude, Codex and TypeScript installs, all of which sit in the same wave.
    // The resource serialises them without inventing a dependency between tools
    // that have nothing to do with each other.
    fn writes(&self) -> &[Resource] {
        &["node_prefix"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(node_version) = &ctx.config.node_version else {
            return Ok(Status::needs("Node is not installed yet"));
        };
        let bin = ctx.paths.node_dir(node_version).join("bin");
        let wrangler = bin.join(PACKAGE);
        // Existence before invocation, for the reason `codex_cli::probe` gives:
        // `RealRunner::run` returns `Err` when the program is not there — a
        // spawn failure, not an exit code — so asking `--version` first would
        // make the missing-binary case an `anyhow` chain instead of a reason a
        // developer can read.
        if !tokio::fs::try_exists(&wrangler).await.unwrap_or(false) {
            return Ok(Status::needs("Wrangler is not installed"));
        }
        let output = ctx
            .runner
            .run(
                &wrangler.to_string_lossy(),
                &["--version"],
                &probe_options(&bin),
            )
            .await?;
        if !output.ok() {
            return Ok(Status::needs("Wrangler is installed but will not run"));
        }
        // Equality, not a floor. A machine running some other Wrangler — newer
        // included — is a machine whose behaviour nobody in the org has
        // reproduced, and reinstalling converges it.
        if !version::same(output.trimmed(), PACKAGE_VERSION) {
            return Ok(Status::needs(format!(
                "Wrangler reports `{}`, and riabuild installs {PACKAGE_VERSION}",
                output.trimmed()
            )));
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let Some(node_version) = ctx.config.node_version.clone() else {
            return Err(Failure::new(
                "installing Wrangler",
                "Run `riabuild` again — the Node install has to finish first.",
            )
            .into());
        };
        let node_dir = ctx.paths.node_dir(&node_version);
        let bin = node_dir.join("bin");
        let npm = bin.join("npm");
        if !tokio::fs::try_exists(&npm).await.unwrap_or(false) {
            return Err(Failure::new(
                "installing Wrangler",
                "Run `riabuild` again — the Node install has to finish first.",
            )
            .detail(format!("{} does not exist", npm.display()))
            .into());
        }

        ctx.ui.note("Installing Wrangler…");
        // `--prefix` names the tree `check()` reads, and names it on the command
        // line so a `prefix` line in the developer's own `~/.npmrc` cannot
        // redirect the install — which would leave `check()` reporting Wrangler
        // as missing on a machine that has just installed it, and installing it
        // again on every run, for ever.
        let prefix = node_dir.to_string_lossy().into_owned();
        let spec = format!("{PACKAGE}@{PACKAGE_VERSION}");
        // `--ignore-scripts` because nothing in this tree needs one: `wrangler`
        // itself declares no lifecycle script, and its two binary-bearing
        // dependencies — `workerd` and `esbuild` — resolve their per-platform
        // executables through `optionalDependencies` that npm installs and
        // verifies like any other package, rather than through a `postinstall`
        // that downloads one. Checked against 4.134.0, where `wrangler
        // --version`, `workerd --version` and `esbuild --version` all answer
        // from a tree installed with the flag. So it costs nothing here and
        // closes the gap that makes an npm install different from every other
        // tool riabuild owns: a lifecycle script is arbitrary code from a
        // package riabuild never verified, running before anything has looked
        // at what was installed.
        let output = ctx
            .runner
            .run(
                &npm.to_string_lossy(),
                &[
                    "install",
                    "-g",
                    "--ignore-scripts",
                    "--prefix",
                    &prefix,
                    &spec,
                ],
                &install_options(&bin),
            )
            .await?;
        if !output.ok() {
            let by_hand = format!("npm install -g {spec}");
            return Err(Failure::new(
                "installing Wrangler",
                format!("Install it yourself with `{by_hand}`, then run `riabuild` again."),
            )
            .command(by_hand)
            .detail(output.stderr)
            .into());
        }
        Ok(())
    }
}

/// The environment a `wrangler --version` probe runs in.
///
/// npm installs `bin/wrangler` as a symlink to a script whose shebang asks
/// `PATH` for a Node, so riabuild's own Node has to be the one that answers.
/// Without this the probe exits 127 on a **managed server**, where riabuild runs
/// under a non-interactive SSH exec whose `PATH` is
/// `/usr/local/bin:/usr/bin:/bin` and carries no Node at all — `check()` would
/// read that as "not installed" on a machine where `apply()` had just installed
/// it perfectly well, which is the hard error a developer cannot get past by
/// running riabuild again. A laptop hides it: the developer's own nvm or
/// Homebrew Node answers, and the probe passes for a reason riabuild did not
/// arrange and cannot rely on.
fn probe_options(node_bin: &Path) -> RunOptions {
    RunOptions {
        env: vec![path_led_by(node_bin)],
        ..Default::default()
    }
}

/// How long `npm install` may take — the same bound, for the same reason, as
/// `codex_cli`'s: a package download over a link riabuild does not choose, which
/// `RunOptions`' ten-minute ceiling was never a statement about. Wrangler pulls
/// workerd, which is the largest of these by some way.
const INSTALL_PATIENCE: std::time::Duration = std::time::Duration::from_secs(1800);

fn install_options(node_bin: &Path) -> RunOptions {
    RunOptions {
        timeout: Some(INSTALL_PATIENCE),
        ..probe_options(node_bin)
    }
}

fn path_led_by(dir: &Path) -> (String, String) {
    let ambient = std::env::var("PATH").unwrap_or_default();
    ("PATH".to_string(), format!("{}:{ambient}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;

    const NODE: &str = "22.23.1";

    async fn node_ctx(runner: FakeRunner) -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = ctx_with(runner).await;
        ctx.update_config(|config| config.node_version = Some(NODE.into()))
            .await
            .expect("config");
        (ctx, home)
    }

    async fn installed_ctx(runner: FakeRunner) -> (Ctx, tempfile::TempDir) {
        let (ctx, home) = node_ctx(runner).await;
        write_file(
            &ctx.paths.node_dir(NODE).join("bin").join("wrangler"),
            "#!/bin/sh\n",
        )
        .await;
        (ctx, home)
    }

    #[tokio::test]
    async fn the_pinned_version_is_satisfied() {
        let runner = FakeRunner::new().with("wrangler --version", 0, PACKAGE_VERSION, "");
        let (ctx, _home) = installed_ctx(runner).await;
        assert_eq!(Wrangler.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn another_version_is_drift_even_when_it_is_newer() {
        let runner = FakeRunner::new().with("wrangler --version", 0, "4.200.0", "");
        let (ctx, _home) = installed_ctx(runner).await;
        let status = Wrangler.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("4.200.0"),
            "expected the reported version in the reason, got {status:?}"
        );
    }

    #[tokio::test]
    async fn an_installed_wrangler_that_will_not_run_is_drift() {
        let runner = FakeRunner::new().with("wrangler --version", 1, "", "boom");
        let (ctx, _home) = installed_ctx(runner).await;
        let status = Wrangler.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("will not run"),
            "got {status:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_wrangler_is_detected_without_running_it() {
        let (ctx, _home) = node_ctx(FakeRunner::new()).await;
        let status = Wrangler.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "got {status:?}"
        );
    }

    #[tokio::test]
    async fn no_node_yet_is_reported_rather_than_probed() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = Wrangler.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("Node is not installed"),
            "got {status:?}"
        );
    }

    #[test]
    fn the_package_is_exactly_pinned() {
        assert_eq!(format!("{PACKAGE}@{PACKAGE_VERSION}"), "wrangler@4.134.0");
    }
}

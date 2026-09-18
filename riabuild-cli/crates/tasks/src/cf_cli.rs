//! `cf`, Cloudflare's CLI for the whole platform.
//!
//! Cloudflare is rebuilding Wrangler as one command for every product rather
//! than for Workers alone, and `cf` is that rebuild's technical preview —
//! announced at <https://blog.cloudflare.com/cf-cli-local-explorer/>, which
//! also says where it is going: "this Technical Preview is just a small piece
//! of the future Wrangler CLI". So this task and [`crate::wrangler`] are two
//! tasks for what will eventually be one tool, and they are deliberately not
//! folded into one now.
//!
//! **Two tasks rather than one, and the reason is failure rather than tidiness.**
//! One task installing both packages would be one `npm install`, one wave slot
//! and less code — and a preview at `0.x` that fails to install, or ships a
//! version whose `--version` banner this cannot read, would take Wrangler down
//! with it, because a task records nothing when its `apply()` fails.
//! `engine::run_all` carries on past a failed task and skips only its
//! dependents, so two tasks is what buys a developer a working `wrangler` on a
//! morning when the preview is broken. When `cf` absorbs Wrangler, these
//! collapse into one; until then the split is the whole point.
//!
//! riabuild does not sign anyone in: `cf auth` is the developer's own
//! Cloudflare account, for the reasons [`crate::wrangler`] gives.

use super::{Ctx, Resource, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;
use std::path::Path;

const PACKAGE: &str = "cf";

/// The exact version riabuild installs.
///
/// Pinned for the reason every other package riabuild installs is pinned — see
/// the constant of the same name in [`crate::wrangler`] — and more so here, not
/// less: `cf` is a technical preview at `0.x`, where a minor bump is allowed to
/// change anything, and "whatever npm called `latest` this morning" would mean
/// two developers onboarding a week apart get two different CLIs.
///
/// npm resolves the exact version to one packument entry and verifies the
/// tarball against that entry's `dist.integrity` — a sha512 the registry
/// publishes, which neither riabuild nor riabuild-web supplies. Say the one
/// difference from `wrangler` out loud rather than let it be discovered:
/// `cf@0.10.0` carries **no npm provenance attestation**, so the registry's
/// integrity record is the whole of what is verified here. That is the same
/// guarantee `pnpm` is fetched under — see `fetch::download::assets` — and it
/// is a fact about what Cloudflare publishes today rather than a bar riabuild
/// lowered.
///
/// Bumping this is a code change, and `version()` goes up beside it so every
/// existing install converges.
const PACKAGE_VERSION: &str = "0.10.0";

pub struct CfCli;

#[async_trait]
impl Task for CfCli {
    fn id(&self) -> TaskId {
        "cf_cli"
    }

    fn title(&self) -> &str {
        "Cloudflare CLI"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &["toolchain"]
    }

    // The same Node prefix as the Claude, Codex, TypeScript and Wrangler
    // installs, all of which sit in the same wave.
    fn writes(&self) -> &[Resource] {
        &["node_prefix"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(node_version) = &ctx.config.node_version else {
            return Ok(Status::needs("Node is not installed yet"));
        };
        let bin = ctx.paths.node_dir(node_version).join("bin");
        let cf = bin.join(PACKAGE);
        // Existence before invocation — `RealRunner::run` returns `Err` when the
        // program is not there, so asking `--version` first would make the
        // missing-binary case an `anyhow` chain instead of a reason to act on.
        if !tokio::fs::try_exists(&cf).await.unwrap_or(false) {
            return Ok(Status::needs("the Cloudflare CLI is not installed"));
        }
        let output = ctx
            .runner
            .run(&cf.to_string_lossy(), &["--version"], &probe_options(&bin))
            .await?;
        if !output.ok() {
            return Ok(Status::needs(
                "the Cloudflare CLI is installed but will not run",
            ));
        }
        let Some(reported) = reported_version(output.trimmed()) else {
            // A preview is allowed to change how it announces itself, and the
            // honest answer to "I cannot read this" is drift rather than
            // silence: `apply()` reinstalls the pin, after which this reads
            // again or the machine gets a reason a person can look at.
            return Ok(Status::needs(
                "the Cloudflare CLI does not report a version this riabuild can read",
            ));
        };
        // Equality, not a floor: at `0.x` a newer preview is not a better one,
        // it is one nobody in the org has reproduced anything against.
        if !version::same(&reported, PACKAGE_VERSION) {
            return Ok(Status::needs(format!(
                "the Cloudflare CLI reports `{reported}`, and riabuild installs {PACKAGE_VERSION}"
            )));
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let Some(node_version) = ctx.config.node_version.clone() else {
            return Err(Failure::new(
                "installing the Cloudflare CLI",
                "Run `riabuild` again — the Node install has to finish first.",
            )
            .into());
        };
        let node_dir = ctx.paths.node_dir(&node_version);
        let bin = node_dir.join("bin");
        let npm = bin.join("npm");
        if !tokio::fs::try_exists(&npm).await.unwrap_or(false) {
            return Err(Failure::new(
                "installing the Cloudflare CLI",
                "Run `riabuild` again — the Node install has to finish first.",
            )
            .detail(format!("{} does not exist", npm.display()))
            .into());
        }

        ctx.ui.note("Installing the Cloudflare CLI…");
        // `--prefix` on the command line so a `prefix` line in the developer's
        // own `~/.npmrc` cannot redirect the install out from under `check()`.
        // `--ignore-scripts` because `cf@0.10.0` declares no lifecycle script
        // and its binary-bearing dependency — miniflare's `workerd` — arrives
        // as a package npm resolves and verifies rather than as a `postinstall`
        // that downloads one. See `wrangler::apply` for why the flag is the
        // default here rather than an option.
        let prefix = node_dir.to_string_lossy().into_owned();
        let spec = format!("{PACKAGE}@{PACKAGE_VERSION}");
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
                "installing the Cloudflare CLI",
                format!("Install it yourself with `{by_hand}`, then run `riabuild` again."),
            )
            .command(by_hand)
            .detail(output.stderr)
            .into());
        }
        Ok(())
    }
}

/// The version `cf --version` is announcing, or `None` when its output holds no
/// version at all.
///
/// `cf` answers with a banner rather than a string — `🍊☁️  cf · v0.10.0` over a
/// rule — and it colours it whether or not anything is watching, so on a stock
/// run the bytes carry SGR escapes whose parameters are themselves digits.
/// `NO_COLOR` in [`probe_options`] takes those out, and this is the second
/// belt: `version::parse` wants the first run of at least two dot-separated
/// numbers, every escape parameter is a single number followed by `;` or `m`,
/// and so the first thing that qualifies is the version. A test pins that
/// against the real coloured bytes, because `NO_COLOR` is a courtesy a preview
/// is free to stop honouring.
fn reported_version(output: &str) -> Option<String> {
    let parsed = version::parse(output)?;
    Some(
        parsed
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

/// The environment a `cf --version` probe runs in.
///
/// `PATH` for the reason `wrangler::probe_options` gives: npm installs `bin/cf`
/// as a script whose shebang asks `PATH` for a Node, and on a managed server
/// under a non-interactive SSH exec there is none — which would read as "not
/// installed" on a machine where `apply()` had just installed it.
///
/// `NO_COLOR` because the version is read out of a banner. See
/// [`reported_version`].
fn probe_options(node_bin: &Path) -> RunOptions {
    RunOptions {
        env: vec![path_led_by(node_bin), ("NO_COLOR".into(), "1".into())],
        ..Default::default()
    }
}

/// The same bound, for the same reason, as `wrangler`'s: a package download
/// over a link riabuild does not choose.
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

    /// What `cf --version` prints with `NO_COLOR` set, byte for byte.
    const PLAIN_BANNER: &str = "🍊☁️  cf · v0.10.0\n──────────────────";

    /// And without it — the SGR escapes are real, and their parameters are
    /// digits, which is the thing `reported_version` has to survive.
    const COLOURED_BANNER: &str = "🍊☁️  \u{1b}[1m\u{1b}[38;2;246;130;31mcf\u{1b}[39m\u{1b}[22m \u{1b}[2m·\u{1b}[22m \u{1b}[2mv0.10.0\u{1b}[22m\n\u{1b}[38;2;246;130;31m──────────────────\u{1b}[39m";

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
            &ctx.paths.node_dir(NODE).join("bin").join("cf"),
            "#!/bin/sh\n",
        )
        .await;
        (ctx, home)
    }

    #[tokio::test]
    async fn the_pinned_version_is_satisfied() {
        let runner = FakeRunner::new().with("cf --version", 0, PLAIN_BANNER, "");
        let (ctx, _home) = installed_ctx(runner).await;
        assert_eq!(CfCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_coloured_banner_is_read_the_same_way() {
        let runner = FakeRunner::new().with("cf --version", 0, COLOURED_BANNER, "");
        let (ctx, _home) = installed_ctx(runner).await;
        assert_eq!(CfCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[test]
    fn the_escape_parameters_are_not_mistaken_for_a_version() {
        assert_eq!(reported_version(COLOURED_BANNER).as_deref(), Some("0.10.0"));
        assert_eq!(reported_version(PLAIN_BANNER).as_deref(), Some("0.10.0"));
    }

    #[tokio::test]
    async fn another_preview_is_drift_even_when_it_is_newer() {
        let runner = FakeRunner::new().with("cf --version", 0, "🍊☁️  cf · v0.11.0", "");
        let (ctx, _home) = installed_ctx(runner).await;
        let status = CfCli.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("0.11.0"), "got {status:?}");
    }

    #[tokio::test]
    async fn an_unreadable_banner_is_drift_rather_than_satisfied() {
        let runner = FakeRunner::new().with("cf --version", 0, "cf (preview)", "");
        let (ctx, _home) = installed_ctx(runner).await;
        let status = CfCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("does not report a version"),
            "got {status:?}"
        );
    }

    #[tokio::test]
    async fn an_installed_cf_that_will_not_run_is_drift() {
        let runner = FakeRunner::new().with("cf --version", 1, "", "boom");
        let (ctx, _home) = installed_ctx(runner).await;
        let status = CfCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("will not run"),
            "got {status:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_cf_is_detected_without_running_it() {
        let (ctx, _home) = node_ctx(FakeRunner::new()).await;
        let status = CfCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "got {status:?}"
        );
    }

    #[tokio::test]
    async fn no_node_yet_is_reported_rather_than_probed() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = CfCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("Node is not installed"),
            "got {status:?}"
        );
    }

    #[test]
    fn the_package_is_exactly_pinned() {
        assert_eq!(format!("{PACKAGE}@{PACKAGE_VERSION}"), "cf@0.10.0");
    }
}

//! Installing Claude Code with the Node riabuild owns.
//!
//! `@anthropic-ai/claude-code` is an npm package, so the whole of this is
//! about making `npm -g` mean riabuild's own tree rather than whichever Node
//! the developer's `PATH` happens to lead to.

use crate::Ctx;
use anyhow::Result;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use std::path::Path;

/// The environment `npm` has to run in for `-g` to mean riabuild's Node.
///
/// `bin/npm` in the Node tarball is a symlink to a script whose shebang is
/// `#!/usr/bin/env node`, so the machine's own Node interprets it whenever one is
/// on `PATH` — and npm derives its global prefix from `process.execPath`, which
/// would then be a Node riabuild does not own. Putting riabuild's Node first both
/// fixes that and removes the need for any system Node at all, which is the whole
/// point of owning one.
///
/// Prepended rather than replacing `PATH`: npm shells out to `git` and `sh`, and
/// a provisioner that broke `npm install` to fix a prefix would have traded one
/// failure for a stranger one.
pub(super) fn npm_env(node_bin: &Path) -> Vec<(String, String)> {
    let ambient = std::env::var("PATH").unwrap_or_default();
    vec![(
        "PATH".to_string(),
        format!("{}:{ambient}", node_bin.display()),
    )]
}

/// How long `npm install` may take.
///
/// The same reasoning as the clone in `project.rs`, one step down: this is tens
/// of megabytes off the npm registry over a link riabuild does not choose, and
/// `RunOptions`' ten-minute ceiling is a bound on a call that has *hung* rather
/// than a statement about how long a package download honestly takes. Held to
/// it, a developer on a slow or throttled connection is told riabuild timed out
/// installing Claude Code, which is the one thing the run exists to deliver.
///
/// Half an hour rather than `None`: npm genuinely can wedge on a registry that
/// accepts the connection and then stops answering, and unlike the clone this
/// is a download of a known, bounded size.
const INSTALL_PATIENCE: std::time::Duration = std::time::Duration::from_secs(1800);

/// The options `npm install` runs under — named so
/// `installing_claude_code_is_given_its_own_patience` can pin the bound.
fn install_options(node_bin: &Path) -> RunOptions {
    RunOptions {
        env: npm_env(node_bin),
        timeout: Some(INSTALL_PATIENCE),
        ..Default::default()
    }
}

pub(super) async fn install_claude(ctx: &mut Ctx) -> Result<()> {
    let node_version = match ctx.config.node_version.clone() {
        Some(pinned) => pinned,
        // Not `unwrap_or_else`: the fallback awaits, and a closure cannot.
        None => crate::toolchain::desired_node(ctx.project_dir().as_deref()).await,
    };
    let node_dir = ctx.paths.node_dir(&node_version);
    let node_bin = node_dir.join("bin");
    let npm = node_bin.join("npm");

    if !tokio::fs::try_exists(&npm).await.unwrap_or(false) {
        return Err(Failure::new(
            "installing Claude Code",
            "Run `riabuild` again — the Node install has to finish first.",
        )
        .detail(format!("{} does not exist", npm.display()))
        .into());
    }

    ctx.ui.note("Installing Claude Code…");
    // `--prefix` names the tree `Ctx::claude()` reads, and names it on the
    // command line so a `prefix` line in the developer's own `~/.npmrc` cannot
    // redirect the install. Without it, `check()` reports Claude Code as missing
    // on a machine that has just installed it — and keeps installing it, every
    // run, forever.
    let prefix = node_dir.to_string_lossy().into_owned();
    let output = ctx
        .runner
        .run(
            &npm.to_string_lossy(),
            &[
                "install",
                "-g",
                "--prefix",
                &prefix,
                "@anthropic-ai/claude-code",
            ],
            &install_options(&node_bin),
        )
        .await?;
    if !output.ok() {
        return Err(Failure::new(
            "installing Claude Code",
            "Install it yourself with `npm install -g @anthropic-ai/claude-code`, then run `riabuild` again.",
        )
        .command("npm install -g @anthropic-ai/claude-code")
        .detail(output.stderr)
        .into());
    }
    Ok(())
}

//! Installing the Codex CLI with the Node riabuild owns.
//!
//! `@openai/codex` is an npm package, so the whole of this is about making
//! `npm -g` mean riabuild's own tree rather than whichever Node the
//! developer's `PATH` happens to lead to.

use super::{package_spec, path_led_by};
use crate::Ctx;
use anyhow::Result;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use std::path::Path;

pub(super) async fn install_codex(ctx: &mut Ctx) -> Result<()> {
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
            "installing the Codex CLI",
            "Run `riabuild` again — the Node install has to finish first.",
        )
        .detail(format!("{} does not exist", npm.display()))
        .into());
    }

    ctx.ui.note("Installing the Codex CLI…");
    // `--prefix` names the tree `Ctx::codex()` reads, and names it on the
    // command line so a `prefix` line in the developer's own `~/.npmrc` cannot
    // redirect the install. Without it, `check()` reports Codex as missing on a
    // machine that has just installed it — and keeps installing it, every run,
    // forever.
    let prefix = node_dir.to_string_lossy().into_owned();
    let spec = package_spec();
    // `--ignore-scripts` because `@openai/codex` declares none — checked
    // against 0.149.0, whose `package.json` has no `scripts` block at all — and
    // its per-platform binaries arrive as `optionalDependencies` npm resolves
    // rather than as a `postinstall` that downloads one. So the flag costs
    // nothing here and closes the gap that makes an npm install different from
    // every other tool riabuild owns: a lifecycle script is arbitrary code from
    // a package riabuild never verified, running before anything has looked at
    // what was installed. If a future Codex needs a script to work, that is a
    // decision to take deliberately rather than one to inherit.
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
            &install_options(&node_bin),
        )
        .await?;
    if !output.ok() {
        let by_hand = format!("npm install -g {spec}");
        return Err(Failure::new(
            "installing the Codex CLI",
            format!("Install it yourself with `{by_hand}`, then run `riabuild` again."),
        )
        .command(by_hand)
        .detail(output.stderr)
        .into());
    }
    Ok(())
}

/// The environment `npm` has to run in for `-g` to mean riabuild's Node.
///
/// The same reasoning as `claude_accounts::npm_env`, and deliberately its own
/// copy rather than a shared helper: these are two lines each task owns, and a
/// shared one would have to be reached through a module that exists only to
/// hold it.
///
/// `bin/npm` in the Node tarball is a symlink to a script whose shebang is
/// `#!/usr/bin/env node`, so the machine's own Node interprets it whenever one
/// is on `PATH` — and npm derives its global prefix from `process.execPath`,
/// which would then be a Node riabuild does not own.
pub(super) fn npm_env(node_bin: &Path) -> Vec<(String, String)> {
    vec![path_led_by(node_bin)]
}

/// How long `npm install` may take — the same bound, for the same reason, as
/// `claude_accounts::INSTALL_PATIENCE`: a package download over a link riabuild
/// does not choose, which `RunOptions`' ten-minute ceiling was never a
/// statement about. A copy rather than a shared constant for the reason
/// `npm_env` above is a copy — the two files agree by coincidence of size, not
/// by one deciding for the other.
const INSTALL_PATIENCE: std::time::Duration = std::time::Duration::from_secs(1800);

/// The options `npm install` runs under — named so
/// `installing_codex_is_given_its_own_patience` can pin the bound.
fn install_options(node_bin: &Path) -> RunOptions {
    RunOptions {
        env: npm_env(node_bin),
        timeout: Some(INSTALL_PATIENCE),
        ..Default::default()
    }
}

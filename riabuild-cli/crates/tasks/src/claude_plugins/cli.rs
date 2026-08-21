//! The three `claude` invocations this task makes, and how their answers are
//! read.

use crate::Ctx;
use anyhow::Result;
use riabuild_runner::{CommandOutput, RunOptions};
use riabuild_ui::Failure;
use serde_json::Value;

/// Runs one `claude` against one account's config directory.
///
/// No `cwd`, so the answer depends on argv and `CLAUDE_CONFIG_DIR` and nothing
/// else. Claude Code reads `.claude/` out of its working directory, which is
/// the class of bug `../../CLAUDE.md` describes for pnpm and infisical: pointed
/// at the checkout, `plugin list` would answer partly for the *repository*
/// rather than for the account, and a `check()` built on that reports drift the
/// `apply()` after it cannot repair.
pub(super) async fn ask(ctx: &Ctx, id: &str, args: &[&str]) -> Result<CommandOutput> {
    let dir = ctx.paths.claude_profile_dir(id);
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };
    ctx.runner.run(&ctx.claude(), args, &options).await
}

/// The `key` of every entry in a `--json` listing, or nothing if the listing is
/// not one riabuild understands.
pub(super) fn names(json: &str, key: &str) -> Vec<String> {
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| entry.get(key)?.as_str().map(str::to_string))
        .collect()
}

/// One installation step, or a failure that names what to run by hand.
///
/// Deliberately without `--yes`. That flag exists for a plugin a marketplace
/// installs by *running a command it declares*, and riabuild accepting that on
/// a developer's behalf, unattended, is the boundary the architecture rules
/// draw — riabuild may install what the checkout names, never run what a
/// marketplace hands it. Such a plugin fails here instead, with the command to
/// review and run.
pub(super) async fn install(ctx: &Ctx, id: &str, args: &[&str]) -> Result<()> {
    let ran = ask(ctx, id, args).await?;
    if ran.ok() {
        return Ok(());
    }
    let spelled = format!("claude {}", args.join(" "));
    Err(Failure::new(
        "installing the plugins the checkout asks for",
        format!("Run `{spelled}` yourself to see what it says — it is safe to re-run, and so is riabuild."),
    )
    .command(spelled)
    .detail(if ran.stderr.trim().is_empty() {
        format!("that command exited with status {:?}", ran.code)
    } else {
        ran.stderr.trim().to_string()
    })
    .into())
}

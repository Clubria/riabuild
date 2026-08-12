//! The per-account `.claude.json` — Claude Code's own first-run state.
//!
//! Not a task. Two tasks write here: `claude_trust` records the trusted
//! checkout, `claude_onboarding` records that the first-run setup is done.
//! Neither fact is expressible in a settings file, which is the only reason
//! riabuild writes into this file at all.
//!
//! Both edits share the same three hazards, so the read-modify-write lives here
//! once rather than twice: the file is live state Claude Code may be rewriting
//! this instant, every key riabuild does not own has to survive, and a
//! half-written config is one Claude Code cannot start against. Hence
//! read-modify-write, never a template, and a staged file renamed into place so
//! the new content lands whole or not at all.

use super::Ctx;
use anyhow::Result;
use riabuild_paths::contract_tilde;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Where one account keeps its Claude Code config.
pub(crate) fn config_file(ctx: &Ctx, profile: &str) -> PathBuf {
    ctx.paths.claude_config_file(profile)
}

/// What `check()` found at one account's config.
///
/// The three cases are kept apart because they are three different things to
/// tell a developer, and because only one of them means the machine is right.
pub(crate) enum Stored {
    /// Claude Code has never run for this account and riabuild has not written
    /// to it either.
    Missing,
    /// Present but not a JSON object. Claude Code cannot start against this, so
    /// the machine is broken whatever else the file says.
    Unreadable,
    Present(Map<String, Value>),
}

/// Reads one account's config for a `check()`.
pub(crate) async fn read(ctx: &Ctx, id: &str) -> Stored {
    let Ok(text) = tokio::fs::read_to_string(config_file(ctx, id)).await else {
        return Stored::Missing;
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Stored::Present(map),
        _ => Stored::Unreadable,
    }
}

/// Applies one change to an account's config, preserving every key it does not
/// touch.
///
/// Per-account rather than per-run so that `riabuild claude new` can reach it:
/// an account created between two `riabuild` runs would otherwise meet the
/// dialogs these edits exist to prevent on its first launch, which is precisely
/// when the developer is about to use it.
pub(crate) async fn edit<F>(ctx: &mut Ctx, id: &str, change: F) -> Result<()>
where
    F: FnOnce(&mut Map<String, Value>),
{
    let file = config_file(ctx, id);
    if let Some(parent) = file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut root = load_or_reset(ctx, &file).await?;
    change(&mut root);

    let text = serde_json::to_string_pretty(&Value::Object(root))?;
    let staged = file.with_extension("json.riabuild-tmp");
    tokio::fs::write(&staged, text).await?;
    tokio::fs::rename(&staged, &file).await?;
    Ok(())
}

/// The existing config, or a fresh one if there is nothing usable there.
///
/// A config that does not parse is moved aside rather than merged into or
/// silently overwritten: it is the developer's session history and MCP servers,
/// and a copy on disk is what makes the loss recoverable.
async fn load_or_reset(ctx: &mut Ctx, file: &Path) -> Result<Map<String, Value>> {
    let Ok(text) = tokio::fs::read_to_string(file).await else {
        return Ok(Map::new());
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        _ => {
            let aside = file.with_extension("json.unreadable");
            tokio::fs::rename(file, &aside).await?;
            ctx.note(format!(
                "The Claude Code profile config was unreadable; the old file is at {}",
                contract_tilde(&aside, &ctx.paths.home())
            ));
            Ok(Map::new())
        }
    }
}

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
//! read-modify-write, never a template, and `config::write_atomic` — riabuild's
//! one atomic write — so the new content lands whole or not at all under a name
//! no second riabuild is also staging into.

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
    // riabuild's one atomic write, rather than a staged file named here. The
    // name is the whole point: this used to be a *constant*
    // `.claude.json.riabuild-tmp`, and three tasks plus `riabuild claude new`
    // reach this function — the last of them from outside `.provision.lock`.
    // Two runs touching one account therefore staged into the same path, and
    // whichever renamed second published the other's half-written buffer over
    // the developer's session history and MCP servers. `write_atomic` names the
    // temporary for this process and refuses one that already exists, so the
    // two writers cannot meet; the loser of the rename race loses only its own
    // edit, which is the ordinary lost-update this file's read-modify-write
    // already accepts.
    riabuild_paths::config::write_atomic(&file, text.as_bytes()).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ctx_with;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;
    use std::sync::Arc;

    /// Two riabuilds editing one account's `.claude.json` at the same time.
    ///
    /// The shape this pins is not "the last writer wins" — a lost update is
    /// what the read-modify-write here already accepts — but that neither
    /// writer is *broken* by the other. With a constant
    /// `.claude.json.riabuild-tmp` both staged into one path: one truncated the
    /// other's buffer, one renamed a file it had not written, and the loser's
    /// own rename failed on a name that was no longer there. All three are
    /// invisible under `--check`, and `riabuild claude new` reaches this
    /// function from outside `.provision.lock`.
    ///
    /// Repeated because the interleaving is the scheduler's to choose. One pass
    /// can get lucky; twenty-five in a row cannot.
    #[tokio::test]
    async fn two_concurrent_edits_leave_a_config_claude_code_can_start_against() {
        let (mut left, home) = ctx_with(FakeRunner::new()).await;
        let (mut right, _elsewhere) = ctx_with(FakeRunner::new()).await;
        // A second riabuild on the *same* machine, not a second machine: what
        // makes this a race is the one path both processes write.
        right.paths = Arc::new(RealPaths::rooted_at(home.path()));
        let id = "550e8400-e29b-41d4-a716-446655440000";

        for round in 0..25 {
            let file = config_file(&left, id);
            let _ = tokio::fs::remove_file(&file).await;

            let (first, second) = tokio::join!(
                edit(&mut left, id, |root| {
                    root.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
                }),
                edit(&mut right, id, |root| {
                    root.insert("hasCompletedOnboarding".into(), Value::Bool(true));
                }),
            );
            first.unwrap_or_else(|error| panic!("round {round}: first edit failed: {error:#}"));
            second.unwrap_or_else(|error| panic!("round {round}: second edit failed: {error:#}"));

            let text = tokio::fs::read_to_string(&file).await.expect("read back");
            let parsed: Value = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("round {round}: {error} in {text:?}"));
            // Whole or nothing. Which edits survive is the scheduler's to
            // decide — one may read the other's file and carry both keys, or
            // land alone — but what is on disk is always one writer's complete
            // buffer, never a splice of two or a truncation of either.
            let Value::Object(map) = &parsed else {
                panic!("round {round}: not an object — {parsed}");
            };
            assert!(!map.is_empty(), "round {round}: {parsed}");
            for (key, value) in map {
                assert!(
                    matches!(
                        key.as_str(),
                        "hasTrustDialogAccepted" | "hasCompletedOnboarding"
                    ) && value == &Value::Bool(true),
                    "round {round}: {parsed}"
                );
            }
        }
    }

    /// The temporary never survives the write, so the account directory a later
    /// `check()` reads holds the config and nothing beside it.
    #[tokio::test]
    async fn an_edit_leaves_no_temporary_behind() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let id = "550e8400-e29b-41d4-a716-446655440000";
        edit(&mut ctx, id, |root| {
            root.insert("hasCompletedOnboarding".into(), Value::Bool(true));
        })
        .await
        .expect("edit");

        let dir = ctx.paths.claude_profile_dir(id);
        let mut entries = tokio::fs::read_dir(&dir).await.expect("read_dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        assert_eq!(names, vec![".claude.json"], "in {}", dir.display());
    }
}

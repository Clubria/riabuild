//! The per-account `.claude.json` — Claude Code's own first-run state.
//!
//! Not a task. Three tasks write here through [`edit`]: `claude_trust` records
//! the trusted checkout, `claude_onboarding` records that the first-run setup
//! is done, and `claude_agents_view` settles which view a session opens in.
//! None of
//! those facts is expressible in a settings file, which is the only reason
//! riabuild writes into this file at all. `riabuild claude new` reaches `edit`
//! too, from outside `.provision.lock`.
//!
//! Every edit shares the same hazards, so the read-modify-write lives here once
//! rather than three times: the file is live state Claude Code may be rewriting
//! this instant, every key riabuild does not own has to survive, and a
//! half-written config is one Claude Code cannot start against. Hence
//! read-modify-write, never a template, and `config::write_atomic` — riabuild's
//! one atomic write — so the new content lands whole or not at all under a name
//! no second riabuild is also staging into.
//!
//! **Whole is not the same as kept**, which is what the lock in [`edit`] is
//! for. Two riabuilds that both read before either writes each land a complete
//! file and one of them lands second, so the first one's key is simply gone —
//! and the developer meets the dialog it was written to prevent. The lock makes
//! the read and the write one turn; `write_atomic` is still what makes each
//! turn whole, and still carries the case where a filesystem refuses to lock.
//!
//! What it does **not** cover is the fourth writer. `claude_plugins` writes
//! this file by running `claude`, which knows nothing about riabuild's lock —
//! see `claude_plugins`'s own note. Inside one riabuild `Task::writes` keeps it
//! away from the other three; across two, it is Claude Code's file and riabuild
//! is the guest.

use super::Ctx;
use anyhow::Result;
use riabuild_paths::contract_tilde;
use riabuild_paths::filelock::FileLock;
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

    // The read, the change and the write are one transaction, which is the
    // whole reason this is taken *here* rather than around the write below.
    // Two riabuilds that both read before either writes are two edits and one
    // survivor, and `write_atomic` cannot help with that: it makes each write
    // whole, and a lost update is two writes that were each perfectly whole.
    // The one this was losing is a developer's own — `hasTrustDialogAccepted`
    // dropped by a `riabuild claude new` that read a moment earlier means the
    // trust dialog is back on their next launch, which is exactly the thing
    // these tasks exist to prevent.
    //
    // It is the third lock a riabuild can hold, after `.provision.lock` and
    // `.state.lock`, and there is no cycle between them: nothing takes either
    // of those while holding this one. Three *different* files, which is what
    // `std` requires — a second lock on a file this process already holds is
    // unspecified and may deadlock. Within one process `Task::writes` keeps
    // the four `.claude.json` writers off each other already, so this one is
    // only ever contended between processes.
    let _lock = FileLock::acquire(&ctx.paths.claude_config_lock_file(id), || {}).await?;

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
    // two writers cannot meet even if the lock above could not be taken —
    // which is a real case, because `FileLock` fails open on a filesystem that
    // refuses to lock at all.
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
    /// Two properties, and the second one used to be false. Neither writer is
    /// *broken* by the other: with a constant `.claude.json.riabuild-tmp` both
    /// staged into one path, so one truncated the other's buffer, one renamed a
    /// file it had not written, and the loser's own rename failed on a name
    /// that was no longer there. And neither writer's edit is *lost*: both read
    /// before either wrote, so the second one published a document assembled
    /// from a file that no longer existed by the time it landed.
    ///
    /// A lost update reads as riabuild not having run. The developer meets the
    /// trust dialog, or the onboarding, on the next launch — with `check()`
    /// reporting satisfied, because by then the file says what the *other* task
    /// wrote and each task only looks at its own key. That is the failure this
    /// costs one lock to remove, and `riabuild claude new` reaches this
    /// function from outside `.provision.lock`, so it is not a rare shape.
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
            // Whole *and* kept. The order the two edits land in is still the
            // scheduler's to choose, and it does not matter: the second reads
            // inside the lock, so it reads the first one's file and carries
            // both keys forward. Asserting only that the document parses would
            // pass just as well with no lock at all.
            let Value::Object(map) = &parsed else {
                panic!("round {round}: not an object — {parsed}");
            };
            for key in ["hasTrustDialogAccepted", "hasCompletedOnboarding"] {
                assert_eq!(
                    map.get(key),
                    Some(&Value::Bool(true)),
                    "round {round}: an edit was lost — {parsed}"
                );
            }
            assert_eq!(map.len(), 2, "round {round}: {parsed}");
        }
    }

    /// The temporary never survives the write.
    ///
    /// The lock file does, and is meant to: it is what makes the read and the
    /// write one turn, so it has to outlive both — see
    /// `Paths::claude_config_lock_file`. Listing the directory exactly is the
    /// point of the test. A staged `.claude.json.part` left behind here is a
    /// leak nobody sweeps, and this is the only thing that would notice it.
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
        assert_eq!(
            names,
            vec![".claude.json", ".claude.json.lock"],
            "in {}",
            dir.display()
        );
    }
}

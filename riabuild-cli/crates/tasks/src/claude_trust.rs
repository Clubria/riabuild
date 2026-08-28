//! Task 10 — the checkout, trusted, so Claude Code starts working immediately.
//!
//! Everything else riabuild tells Claude Code is settings data layered at launch
//! by the account launchers. Trust cannot be: `hasTrustDialogAccepted` is per-project
//! state in `.claude.json`, keyed by absolute path, and no settings file can
//! express it. Claude Code says as much in its own diagnostic — *"Run Claude
//! Code interactively here once and accept the trust dialog, or set
//! projects[<path>].hasTrustDialogAccepted: true"*.
//!
//! Until it is set, the first `claude` in a fresh checkout opens a modal, and the
//! settings the org ships are held back as untrusted. That is the one dialog a
//! provisioner cannot leave for the developer to meet on their own.
//!
//! Every riabuild-owned account is touched — every `~/.riabuild/claude/<uuid>/`
//! config directory in `claude_accounts` — never the developer's own
//! `~/.claude.json`. Each account's file is live state Claude Code rewrites
//! constantly, so each one is a read-modify-write that preserves every key it
//! does not own, not a template.

use super::claude_config::{self, Stored};
use super::{Ctx, Resource, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_paths::contract_tilde;
use riabuild_ui::Failure;
use serde_json::{Map, Value, json};
use std::path::Path;

pub struct ClaudeTrust;

/// The shape Claude Code gives a project entry it creates itself. Writing the
/// whole thing keeps riabuild's entries indistinguishable from Claude's own,
/// rather than betting that every field is optional.
fn new_project_entry() -> Value {
    json!({
        "allowedTools": [],
        "mcpContextUris": [],
        "mcpServers": {},
        "enabledMcpjsonServers": [],
        "disabledMcpjsonServers": [],
        "hasTrustDialogAccepted": true,
        "projectOnboardingSeenCount": 0,
        "hasClaudeMdExternalIncludesApproved": false,
        "hasClaudeMdExternalIncludesWarningShown": false
    })
}

/// Every path Claude Code might key the checkout under.
///
/// `pub(crate)` alongside `trust_one`, so `riabuild claude new` can trust the
/// account it just created rather than leaving it for the next `riabuild` run —
/// see the call site for why that window matters.
///
/// It derives the key from the real path of its working directory, but probes
/// the literal path too. A symlinked checkout — or a home directory that is
/// itself a symlink — makes those two different strings, and trust written
/// under one is invisible under the other. Writing both costs a few bytes and
/// removes the failure where riabuild reports a trusted checkout and the
/// developer still gets the dialog.
pub(crate) async fn trust_keys(dir: &Path) -> Vec<String> {
    let mut keys = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = tokio::fs::canonicalize(dir).await {
        let real = real.to_string_lossy().into_owned();
        if !keys.contains(&real) {
            keys.push(real);
        }
    }
    keys
}

fn is_trusted(root: &Map<String, Value>, key: &str) -> bool {
    root.get("projects")
        .and_then(|projects| projects.get(key))
        .and_then(|entry| entry.get("hasTrustDialogAccepted"))
        == Some(&Value::Bool(true))
}

#[async_trait]
impl Task for ClaudeTrust {
    fn id(&self) -> TaskId {
        "claude_trust"
    }

    fn title(&self) -> &str {
        "Trusted checkout"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // The accounts task supplies the config files to write into, one per
        // account; the project task supplies the path being trusted. A checkout
        // moved by `project` has to be re-trusted at its new path, which is what
        // this edge buys.
        &["claude_accounts", "project"]
    }

    /// The per-account `.claude.json`, which three sibling tasks in this wave
    /// also write. See `Task::writes`.
    fn writes(&self) -> &[Resource] {
        &["claude_config"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        if ctx.config.claude_accounts.is_empty() {
            return Ok(Status::needs("no Claude Code account yet"));
        }
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory yet"));
        };
        let keys = trust_keys(&dir).await;
        let shown = contract_tilde(&dir, &ctx.paths.home());

        for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
            let number = index + 1;
            match claude_config::read(ctx, id).await {
                Stored::Missing => {
                    return Ok(Status::needs(format!(
                        "account {number} has no Claude Code config yet"
                    )));
                }
                Stored::Unreadable => {
                    // Claude Code cannot start against this, so the machine is
                    // broken whatever the trust key says.
                    return Ok(Status::needs(format!(
                        "the Claude Code config for account {number} is not valid JSON"
                    )));
                }
                Stored::Present(root) => {
                    if !keys.iter().all(|key| is_trusted(&root, key)) {
                        return Ok(Status::needs(format!(
                            "{shown} is not trusted by account {number} yet"
                        )));
                    }
                }
            }
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if ctx.config.claude_accounts.is_empty() {
            return Err(Failure::new(
                "trusting the checkout",
                "Run `riabuild` again — a Claude Code account has to exist first.",
            )
            .into());
        }
        let Some(dir) = ctx.project_dir() else {
            return Err(Failure::new(
                "trusting the checkout",
                "Run `riabuild` again — the checkout has to exist first.",
            )
            .into());
        };
        let keys = trust_keys(&dir).await;

        for id in ctx.config.claude_accounts.clone() {
            trust_one(ctx, &id, &keys).await?;
        }
        Ok(())
    }
}

/// Writes the trust key into one account's config, preserving every key it does
/// not own. Claude Code may be running against this file right now, so the new
/// content lands whole or not at all.
///
/// Per-account rather than per-run so that `riabuild claude new` can reach it:
/// an account created between two `riabuild` runs would otherwise meet the trust
/// dialog on its first launch, which is precisely when the developer is about to
/// use it.
pub(crate) async fn trust_one(ctx: &mut Ctx, id: &str, keys: &[String]) -> Result<()> {
    claude_config::edit(ctx, id, |root| {
        let mut projects = match root.remove("projects") {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };

        for key in keys {
            match projects.get_mut(key) {
                Some(Value::Object(entry)) => {
                    entry.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
                }
                _ => {
                    projects.insert(key.clone(), new_project_entry());
                }
            }
        }
        root.insert("projects".into(), Value::Object(projects));
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::new_id;
    use crate::testing::{ctx_with, write_file};
    use claude_config::config_file;
    use riabuild_runner::FakeRunner;
    use std::path::PathBuf;

    /// A ctx with two accounts and a real checkout directory on disk.
    async fn ready() -> (Ctx, tempfile::TempDir, Vec<String>, PathBuf) {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let mut ids = Vec::new();
        for _ in 0..2 {
            let id = new_id();
            tokio::fs::create_dir_all(ctx.paths.claude_dir().join(&id))
                .await
                .expect("account dir");
            ids.push(id);
        }
        let dir = home.path().join("code/hub");
        tokio::fs::create_dir_all(&dir).await.expect("checkout");

        ctx.config.claude_accounts = ids.clone();
        ctx.config.project_path = Some(dir.to_string_lossy().into_owned());
        (ctx, home, ids, dir)
    }

    #[tokio::test]
    async fn a_machine_without_a_profile_is_not_claimed_to_be_trusted() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            ClaudeTrust.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn an_untrusted_checkout_is_detected() {
        let (ctx, _home, ids, _dir) = ready().await;
        write_file(&config_file(&ctx, &ids[0]), r#"{"numStartups": 3}"#).await;

        let status = ClaudeTrust.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not trusted"), "{status:?}");
    }

    #[tokio::test]
    async fn trust_recorded_for_another_checkout_does_not_count() {
        let (ctx, _home, ids, _dir) = ready().await;
        write_file(
            &config_file(&ctx, &ids[0]),
            r#"{"projects":{"/somewhere/else":{"hasTrustDialogAccepted":true}}}"#,
        )
        .await;

        let status = ClaudeTrust.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not trusted"), "{status:?}");
    }

    #[tokio::test]
    async fn one_trusted_account_is_not_enough() {
        // claude-2 would open the trust modal on first launch and hold the
        // org's settings back as untrusted — the exact dialog this task exists
        // to prevent, just one account over.
        let (mut ctx, _home, ids, _dir) = ready().await;
        write_file(&config_file(&ctx, &ids[0]), r#"{"numStartups":1}"#).await;
        ClaudeTrust.apply(&mut ctx).await.unwrap();

        // Now break only the second account's trust.
        write_file(&config_file(&ctx, &ids[1]), r#"{"numStartups":1}"#).await;
        let status = ClaudeTrust.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not trusted"), "{status:?}");
        assert!(format!("{status:?}").contains('2'), "{status:?}");
    }

    #[tokio::test]
    async fn applying_trusts_every_account() {
        let (mut ctx, _home, ids, dir) = ready().await;
        ClaudeTrust.apply(&mut ctx).await.unwrap();

        let key = dir.to_string_lossy().into_owned();
        for id in &ids {
            let text = tokio::fs::read_to_string(config_file(&ctx, id))
                .await
                .unwrap();
            let root: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                root["projects"][&key]["hasTrustDialogAccepted"],
                json!(true),
                "{id}"
            );
        }
        assert_eq!(ClaudeTrust.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_keeps_everything_else_in_the_config() {
        let (mut ctx, _home, ids, _dir) = ready().await;
        write_file(
            &config_file(&ctx, &ids[0]),
            r#"{"numStartups":7,"projects":{"/other":{"hasTrustDialogAccepted":true,"allowedTools":["Bash"]}}}"#,
        )
        .await;

        ClaudeTrust.apply(&mut ctx).await.unwrap();

        let text = tokio::fs::read_to_string(config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        let root: Value = serde_json::from_str(&text).unwrap();
        // Session state and other projects are the developer's, not riabuild's.
        assert_eq!(root["numStartups"], json!(7));
        assert_eq!(root["projects"]["/other"]["allowedTools"], json!(["Bash"]));
        assert_eq!(ClaudeTrust.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn an_existing_entry_is_edited_rather_than_replaced() {
        let (mut ctx, _home, ids, dir) = ready().await;
        let key = dir.to_string_lossy().into_owned();
        write_file(
            &config_file(&ctx, &ids[0]),
            &format!(
                r#"{{"projects":{{"{key}":{{"hasTrustDialogAccepted":false,"allowedTools":["Read"]}}}}}}"#
            ),
        )
        .await;

        ClaudeTrust.apply(&mut ctx).await.unwrap();

        let text = tokio::fs::read_to_string(config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        let root: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(root["projects"][&key]["allowedTools"], json!(["Read"]));
        assert_eq!(ClaudeTrust.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home, ids, _dir) = ready().await;
        ClaudeTrust.apply(&mut ctx).await.unwrap();
        let first = tokio::fs::read_to_string(config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        ClaudeTrust.apply(&mut ctx).await.unwrap();
        let second = tokio::fs::read_to_string(config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn an_unreadable_config_is_moved_aside_rather_than_overwritten() {
        let (mut ctx, _home, ids, _dir) = ready().await;
        let file = config_file(&ctx, &ids[0]);
        write_file(&file, "{ not json").await;

        assert!(matches!(
            ClaudeTrust.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
        ClaudeTrust.apply(&mut ctx).await.unwrap();

        assert_eq!(ClaudeTrust.check(&ctx).await.unwrap(), Status::Satisfied);
        let aside = file.with_extension("json.unreadable");
        assert_eq!(
            tokio::fs::read_to_string(&aside).await.unwrap(),
            "{ not json"
        );
        assert!(!ctx.notes.is_empty(), "the developer is told where it went");
    }
}

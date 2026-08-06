//! Task 10 — the checkout, trusted, so Claude Code starts working immediately.
//!
//! Everything else riabuild tells Claude Code is settings data layered at launch
//! by the `c` shim. Trust cannot be: `hasTrustDialogAccepted` is per-project
//! state in `.claude.json`, keyed by absolute path, and no settings file can
//! express it. Claude Code says as much in its own diagnostic — *"Run Claude
//! Code interactively here once and accept the trust dialog, or set
//! projects[<path>].hasTrustDialogAccepted: true"*.
//!
//! Until it is set, the first `c` in a fresh checkout opens a modal, and the
//! settings the org ships are held back as untrusted. That is the one dialog a
//! provisioner cannot leave for the developer to meet on their own.
//!
//! Only the riabuild-owned profile is touched — `~/.riabuild/claude/<uuid>/` —
//! never the developer's own `~/.claude.json`. The file is live state Claude
//! Code rewrites constantly, so this is a read-modify-write that preserves every
//! key it does not own, not a template.

use super::{Ctx, Status, Task, TaskId};
use crate::paths::contract_tilde;
use crate::ui::Failure;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

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
/// It derives the key from the real path of its working directory, but probes
/// the literal path too. A symlinked checkout — or a home directory that is
/// itself a symlink — makes those two different strings, and trust written
/// under one is invisible under the other. Writing both costs a few bytes and
/// removes the failure where riabuild reports a trusted checkout and the
/// developer still gets the dialog.
async fn trust_keys(dir: &Path) -> Vec<String> {
    let mut keys = vec![dir.to_string_lossy().into_owned()];
    if let Ok(real) = tokio::fs::canonicalize(dir).await {
        let real = real.to_string_lossy().into_owned();
        if !keys.contains(&real) {
            keys.push(real);
        }
    }
    keys
}

fn is_trusted(root: &Value, key: &str) -> bool {
    root.get("projects")
        .and_then(|projects| projects.get(key))
        .and_then(|entry| entry.get("hasTrustDialogAccepted"))
        == Some(&Value::Bool(true))
}

fn config_file(ctx: &Ctx, profile: &str) -> PathBuf {
    ctx.paths.claude_config_file(profile)
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
        // The profile supplies the config file to write into; the project
        // supplies the path being trusted. A checkout moved by `project` has to
        // be re-trusted at its new path, which is what this edge buys.
        &["claude_profiles", "project"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(profile) = ctx.config.claude_profile.clone() else {
            return Ok(Status::needs("no Claude Code profile yet"));
        };
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory yet"));
        };

        let file = config_file(ctx, &profile);
        let Ok(text) = tokio::fs::read_to_string(&file).await else {
            return Ok(Status::needs("the Claude Code profile has no config yet"));
        };
        let Ok(root) = serde_json::from_str::<Value>(&text) else {
            // Claude Code cannot start against this, so the machine is broken
            // whatever the trust key says.
            return Ok(Status::needs(
                "the Claude Code profile config is not valid JSON",
            ));
        };

        for key in trust_keys(&dir).await {
            if !is_trusted(&root, &key) {
                return Ok(Status::needs(format!(
                    "{} is not trusted by Claude Code yet",
                    contract_tilde(&dir, &ctx.paths.home())
                )));
            }
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let Some(profile) = ctx.config.claude_profile.clone() else {
            return Err(Failure::new(
                "trusting the checkout",
                "Run `riabuild` again — the Claude Code profile has to exist first.",
            )
            .into());
        };
        let Some(dir) = ctx.project_dir() else {
            return Err(Failure::new(
                "trusting the checkout",
                "Run `riabuild` again — the checkout has to exist first.",
            )
            .into());
        };

        let file = config_file(ctx, &profile);
        if let Some(parent) = file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut root = load_or_reset(ctx, &file).await?;
        let mut projects = match root.remove("projects") {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };

        for key in trust_keys(&dir).await {
            match projects.get_mut(&key) {
                Some(Value::Object(entry)) => {
                    entry.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
                }
                _ => {
                    projects.insert(key, new_project_entry());
                }
            }
        }
        root.insert("projects".into(), Value::Object(projects));

        // Claude Code may be running against this file right now, so the new
        // content lands whole or not at all.
        let text = serde_json::to_string_pretty(&Value::Object(root))?;
        let staged = file.with_extension("json.riabuild-tmp");
        tokio::fs::write(&staged, text).await?;
        tokio::fs::rename(&staged, &file).await?;
        Ok(())
    }
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
    use crate::runner::FakeRunner;
    use crate::tasks::claude_profiles::new_profile_id;
    use crate::testing::{ctx_with, write_file};
    use std::path::PathBuf;

    /// A ctx with a profile and a real checkout directory on disk.
    async fn ready() -> (Ctx, tempfile::TempDir, String, PathBuf) {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let profile = new_profile_id();
        tokio::fs::create_dir_all(ctx.paths.claude_dir().join(&profile))
            .await
            .expect("profile dir");
        let dir = home.path().join("code/hub");
        tokio::fs::create_dir_all(&dir).await.expect("checkout");

        ctx.config.claude_profile = Some(profile.clone());
        ctx.config.project_path = Some(dir.to_string_lossy().into_owned());
        (ctx, home, profile, dir)
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
        let (ctx, _home, profile, _dir) = ready().await;
        write_file(&config_file(&ctx, &profile), r#"{"numStartups": 3}"#).await;

        let status = ClaudeTrust.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not trusted"), "{status:?}");
    }

    #[tokio::test]
    async fn trust_recorded_for_another_checkout_does_not_count() {
        let (ctx, _home, profile, _dir) = ready().await;
        write_file(
            &config_file(&ctx, &profile),
            r#"{"projects":{"/somewhere/else":{"hasTrustDialogAccepted":true}}}"#,
        )
        .await;

        let status = ClaudeTrust.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not trusted"), "{status:?}");
    }

    #[tokio::test]
    async fn applying_trusts_the_checkout() {
        let (mut ctx, _home, _profile, _dir) = ready().await;
        ClaudeTrust.apply(&mut ctx).await.unwrap();
        assert_eq!(ClaudeTrust.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_keeps_everything_else_in_the_config() {
        let (mut ctx, _home, profile, _dir) = ready().await;
        write_file(
            &config_file(&ctx, &profile),
            r#"{"numStartups":7,"projects":{"/other":{"hasTrustDialogAccepted":true,"allowedTools":["Bash"]}}}"#,
        )
        .await;

        ClaudeTrust.apply(&mut ctx).await.unwrap();

        let text = tokio::fs::read_to_string(config_file(&ctx, &profile))
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
        let (mut ctx, _home, profile, dir) = ready().await;
        let key = dir.to_string_lossy().into_owned();
        write_file(
            &config_file(&ctx, &profile),
            &format!(
                r#"{{"projects":{{"{key}":{{"hasTrustDialogAccepted":false,"allowedTools":["Read"]}}}}}}"#
            ),
        )
        .await;

        ClaudeTrust.apply(&mut ctx).await.unwrap();

        let text = tokio::fs::read_to_string(config_file(&ctx, &profile))
            .await
            .unwrap();
        let root: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(root["projects"][&key]["allowedTools"], json!(["Read"]));
        assert_eq!(ClaudeTrust.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home, profile, _dir) = ready().await;
        ClaudeTrust.apply(&mut ctx).await.unwrap();
        let first = tokio::fs::read_to_string(config_file(&ctx, &profile))
            .await
            .unwrap();
        ClaudeTrust.apply(&mut ctx).await.unwrap();
        let second = tokio::fs::read_to_string(config_file(&ctx, &profile))
            .await
            .unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn an_unreadable_config_is_moved_aside_rather_than_overwritten() {
        let (mut ctx, _home, profile, _dir) = ready().await;
        let file = config_file(&ctx, &profile);
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

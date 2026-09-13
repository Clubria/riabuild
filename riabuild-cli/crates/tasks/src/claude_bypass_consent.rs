//! The bypass-permissions disclaimer, already accepted — where Claude Code
//! will still believe it.
//!
//! The org settings ask for `permissions.defaultMode: "bypassPermissions"` and
//! carry `skipDangerousModePermissionPrompt: true` beside it, and for a while
//! that pair was enough. It is not any more. Claude Code 2.1.270 checks the
//! disclaimer again before starting a **background** session — every session
//! the agents view dispatches, which is every session a bare `claude` starts —
//! and that check reads only two sources:
//!
//! ```text
//! downgrade = CLAUDE_CODE_SESSION_KIND === "bg"
//!     && !(policySettings.skipDangerousModePermissionPrompt
//!          || userSettings.skipDangerousModePermissionPrompt)
//!     && !globalConfig.bypassPermissionsModeAccepted
//! ```
//!
//! `flagSettings` — the file every launcher passes to `--settings` — is not
//! one of them, so the session starts in `default` with "Permission mode
//! downgraded to default — bypass requires accepting the disclaimer
//! interactively first", and a background session has nobody to accept it.
//! Reproduced against 2.1.270 with `CLAUDE_CODE_SESSION_KIND=bg`: the org file
//! alone starts in `default`, the account's own `settings.json` starts in
//! `bypassPermissions`. The exclusion is deliberate on Claude Code's side, and
//! riabuild does not argue with it: a flag is something any process can pass,
//! and consent is meant to be something the account holds.
//!
//! **So the account holds it.** This writes the key into each account's own
//! `settings.json` — the `userSettings` source, and exactly the key and file
//! Claude Code itself writes when a developer accepts the dialog. The other two
//! routes are both worse. `policySettings` is `/etc/claude-code`, which needs
//! root and is machine-wide rather than per account. And
//! `bypassPermissionsModeAccepted` in `.claude.json` is a legacy key Claude Code
//! migrates out of that file at startup — reproduced: the key is gone after the
//! first launch and the session was still downgraded — so a task that wrote it
//! would report drift on every run and fix nothing.
//!
//! **Consent, not policy.** Nothing here chooses the mode: an account whose
//! org settings ask for `default` still starts in `default`, and
//! `disableBypassPermissionsMode` still takes the mode away. It is the same
//! bargain `claude_onboarding` makes — the developer is on a managed
//! environment whose launchers already pass `--allow-dangerously-skip-permissions`
//! on every line, so the question the dialog asks has been answered before
//! they arrive. It is also why this is not in the dashboard's settings JSON
//! alone any more: the key there still covers a foreground session, and is
//! read by nothing that decides a background one.
//!
//! Re-read the check above when the pinned Claude Code version moves. It is
//! undocumented, and the source list is the part that changed underneath the
//! org settings the first time.

use super::claude_config::{self, Stored};
use super::{Ctx, Resource, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_ui::Failure;
use serde_json::Value;

pub struct ClaudeBypassConsent;

const KEY: &str = "skipDangerousModePermissionPrompt";

#[async_trait]
impl Task for ClaudeBypassConsent {
    fn id(&self) -> TaskId {
        "claude_bypass_consent"
    }

    fn title(&self) -> &str {
        "Claude Code bypass-permissions consent"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // The accounts task supplies the config directories to write into. Not
        // `org_settings`: consent is the same answer whatever the team's file
        // says, and an account on a machine that has never fetched it still
        // launches with `--allow-dangerously-skip-permissions`.
        &["claude_accounts"]
    }

    /// The per-account `settings.json`, which `claude_plugins` also writes by
    /// running `claude plugin install`. See `Task::writes`.
    fn writes(&self) -> &[Resource] {
        &["claude_settings"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        if ctx.config.claude_accounts.is_empty() {
            return Ok(Status::needs("no Claude Code account yet"));
        }

        for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
            let number = index + 1;
            match claude_config::read_settings(ctx, id).await {
                Stored::Unreadable => {
                    return Ok(Status::needs(format!(
                        "the Claude Code settings for account {number} are not valid JSON"
                    )));
                }
                Stored::Present(root) if root.get(KEY) == Some(&Value::Bool(true)) => {}
                Stored::Missing | Stored::Present(_) => {
                    return Ok(Status::needs(format!(
                        "account {number} would start agents-view sessions with permission prompts on"
                    )));
                }
            }
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if ctx.config.claude_accounts.is_empty() {
            return Err(Failure::new(
                "recording Claude Code's bypass-permissions consent",
                "Run `riabuild` again — a Claude Code account has to exist first.",
            )
            .into());
        }

        for id in ctx.config.claude_accounts.clone() {
            consent_one(ctx, &id).await?;
        }
        Ok(())
    }
}

/// Records the disclaimer as accepted for one account, preserving every setting
/// riabuild does not own.
///
/// `pub(crate)` alongside `claude_onboarding::complete_one`, so `riabuild claude
/// new` can settle the account it just created rather than leaving its first
/// agents-view session to be downgraded.
pub(crate) async fn consent_one(ctx: &mut Ctx, id: &str) -> Result<()> {
    claude_config::edit_settings(ctx, id, |root| {
        root.insert(KEY.into(), Value::Bool(true));
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::new_id;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;

    /// A ctx with two registered accounts and their config directories.
    async fn ready() -> (Ctx, tempfile::TempDir, Vec<String>) {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let mut ids = Vec::new();
        for _ in 0..2 {
            let id = new_id();
            tokio::fs::create_dir_all(ctx.paths.claude_dir().join(&id))
                .await
                .expect("account dir");
            ids.push(id);
        }
        ctx.config.claude_accounts = ids.clone();
        (ctx, home, ids)
    }

    async fn settings(ctx: &Ctx, id: &str) -> Value {
        let text = tokio::fs::read_to_string(ctx.paths.claude_settings_file(id))
            .await
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn a_machine_without_an_account_is_not_claimed_to_be_done() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            ClaudeBypassConsent.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn a_fresh_account_with_no_settings_file_is_detected() {
        // The shape `claude auth login` leaves: no `settings.json` at all, so
        // the first session the agents view starts is downgraded.
        let (ctx, _home, _ids) = ready().await;
        let status = ClaudeBypassConsent.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("permission prompts"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn consent_in_the_accounts_claude_json_does_not_count() {
        // The legacy key Claude Code migrates out of `.claude.json` at startup.
        // A check satisfied by it would pass on a machine whose sessions are
        // still downgraded.
        let (ctx, _home, ids) = ready().await;
        for id in &ids {
            write_file(
                &claude_config::config_file(&ctx, id),
                r#"{"bypassPermissionsModeAccepted":true}"#,
            )
            .await;
        }
        assert!(matches!(
            ClaudeBypassConsent.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn consent_recorded_as_false_does_not_count() {
        let (ctx, _home, ids) = ready().await;
        for id in &ids {
            write_file(
                &ctx.paths.claude_settings_file(id),
                r#"{"skipDangerousModePermissionPrompt":false}"#,
            )
            .await;
        }
        assert!(matches!(
            ClaudeBypassConsent.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn one_consenting_account_is_not_enough() {
        let (mut ctx, _home, ids) = ready().await;
        ClaudeBypassConsent.apply(&mut ctx).await.unwrap();

        write_file(
            &ctx.paths.claude_settings_file(&ids[1]),
            r#"{"model":"opus"}"#,
        )
        .await;
        let status = ClaudeBypassConsent.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains('2'), "{status:?}");
    }

    #[tokio::test]
    async fn applying_settles_every_account() {
        let (mut ctx, _home, ids) = ready().await;
        ClaudeBypassConsent.apply(&mut ctx).await.unwrap();

        for id in &ids {
            assert_eq!(settings(&ctx, id).await[KEY], Value::Bool(true), "{id}");
        }
        assert_eq!(
            ClaudeBypassConsent.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    #[tokio::test]
    async fn applying_keeps_the_developers_own_settings() {
        // Everything else in this file is the developer's: `/model`, `/config`
        // and user-scope plugins all land here.
        let (mut ctx, _home, ids) = ready().await;
        write_file(
            &ctx.paths.claude_settings_file(&ids[0]),
            r#"{"model":"sonnet","enabledPlugins":{"superpowers@official":true}}"#,
        )
        .await;

        ClaudeBypassConsent.apply(&mut ctx).await.unwrap();

        let root = settings(&ctx, &ids[0]).await;
        assert_eq!(root["model"], "sonnet");
        assert_eq!(root["enabledPlugins"]["superpowers@official"], true);
        assert_eq!(root[KEY], true);
    }

    #[tokio::test]
    async fn applying_writes_nothing_into_claude_json() {
        // Two files, two owners: this task must not become a fifth
        // `.claude.json` writer the `claude_config` resource does not name.
        let (mut ctx, _home, ids) = ready().await;
        ClaudeBypassConsent.apply(&mut ctx).await.unwrap();
        for id in &ids {
            assert!(
                !tokio::fs::try_exists(claude_config::config_file(&ctx, id))
                    .await
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home, ids) = ready().await;
        ClaudeBypassConsent.apply(&mut ctx).await.unwrap();
        let first = tokio::fs::read_to_string(ctx.paths.claude_settings_file(&ids[0]))
            .await
            .unwrap();
        ClaudeBypassConsent.apply(&mut ctx).await.unwrap();
        let second = tokio::fs::read_to_string(ctx.paths.claude_settings_file(&ids[0]))
            .await
            .unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn unreadable_settings_are_moved_aside_rather_than_overwritten() {
        let (mut ctx, _home, ids) = ready().await;
        let file = ctx.paths.claude_settings_file(&ids[0]);
        write_file(&file, "{ not json").await;

        assert!(matches!(
            ClaudeBypassConsent.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
        ClaudeBypassConsent.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ClaudeBypassConsent.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
        let aside = file.with_extension("json.unreadable");
        assert_eq!(
            tokio::fs::read_to_string(&aside).await.unwrap(),
            "{ not json"
        );
        assert!(!ctx.notes.is_empty(), "the developer is told where it went");
    }
}

//! Task 12 — the first-run setup, already done.
//!
//! `claude auth login` signs an account in without ever completing Claude Code's
//! onboarding: it writes `oauthAccount` and leaves `hasCompletedOnboarding`
//! unset. Claude Code gates the whole first-run flow on that one key in
//! `.claude.json` — verified against 2.1.228, where `showSetupScreens` reads
//! `if (!config.hasCompletedOnboarding) { …render Onboarding… }` — so an account
//! riabuild signed in still meets the full flow on its first interactive launch:
//! a theme picker, then a login step, then the security notes.
//!
//! The login step is the one that reads as a bug. It is pushed whenever OAuth is
//! *available*, never because the account is signed out, so a developer whose
//! `claude-1 auth status` reports `"loggedIn": true` is asked to log in anyway
//! and has no way to tell that answering it changes nothing.
//!
//! Like trust, this cannot be a settings key: `hasCompletedOnboarding` is state
//! in `.claude.json`, and `--settings` cannot reach it. Claude Code sets it
//! itself for enterprise-gateway sessions, which is the same bargain riabuild is
//! making — a managed environment answers the first-run questions on the
//! developer's behalf, because it already knows the answers.
//!
//! Deliberately narrow. This writes one boolean and no preferences: the theme
//! and the permission mode are org policy, and they arrive through the settings
//! file the launchers layer at launch. Writing a theme here would put riabuild's
//! answer in the one place the org's answer cannot override.

use super::claude_config::{self, Stored};
use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_ui::Failure;
use serde_json::Value;

pub struct ClaudeOnboarding;

const KEY: &str = "hasCompletedOnboarding";

#[async_trait]
impl Task for ClaudeOnboarding {
    fn id(&self) -> TaskId {
        "claude_onboarding"
    }

    fn title(&self) -> &str {
        "Claude Code first-run setup"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // The accounts task supplies the config directories to write into, one
        // per account, and is what signs them in — which is the act that leaves
        // onboarding half-done.
        //
        // Deliberately *not* `project`: unlike `claude_trust`, nothing here
        // needs a checkout, and a developer whose checkout has not landed yet
        // should still get a Claude Code that opens without interviewing them.
        &["claude_accounts"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        if ctx.config.claude_accounts.is_empty() {
            return Ok(Status::needs("no Claude Code account yet"));
        }

        for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
            let number = index + 1;
            match claude_config::read(ctx, id).await {
                Stored::Missing => {
                    return Ok(Status::needs(format!(
                        "account {number} has no Claude Code config yet"
                    )));
                }
                Stored::Unreadable => {
                    return Ok(Status::needs(format!(
                        "the Claude Code config for account {number} is not valid JSON"
                    )));
                }
                Stored::Present(root) => {
                    if root.get(KEY) != Some(&Value::Bool(true)) {
                        return Ok(Status::needs(format!(
                            "account {number} would still be asked Claude Code's first-run questions"
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
                "completing Claude Code's first-run setup",
                "Run `riabuild` again — a Claude Code account has to exist first.",
            )
            .into());
        }

        for id in ctx.config.claude_accounts.clone() {
            complete_one(ctx, &id).await?;
        }
        Ok(())
    }
}

/// Marks one account's first-run setup done, preserving every key riabuild does
/// not own.
///
/// `pub(crate)` alongside the trust equivalent, so `riabuild claude new` can
/// settle the account it just created rather than leaving it for the next
/// `riabuild` run.
pub(crate) async fn complete_one(ctx: &mut Ctx, id: &str) -> Result<()> {
    claude_config::edit(ctx, id, |root| {
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

    #[tokio::test]
    async fn a_machine_without_an_account_is_not_claimed_to_be_done() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            ClaudeOnboarding.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn a_signed_in_account_that_never_onboarded_is_detected() {
        // The exact shape `claude auth login` leaves behind: authenticated, and
        // still facing the theme picker and a redundant login prompt.
        let (ctx, _home, ids) = ready().await;
        write_file(
            &claude_config::config_file(&ctx, &ids[0]),
            r#"{"oauthAccount":{"emailAddress":"ada@clubria.com"}}"#,
        )
        .await;

        let status = ClaudeOnboarding.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("first-run"), "{status:?}");
    }

    #[tokio::test]
    async fn onboarding_recorded_as_false_does_not_count() {
        let (ctx, _home, ids) = ready().await;
        write_file(
            &claude_config::config_file(&ctx, &ids[0]),
            r#"{"hasCompletedOnboarding":false}"#,
        )
        .await;

        let status = ClaudeOnboarding.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("first-run"), "{status:?}");
    }

    #[tokio::test]
    async fn one_settled_account_is_not_enough() {
        // claude-2 would open the onboarding flow on first launch — the exact
        // interview this task exists to prevent, just one account over.
        let (mut ctx, _home, ids) = ready().await;
        ClaudeOnboarding.apply(&mut ctx).await.unwrap();

        write_file(
            &claude_config::config_file(&ctx, &ids[1]),
            r#"{"numStartups":1}"#,
        )
        .await;
        let status = ClaudeOnboarding.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("first-run"), "{status:?}");
        assert!(format!("{status:?}").contains('2'), "{status:?}");
    }

    #[tokio::test]
    async fn applying_settles_every_account() {
        let (mut ctx, _home, ids) = ready().await;
        ClaudeOnboarding.apply(&mut ctx).await.unwrap();

        for id in &ids {
            let text = tokio::fs::read_to_string(claude_config::config_file(&ctx, id))
                .await
                .unwrap();
            let root: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(root[KEY], Value::Bool(true), "{id}");
        }
        assert_eq!(
            ClaudeOnboarding.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    #[tokio::test]
    async fn applying_keeps_the_sign_in_and_everything_else() {
        // The whole point is that the developer stays signed in. A task that
        // settled onboarding by rewriting the file would log them out.
        let (mut ctx, _home, ids) = ready().await;
        write_file(
            &claude_config::config_file(&ctx, &ids[0]),
            r#"{"numStartups":7,"oauthAccount":{"emailAddress":"ada@clubria.com"},"projects":{"/other":{"hasTrustDialogAccepted":true}}}"#,
        )
        .await;

        ClaudeOnboarding.apply(&mut ctx).await.unwrap();

        let text = tokio::fs::read_to_string(claude_config::config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        let root: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(root["oauthAccount"]["emailAddress"], "ada@clubria.com");
        assert_eq!(root["numStartups"], 7);
        assert_eq!(root["projects"]["/other"]["hasTrustDialogAccepted"], true);
        assert_eq!(
            ClaudeOnboarding.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home, ids) = ready().await;
        ClaudeOnboarding.apply(&mut ctx).await.unwrap();
        let first = tokio::fs::read_to_string(claude_config::config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        ClaudeOnboarding.apply(&mut ctx).await.unwrap();
        let second = tokio::fs::read_to_string(claude_config::config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn an_unreadable_config_is_moved_aside_rather_than_overwritten() {
        let (mut ctx, _home, ids) = ready().await;
        let file = claude_config::config_file(&ctx, &ids[0]);
        write_file(&file, "{ not json").await;

        assert!(matches!(
            ClaudeOnboarding.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
        ClaudeOnboarding.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ClaudeOnboarding.check(&ctx).await.unwrap(),
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

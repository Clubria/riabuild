//! Task 7 — the developer's Claude Code accounts.
//!
//! riabuild creates the account directories and never writes into anyone's
//! `settings.json`. Org policy is layered at launch by the `claude-<n>`
//! launchers instead — see `org_settings` for why a recurring deep-merge is the
//! wrong shape.
//!
//! Account 1 is the one this task insists on: it must exist, and it must be
//! signed in. riabuild's job is "running Claude Code against our codebase", and
//! a signed-out Claude Code is not that. Accounts 2 upward are the developer's
//! own business — the account box reports them and this task ignores them.

use super::{Ctx, Status, Task, TaskId};
use crate::accounts::{self, status::Identity};
use crate::runner::RunOptions;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MIN_VERSION: &str = "2.0.0";

pub struct ClaudeAccounts;

/// Every account directory actually on disk, oldest first.
///
/// Oldest first so that adoption keeps a developer's original account as
/// account 1, which is the one their editor and their muscle memory point at.
async fn ids_on_disk(claude_dir: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(claude_dir).await else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, String)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !accounts::looks_like_id(&name) {
            continue;
        }
        found.push((meta.modified().unwrap_or(UNIX_EPOCH), name));
    }
    found.sort();
    found.into_iter().map(|(_, name)| name).collect()
}

#[async_trait]
impl Task for ClaudeAccounts {
    fn id(&self) -> TaskId {
        "claude_accounts"
    }

    fn title(&self) -> &str {
        "Claude Code accounts"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // Claude Code is installed with the Node riabuild owns, so the
        // toolchain has to exist first.
        &["toolchain"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let claude = ctx.claude();
        let reported = ctx
            .runner
            .run(&claude, &["--version"], &RunOptions::default())
            .await?;
        if !reported.ok() {
            return Ok(Status::needs("Claude Code is not installed"));
        }
        if !version::at_least(reported.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!(
                "Claude Code is older than {MIN_VERSION}"
            )));
        }

        let ids = &ctx.config.claude_accounts;
        let Some(primary) = ids.first() else {
            return Ok(Status::needs("no Claude Code account yet"));
        };
        for id in ids {
            let dir = ctx.paths.claude_profile_dir(id);
            if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
                return Ok(Status::needs("a Claude Code account directory is missing"));
            }
        }
        // A directory nothing recorded is drift in the other direction: real
        // sessions and a real login that no riabuild command can reach.
        for found in ids_on_disk(&ctx.paths.claude_dir()).await {
            if !ids.contains(&found) {
                return Ok(Status::needs("a Claude Code account is not registered"));
            }
        }

        match accounts::status::read(ctx, primary).await {
            Identity::LoggedIn(_) => Ok(Status::Satisfied),
            Identity::LoggedOut => Ok(Status::needs("account 1 is not signed in")),
            Identity::Unknown(why) => Ok(Status::needs(format!(
                "riabuild could not tell whether account 1 is signed in: {why}"
            ))),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let claude = ctx.claude();
        if !ctx
            .runner
            .run(&claude, &["--version"], &RunOptions::default())
            .await?
            .ok()
        {
            install_claude(ctx).await?;
        }

        let claude_dir = ctx.paths.claude_dir();
        tokio::fs::create_dir_all(&claude_dir).await?;

        let mut kept = Vec::new();
        for id in ctx.config.claude_accounts.clone() {
            if tokio::fs::try_exists(claude_dir.join(&id))
                .await
                .unwrap_or(false)
            {
                kept.push(id);
            }
        }
        for found in ids_on_disk(&claude_dir).await {
            if !kept.contains(&found) && kept.len() < accounts::MAX {
                kept.push(found);
            }
        }
        if kept.is_empty() {
            let id = accounts::new_id();
            tokio::fs::create_dir_all(claude_dir.join(&id)).await?;
            kept.push(id);
        }
        ctx.config.claude_accounts = kept;
        ctx.config.save(ctx.paths.as_ref()).await?;

        let Some(primary) = ctx.config.claude_accounts.first().cloned() else {
            return Ok(());
        };
        if !matches!(
            accounts::status::read(ctx, &primary).await,
            Identity::LoggedIn(_)
        ) {
            sign_in(ctx, &primary).await?;
        }
        Ok(())
    }
}

/// The one browser round trip provisioning makes for Claude Code.
///
/// Mirrors `github_cli::sign_in`, including checking the exit code: a developer
/// who abandons the browser must not leave riabuild convinced this machine is
/// ready, with the only symptom a later failure that says nothing about a
/// sign-in.
async fn sign_in(ctx: &mut Ctx, id: &str) -> Result<()> {
    ctx.ui
        .note("Opening your browser to sign in to Claude Code…");
    let claude = ctx.claude();
    let dir = ctx.paths.claude_profile_dir(id);
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };

    let code = ctx
        .runner
        .run_interactive(&claude, &["auth", "login"], &options)
        .await?;
    if code != 0 {
        return Err(Failure::new(
            "signing you in to Claude Code",
            "Run `riabuild` again and finish the Claude Code sign-in in your browser.",
        )
        .command("claude auth login")
        .detail(format!("that command exited with status {code}"))
        .into());
    }
    Ok(())
}

async fn install_claude(ctx: &mut Ctx) -> Result<()> {
    let node_version = match ctx.config.node_version.clone() {
        Some(pinned) => pinned,
        // Not `unwrap_or_else`: the fallback awaits, and a closure cannot.
        None => super::toolchain::desired_node(ctx.project_dir().as_deref()).await,
    };
    let npm = ctx.paths.node_dir(&node_version).join("bin").join("npm");

    if !tokio::fs::try_exists(&npm).await.unwrap_or(false) {
        return Err(Failure::new(
            "installing Claude Code",
            "Run `riabuild` again — the Node install has to finish first.",
        )
        .detail(format!("{} does not exist", npm.display()))
        .into());
    }

    ctx.ui.note("Installing Claude Code…");
    let output = ctx
        .runner
        .run(
            &npm.to_string_lossy(),
            &["install", "-g", "@anthropic-ai/claude-code"],
            &RunOptions::default(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
    use std::sync::Arc;

    const VERSION: &str = "claude --version";
    const STATUS: &str = "claude auth status --json";

    fn installed() -> FakeRunner {
        FakeRunner::new().with(VERSION, 0, "2.1.223 (Claude Code)", "")
    }

    fn signed_in() -> FakeRunner {
        installed().with(
            STATUS,
            0,
            r#"{"loggedIn":true,"email":"clubria@proton.me"}"#,
            "",
        )
    }

    /// A ctx with one account on disk and Claude Code installed and signed in.
    async fn ready() -> (Ctx, tempfile::TempDir, String) {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let id = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
            .await
            .unwrap();
        ctx.config.claude_accounts = vec![id.clone()];
        ctx.runner = Arc::new(signed_in());
        (ctx, home, id)
    }

    #[tokio::test]
    async fn a_missing_claude_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn an_old_claude_is_detected() {
        let runner = FakeRunner::new().with(VERSION, 0, "1.9.0 (Claude Code)", "");
        let (ctx, _home) = ctx_with(runner).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("older than"), "{status:?}");
    }

    #[tokio::test]
    async fn a_machine_with_no_account_is_detected() {
        let (ctx, _home) = ctx_with(installed()).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("no Claude Code account"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_deleted_account_directory_is_noticed() {
        let (mut ctx, _home) = ctx_with(installed()).await;
        tokio::fs::create_dir_all(ctx.paths.claude_dir())
            .await
            .unwrap();
        ctx.config.claude_accounts = vec![accounts::new_id()];
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("directory is missing"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_directory_nothing_recorded_is_noticed() {
        let (ctx, _home, _id) = ready().await;
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&accounts::new_id()))
            .await
            .unwrap();
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not registered"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_signed_out_primary_is_drift() {
        let (mut ctx, _home, _id) = ready().await;
        ctx.runner = Arc::new(installed().with(STATUS, 1, r#"{"loggedIn":false}"#, ""));
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("account 1 is not signed in"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_provisioned_machine_is_satisfied() {
        let (ctx, _home, _id) = ready().await;
        assert_eq!(ClaudeAccounts.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_creates_the_first_account() {
        let (mut ctx, _home) = ctx_with(signed_in()).await;
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts.len(), 1);
        assert_eq!(ClaudeAccounts.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_directory_nothing_recorded_is_adopted_rather_than_abandoned() {
        // The rescue this exists for: config.json lost, but the login and a
        // year of session history are still sitting in the directory.
        let (mut ctx, _home) = ctx_with(signed_in()).await;
        let orphan = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&orphan))
            .await
            .unwrap();

        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, vec![orphan]);
    }

    #[tokio::test]
    async fn an_account_whose_directory_vanished_is_dropped() {
        let (mut ctx, _home, id) = ready().await;
        let gone = accounts::new_id();
        ctx.config.claude_accounts.push(gone.clone());

        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, vec![id]);
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home) = ctx_with(signed_in()).await;
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        let first = ctx.config.claude_accounts.clone();
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, first);
    }

    #[tokio::test]
    async fn an_abandoned_sign_in_is_not_treated_as_success() {
        // Claude Code exits non-zero when the browser is closed. A task that
        // ignored that would report a machine that is ready and is not.
        let (mut ctx, _home) = ctx_with(
            installed()
                .with(STATUS, 1, r#"{"loggedIn":false}"#, "")
                .with("claude auth login", 1, "", ""),
        )
        .await;
        let error = ClaudeAccounts
            .apply(&mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("signing you in to Claude Code"), "{error}");
    }

    #[tokio::test]
    async fn a_signed_in_account_is_never_sent_through_a_browser() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("auth login")),
            "{:?}",
            runner.calls()
        );
    }
}

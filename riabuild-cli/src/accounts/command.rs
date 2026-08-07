//! `riabuild claude` — the account list, and the four things done to it.

use crate::accounts;
use crate::accounts::render;
use crate::accounts::status::{self, Identity};
use crate::cli::ClaudeAction;
use crate::paths::contract_tilde;
use crate::runner::RunOptions;
use crate::shims;
use crate::tasks::Ctx;
use crate::ui::Failure;
use anyhow::Result;

pub async fn run(ctx: &mut Ctx, action: Option<ClaudeAction>) -> Result<i32> {
    match action.unwrap_or(ClaudeAction::List) {
        ClaudeAction::List => list(ctx).await,
        ClaudeAction::New => new(ctx).await,
        ClaudeAction::Delete { number, yes } => delete(ctx, number, yes).await,
        ClaudeAction::Primary { number } => primary(ctx, number).await,
    }
}

async fn list(ctx: &Ctx) -> Result<i32> {
    let found = status::read_all(ctx).await;
    ctx.ui.info("");
    ctx.ui.info(&render::accounts_box(&found, ctx.ui.colour()));
    Ok(0)
}

/// Adds an account and signs it in — and only keeps it if that worked.
///
/// No Claude Code session is opened: signing in is the whole job, and the
/// developer starts a session with `claude-<n>` when they want one.
async fn new(ctx: &mut Ctx) -> Result<i32> {
    let id = accounts::new_id();
    // Registered before the directory exists, and deliberately in that order: a
    // directory created first and then refused at the cap is an unregistered
    // account nothing can number, which `claude_accounts` can only report and
    // never repair — every later `riabuild` run aborts there.
    let number = accounts::add(&mut ctx.config, id.clone())?;
    let dir = ctx.paths.claude_profile_dir(&id);
    tokio::fs::create_dir_all(&dir).await?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    shims::write_all(ctx).await?;

    ctx.ui.info(&format!(
        "Signing in account {number} — finish it in your browser."
    ));
    let claude = ctx.claude();
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };
    ctx.runner
        .run_interactive(&claude, &["auth", "login"], &options)
        .await?;

    // Asked rather than inferred from the exit code: the machine's own answer
    // is the one that decides whether an account exists.
    if !matches!(status::read(ctx, &id).await, Identity::LoggedIn(_)) {
        accounts::remove(&mut ctx.config, number)?;
        ctx.config.save(ctx.paths.as_ref()).await?;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        shims::write_all(ctx).await?;
        return Err(Failure::new(
            "adding a Claude Code account",
            "Run `riabuild claude new` again and finish the sign-in in your browser.",
        )
        .detail("the sign-in did not complete, so no account was added")
        .into());
    }

    list(ctx).await
}

async fn delete(ctx: &mut Ctx, number: usize, assume_yes: bool) -> Result<i32> {
    if ctx.config.claude_accounts.len() <= 1 {
        return Err(Failure::new(
            "deleting your only Claude Code account",
            "Add another with `riabuild claude new` first.",
        )
        .detail("the next run would only create an empty one and ask you to sign in again")
        .into());
    }

    let id = accounts::id_of(&ctx.config, number)?;
    let named = match status::read(ctx, &id).await {
        Identity::LoggedIn(email) => email,
        _ => format!("account {number}"),
    };

    if !assume_yes {
        // Checked before asking, because `ask` returns None both for "they
        // chose the default" and for "there was nobody to ask", and an
        // irreversible delete has to tell those apart.
        if !ctx.ui.interactive() {
            return Err(Failure::new(
                format!("asking whether to delete Claude Code account {number}"),
                "re-run as `riabuild claude delete <number> --yes` if you meant to remove it unattended",
            )
            .detail("riabuild has no terminal to ask on, and will not assume yes.")
            .into());
        }
        ctx.ui.info("");
        ctx.ui
            .info(&format!("  Delete account {number} — {named}?"));
        ctx.ui
            .info("  Its Claude Code sessions, history and login are removed.");
        // `Ui::confirm` defaults to yes, which is right for "shall I install
        // this" and wrong here: an empty answer must decline.
        let answer = ctx.ui.ask("  Confirm [y/N]");
        let confirmed = answer
            .is_some_and(|answer| matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"));
        if !confirmed {
            ctx.ui.info("Left alone.");
            return Ok(0);
        }
    }

    // Signing out first is load-bearing: on macOS the keychain item is named
    // for a hash of the config directory's path, so removing the directory
    // first orphans a credential nothing can ever reach again. A logout that
    // fails is not fatal — the directory still has to go.
    let dir = ctx.paths.claude_profile_dir(&id);
    let claude = ctx.claude();
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };
    let _ = ctx.runner.run(&claude, &["auth", "logout"], &options).await;
    ctx.ui.note(&format!("Signed out {named}"));

    let shown = contract_tilde(&dir, &ctx.paths.home());
    tokio::fs::remove_dir_all(&dir).await.map_err(|error| {
        Failure::new(
            format!("removing {shown}"),
            "check what still has a file open there, then run it again",
        )
        .detail(error.to_string())
    })?;
    ctx.ui.note(&format!("Removed {shown}"));

    accounts::remove(&mut ctx.config, number)?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    shims::write_all(ctx).await?;
    if number <= ctx.config.claude_accounts.len() {
        ctx.ui
            .note(&format!("Account {} is now account {number}", number + 1));
    }

    list(ctx).await
}

async fn primary(ctx: &mut Ctx, number: usize) -> Result<i32> {
    accounts::promote(&mut ctx.config, number)?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    // Rewritten in place, so shells that are already open pick this up with no
    // further action — which is the reason the environment no longer exports
    // CLAUDE_CONFIG_DIR.
    shims::write_all(ctx).await?;
    list(ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
    use crate::ui::Ui;
    use std::sync::Arc;

    const STATUS: &str = "claude auth status --json";

    fn signed_in() -> FakeRunner {
        FakeRunner::new().with(
            STATUS,
            0,
            r#"{"loggedIn":true,"email":"clubria@proton.me"}"#,
            "",
        )
    }

    /// A ctx with `count` accounts on disk, all signed in.
    async fn with_accounts(count: usize) -> (Ctx, tempfile::TempDir, Vec<String>) {
        let (mut ctx, home) = ctx_with(signed_in()).await;
        let mut ids = Vec::new();
        for _ in 0..count {
            let id = accounts::new_id();
            tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
                .await
                .unwrap();
            ids.push(id);
        }
        ctx.config.claude_accounts = ids.clone();
        (ctx, home, ids)
    }

    #[tokio::test]
    async fn deleting_the_only_account_is_refused() {
        let (mut ctx, _home, ids) = with_accounts(1).await;
        let error = delete(&mut ctx, 1, true).await.unwrap_err().to_string();
        assert!(error.contains("only Claude Code account"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
        assert!(
            tokio::fs::try_exists(ctx.paths.claude_profile_dir(&ids[0]))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn deleting_signs_out_before_removing_the_directory() {
        // The keychain item is named for a hash of the directory's path, so
        // removing the directory first orphans a credential permanently.
        let (mut ctx, _home, ids) = with_accounts(2).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();

        delete(&mut ctx, 2, true).await.unwrap();

        let logouts: Vec<String> = runner
            .calls()
            .into_iter()
            .filter(|call| call.contains("auth logout"))
            .collect();
        assert_eq!(logouts.len(), 1, "{:?}", runner.calls());
        assert!(
            !tokio::fs::try_exists(ctx.paths.claude_profile_dir(&ids[1]))
                .await
                .unwrap()
        );
        assert_eq!(ctx.config.claude_accounts, vec![ids[0].clone()]);
    }

    #[tokio::test]
    async fn deleting_with_nobody_to_ask_refuses_rather_than_assuming() {
        let (mut ctx, _home, ids) = with_accounts(2).await;
        // ctx_with builds a quiet, non-interactive Ui.
        let error = delete(&mut ctx, 2, false).await.unwrap_err().to_string();
        assert!(error.contains("asking whether to delete"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn declining_leaves_the_account_alone() {
        let (mut ctx, _home, ids) = with_accounts(2).await;
        ctx.ui = Ui::scripted(["n"]);
        delete(&mut ctx, 2, false).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn making_an_account_primary_reorders_and_rewrites_the_launchers() {
        let (mut ctx, _home, ids) = with_accounts(3).await;
        primary(&mut ctx, 3).await.unwrap();

        assert_eq!(
            ctx.config.claude_accounts,
            vec![ids[2].clone(), ids[0].clone(), ids[1].clone()]
        );
        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join("claude"))
            .await
            .unwrap();
        assert!(script.contains(ids[2].as_str()), "{script}");
    }

    #[tokio::test]
    async fn a_sign_in_that_did_not_take_adds_no_account() {
        // The browser was closed. Anything left behind would show as an
        // account permanently "(logged out)" that nobody chose to create.
        let (mut ctx, _home, ids) = with_accounts(1).await;
        ctx.runner = Arc::new(
            FakeRunner::new()
                .with(STATUS, 1, r#"{"loggedIn":false}"#, "")
                .with("claude auth login", 1, "", ""),
        );

        let error = new(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("adding a Claude Code account"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);

        let mut entries = tokio::fs::read_dir(ctx.paths.claude_dir()).await.unwrap();
        let mut count = 0;
        while let Ok(Some(_)) = entries.next_entry().await {
            count += 1;
        }
        assert_eq!(count, 1, "the abandoned directory was left behind");
    }
}

//! Who each Claude Code account is signed in as.
//!
//! Asked of Claude Code rather than read off disk: `claude auth status --json`
//! is a supported command that reports `loggedIn` and `email` for whatever
//! `CLAUDE_CONFIG_DIR` points at. The alternative — parsing `oauthAccount` out
//! of `.claude.json` — reads a key nothing promises to keep.
//!
//! The call costs about 450 ms, almost all of it the child process's own
//! startup, so the accounts are asked all at once. The runtime is
//! current-thread and that is fine: every task is blocked awaiting a subprocess,
//! so the children run concurrently even though riabuild's own work does not.

use crate::Ctx;
use riabuild_runner::{CommandRunner, RunOptions};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use tokio::task::JoinSet;

/// What Claude Code says about one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    LoggedIn(String),
    LoggedOut,
    /// riabuild could not tell, and says so rather than guessing. Rendering
    /// this as "logged out" would assert something about the account that
    /// nothing established.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// 1-based, derived from position in the list. Never stored.
    pub number: usize,
    pub id: String,
    pub identity: Identity,
}

/// Every account, asked at the same time.
pub async fn read_all(ctx: &Ctx) -> Vec<Account> {
    let claude = ctx.claude();
    let mut asking = JoinSet::new();
    // A panicked lookup's `JoinError` carries no payload, so the number and id
    // it would have reported are kept here, keyed by the task that owns them —
    // that is the only way to still list the account rather than lose it.
    let mut pending: HashMap<tokio::task::Id, (usize, String)> = HashMap::new();
    for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
        let runner = ctx.runner.clone();
        let claude = claude.clone();
        let dir = ctx.paths.claude_profile_dir(id);
        let number = index + 1;
        let id_for_task = id.clone();
        let handle = asking.spawn(async move {
            let identity = ask(runner.as_ref(), &claude, &dir).await;
            Account {
                number,
                id: id_for_task,
                identity,
            }
        });
        pending.insert(handle.id(), (number, id.clone()));
    }

    let mut found = Vec::new();
    while let Some(joined) = asking.join_next_with_id().await {
        match joined {
            Ok((_, account)) => found.push(account),
            // A panicked lookup must not take the account with it — the
            // number and id are still known even though the identity is not.
            Err(error) => {
                if let Some((number, id)) = pending.remove(&error.id()) {
                    found.push(Account {
                        number,
                        id,
                        identity: Identity::Unknown("the lookup did not finish".to_string()),
                    });
                }
            }
        }
    }
    // Tasks finish in whatever order the children do; the developer numbers
    // them in one fixed order.
    found.sort_by_key(|account| account.number);
    found
}

/// One account, by id.
pub async fn read(ctx: &Ctx, id: &str) -> Identity {
    ask(
        ctx.runner.as_ref(),
        &ctx.claude(),
        &ctx.paths.claude_profile_dir(id),
    )
    .await
}

/// The exit code is deliberately not consulted.
///
/// It says the same thing as `loggedIn` today, but an exit code is one channel
/// that a future release could also use for "could not reach the server" — and
/// reporting that as a signed-out account would be a lie riabuild told itself.
async fn ask(runner: &dyn CommandRunner, claude: &str, dir: &Path) -> Identity {
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };

    let output = match runner
        .run(claude, &["auth", "status", "--json"], &options)
        .await
    {
        Ok(output) => output,
        Err(error) => return Identity::Unknown(format!("{error:#}")),
    };

    let Ok(value) = serde_json::from_str::<Value>(&output.stdout) else {
        return Identity::Unknown("Claude Code did not answer in JSON".to_string());
    };
    match value.get("loggedIn") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => return Identity::LoggedOut,
        // Absent, or not a bool: riabuild does not know, and must not spend
        // that ignorance as a claim that the account is signed out.
        _ => return Identity::Unknown("Claude Code reported no sign-in state".to_string()),
    }
    match value.get("email").and_then(Value::as_str) {
        Some(email) => Identity::LoggedIn(email.to_string()),
        None => Identity::Unknown("Claude Code reported no email".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;
    use crate::testing::ctx_with;
    use riabuild_runner::FakeRunner;
    use std::sync::Arc;

    #[tokio::test]
    async fn accounts_are_told_apart_by_their_config_directory() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let one = accounts::new_id();
        let two = accounts::new_id();
        ctx.config.claude_accounts = vec![one.clone(), two.clone()];

        let first_dir = ctx
            .paths
            .claude_profile_dir(&one)
            .to_string_lossy()
            .into_owned();
        let second_dir = ctx
            .paths
            .claude_profile_dir(&two)
            .to_string_lossy()
            .into_owned();
        ctx.runner = Arc::new(
            FakeRunner::new()
                .with_env(
                    "claude auth status --json",
                    &[("CLAUDE_CONFIG_DIR", &first_dir)],
                    0,
                    r#"{"loggedIn":true,"authMethod":"claude.ai","email":"first@example.com"}"#,
                    "",
                )
                .with_env(
                    "claude auth status --json",
                    &[("CLAUDE_CONFIG_DIR", &second_dir)],
                    1,
                    r#"{"loggedIn":false,"authMethod":"none"}"#,
                    "",
                ),
        );

        let found = read_all(&ctx).await;
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].number, 1);
        assert_eq!(found[0].id, one);
        assert_eq!(
            found[0].identity,
            Identity::LoggedIn("first@example.com".into())
        );
        assert_eq!(found[1].number, 2);
        assert_eq!(found[1].identity, Identity::LoggedOut);
    }

    #[tokio::test]
    async fn an_answer_that_will_not_parse_is_not_reported_as_signed_out() {
        // No stub is registered, so the fake runner answers with an empty,
        // unparseable stdout rather than a spawn failure — this exercises the
        // JSON-parse branch of `ask`, not the `runner.run` error branch. The
        // distinction that matters is the same either way: riabuild not
        // knowing must never render as "(logged out)", which is a claim about
        // the account.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id()];

        let found = read_all(&ctx).await;
        assert_eq!(found.len(), 1);
        assert!(
            matches!(found[0].identity, Identity::Unknown(_)),
            "{:?}",
            found[0].identity
        );
    }

    #[tokio::test]
    async fn an_answer_with_no_email_is_not_a_sign_in() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let id = accounts::new_id();
        ctx.config.claude_accounts = vec![id.clone()];
        ctx.runner = Arc::new(FakeRunner::new().with(
            "claude auth status --json",
            0,
            r#"{"loggedIn":true}"#,
            "",
        ));

        assert!(matches!(read(&ctx, &id).await, Identity::Unknown(_)));
    }

    #[tokio::test]
    async fn an_answer_with_no_sign_in_state_is_not_a_sign_out() {
        // A future "could not reach the server" signal arrives as JSON with no
        // `loggedIn` field at all. That must not be mistaken for a sign-out —
        // this is exactly what `ask`'s doc comment says the exit code is
        // ignored to avoid, and the JSON path must hold to the same rule.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let id = accounts::new_id();
        ctx.config.claude_accounts = vec![id.clone()];
        ctx.runner = Arc::new(FakeRunner::new().with(
            "claude auth status --json",
            1,
            r#"{"error":"network unreachable"}"#,
            "",
        ));

        assert!(matches!(read(&ctx, &id).await, Identity::Unknown(_)));
    }

    #[tokio::test]
    async fn an_empty_account_list_yields_an_empty_result() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(read_all(&ctx).await.is_empty());
    }
}

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

use crate::runner::{CommandRunner, RunOptions};
use crate::tasks::Ctx;
use serde_json::Value;
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
    let mut asking = JoinSet::new();
    for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
        let runner = ctx.runner.clone();
        let claude = ctx.claude();
        let dir = ctx.paths.claude_profile_dir(id);
        let id = id.clone();
        asking.spawn(async move {
            let identity = ask(runner.as_ref(), &claude, &dir).await;
            Account {
                number: index + 1,
                id,
                identity,
            }
        });
    }

    let mut found = Vec::new();
    while let Some(joined) = asking.join_next().await {
        match joined {
            Ok(account) => found.push(account),
            // A panicked lookup must not take the whole box with it.
            Err(_) => continue,
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
    if value.get("loggedIn") != Some(&Value::Bool(true)) {
        return Identity::LoggedOut;
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
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
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
        // The distinction that matters: riabuild not knowing must never render
        // as "(logged out)", which is a claim about the account.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id()];

        let found = read_all(&ctx).await;
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
    async fn no_accounts_means_no_subprocesses() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(read_all(&ctx).await.is_empty());
    }
}

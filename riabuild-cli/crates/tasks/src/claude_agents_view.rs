//! Task 14 — Claude Code opens on the agents view.
//!
//! Clubria's answer to "what should Claude Code show when it starts" is the
//! agents view, so a developer who has never opened `/config` gets the fleet
//! rather than a single conversation.
//!
//! Like trust and onboarding, this cannot be a settings key.
//! `defaultToAgentsView` is global config in `.claude.json` — Claude Code reads
//! it as `getGlobalConfig().defaultToAgentsView === true`, and it appears in the
//! global-config key list beside `copyOnSelect` and `leftArrowOpensAgents`, not
//! in the settings schema (verified against 2.1.231). Putting the name in the
//! dashboard's settings JSON would carry a key Claude Code never reads, on every
//! laptop, silently. `--settings` cannot reach it; this task is the only route
//! to the key.
//!
//! **The key is not, however, what opens the view for a Clubria developer.**
//! Claude Code consults `defaultToAgentsView` only when every token on the raw
//! command line is a debug flag — it tests argv before its own option parsing —
//! and every launcher `shims` has ever written passes `--settings`. So this key
//! has never once decided what `~/.riabuild/bin/claude` opened on, and could
//! not: dropping `--settings` to let it through would drop org policy with it.
//! The launcher reaches the view by the `agents` positional instead, which is
//! tested *after* the options Claude Code recognises are stripped, and which
//! ignores this key entirely. See `shims`, which is where the promise in the
//! first paragraph is actually kept.
//!
//! What the key is still for is a `claude` started from outside
//! `~/.riabuild/bin` — a developer's own install, an editor integration, a
//! script. Those get no launcher and no `--settings`, which is exactly the
//! shape the key is read in, so the task stays.
//!
//! **A default, not a policy.** riabuild writes the key only when the account
//! has no answer of its own, and never overwrites one. Toggling "Open agents
//! view by default" in `/config` persists the developer's answer — `false`
//! included — so a task that asserted `true` on every run would undo that
//! choice every time the developer ran `riabuild`, with nothing on screen to say
//! why their preference kept coming back. That is the difference between this
//! and its two neighbours: trust and onboarding are facts a developer would
//! never want undone, and this is a view they might.
//!
//! Say the rest of that out loud, because the launcher changed it: through
//! `~/.riabuild/bin/claude` the developer's `/config` answer no longer decides
//! anything, since the positional route does not consult the key. Turning the
//! view off there and getting it anyway is a real surprise, and the honest
//! remedy is the one Claude Code documents — `CLAUDE_CODE_DISABLE_AGENT_VIEW`,
//! which the launcher checks precisely so that a developer who wants out has a
//! way out. What is preserved below is narrower than it was and still worth
//! preserving: riabuild does not *overwrite* an answer a developer gave.
//!
//! Which makes "the account has an answer" the end state `check()` asks about,
//! and it is a real one — a fresh account has no key at all, and `/config`
//! writes the boolean only when somebody changes it.

use super::claude_config::{self, Stored};
use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_ui::Failure;
use serde_json::Value;

pub struct ClaudeAgentsView;

const KEY: &str = "defaultToAgentsView";

#[async_trait]
impl Task for ClaudeAgentsView {
    fn id(&self) -> TaskId {
        "claude_agents_view"
    }

    fn title(&self) -> &str {
        "Claude Code agents view"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // The accounts task supplies the config directories to write into, one
        // per account. Deliberately *not* `project`, for the same reason
        // `claude_onboarding` is not: nothing here needs a checkout.
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
                    // Presence, not truth. A developer who turned the view off
                    // has answered the question, and re-asking it every run is
                    // how riabuild would keep overruling them.
                    if !root.contains_key(KEY) {
                        return Ok(Status::needs(format!(
                            "account {number} would not open on the agents view"
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
                "setting Claude Code to open on the agents view",
                "Run `riabuild` again — a Claude Code account has to exist first.",
            )
            .into());
        }

        for id in ctx.config.claude_accounts.clone() {
            prefer_one(ctx, &id).await?;
        }
        Ok(())
    }
}

/// Gives one account the team's default, if it does not already have an answer.
///
/// `pub(crate)` alongside the trust and onboarding equivalents, so `riabuild
/// claude new` can settle the account it just created rather than leaving its
/// first session to open somewhere else.
pub(crate) async fn prefer_one(ctx: &mut Ctx, id: &str) -> Result<()> {
    claude_config::edit(ctx, id, |root| {
        // `entry` rather than `insert`: the whole distinction this task rests on
        // is between an account with no answer and one whose answer is `false`,
        // and an unconditional insert would erase it.
        root.entry(KEY).or_insert(Value::Bool(true));
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

    async fn stored(ctx: &Ctx, id: &str) -> Value {
        let text = tokio::fs::read_to_string(claude_config::config_file(ctx, id))
            .await
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn a_machine_without_an_account_is_not_claimed_to_be_done() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            ClaudeAgentsView.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn an_account_that_was_never_asked_is_detected() {
        let (ctx, _home, ids) = ready().await;
        write_file(
            &claude_config::config_file(&ctx, &ids[0]),
            r#"{"hasCompletedOnboarding":true}"#,
        )
        .await;

        let status = ClaudeAgentsView.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("agents view"), "{status:?}");
    }

    #[tokio::test]
    async fn one_settled_account_is_not_enough() {
        // claude-2 would open on a single conversation — the same drift, one
        // account over, and invisible to a check that stopped at the primary.
        let (mut ctx, _home, ids) = ready().await;
        ClaudeAgentsView.apply(&mut ctx).await.unwrap();

        write_file(
            &claude_config::config_file(&ctx, &ids[1]),
            r#"{"numStartups":1}"#,
        )
        .await;
        let status = ClaudeAgentsView.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("agents view"), "{status:?}");
        assert!(format!("{status:?}").contains('2'), "{status:?}");
    }

    #[tokio::test]
    async fn applying_gives_every_account_the_default() {
        let (mut ctx, _home, ids) = ready().await;
        ClaudeAgentsView.apply(&mut ctx).await.unwrap();

        for id in &ids {
            assert_eq!(stored(&ctx, id).await[KEY], Value::Bool(true), "{id}");
        }
        assert_eq!(
            ClaudeAgentsView.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    #[tokio::test]
    async fn a_developer_who_turned_it_off_stays_turned_off() {
        // The property the whole task is shaped around. riabuild offers the
        // team's default once; it does not re-impose it on someone who went to
        // `/config` and said no.
        let (mut ctx, _home, ids) = ready().await;
        write_file(
            &claude_config::config_file(&ctx, &ids[0]),
            r#"{"defaultToAgentsView":false}"#,
        )
        .await;

        assert!(
            matches!(
                ClaudeAgentsView.check(&ctx).await.unwrap(),
                Status::Needs(_)
            ),
            "the second account still has no answer"
        );
        ClaudeAgentsView.apply(&mut ctx).await.unwrap();

        assert_eq!(stored(&ctx, &ids[0]).await[KEY], Value::Bool(false));
        assert_eq!(stored(&ctx, &ids[1]).await[KEY], Value::Bool(true));
        assert_eq!(
            ClaudeAgentsView.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    #[tokio::test]
    async fn applying_keeps_the_sign_in_and_everything_else() {
        let (mut ctx, _home, ids) = ready().await;
        write_file(
            &claude_config::config_file(&ctx, &ids[0]),
            r#"{"numStartups":7,"oauthAccount":{"emailAddress":"ada@clubria.com"},"projects":{"/other":{"hasTrustDialogAccepted":true}}}"#,
        )
        .await;

        ClaudeAgentsView.apply(&mut ctx).await.unwrap();

        let root = stored(&ctx, &ids[0]).await;
        assert_eq!(root["oauthAccount"]["emailAddress"], "ada@clubria.com");
        assert_eq!(root["numStartups"], 7);
        assert_eq!(root["projects"]["/other"]["hasTrustDialogAccepted"], true);
        assert_eq!(root[KEY], Value::Bool(true));
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home, ids) = ready().await;
        ClaudeAgentsView.apply(&mut ctx).await.unwrap();
        let first = tokio::fs::read_to_string(claude_config::config_file(&ctx, &ids[0]))
            .await
            .unwrap();
        ClaudeAgentsView.apply(&mut ctx).await.unwrap();
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
            ClaudeAgentsView.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
        ClaudeAgentsView.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ClaudeAgentsView.check(&ctx).await.unwrap(),
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

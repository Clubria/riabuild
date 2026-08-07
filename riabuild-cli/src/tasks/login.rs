//! Task 1 — sign this machine in to riabuild.
//!
//! The session token lives in the Keychain, never in `~/.riabuild`. Everything
//! downstream that talks to riabuild-web depends on this having run.

use super::{Ctx, Status, Task, TaskId};
use crate::api::auth;
use crate::api::org;
use crate::config::now_millis;
use crate::ui::Failure;
use anyhow::Result;
use async_trait::async_trait;

/// Re-authenticate before the token is close enough to expiry that a developer
/// could be interrupted mid-task by a browser prompt.
const REFRESH_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;

pub struct Login;

#[async_trait]
impl Task for Login {
    fn id(&self) -> TaskId {
        "login"
    }

    fn title(&self) -> &str {
        "riabuild sign-in"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        // `ctx.member` is populated at startup by asking the server who we are,
        // so this checks a live session rather than the presence of a file.
        let Some(member) = &ctx.member else {
            return Ok(Status::needs("this machine is not signed in"));
        };

        if member.status != "active" {
            // Not a check failure: signing in again would succeed and change
            // nothing. Stop and say so.
            return Err(Failure::new(
                "checking your riabuild account",
                "Ask your team lead to reactivate your account.",
            )
            .detail(format!("@{} is suspended", member.github_login))
            .into());
        }

        if let Some(expires_at) = ctx.config.session_expires_at
            && expires_at.saturating_sub(now_millis()) < REFRESH_WINDOW_MS
        {
            return Ok(Status::needs("this machine's session expires soon"));
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let web_url = ctx.web_url.clone();
        let version = ctx.cli_version.clone();
        let label = auth::device_label(ctx.runner.as_ref()).await;
        ctx.ui.heading("Signing this machine in to riabuild");
        // The session id `auth::login` also returns is only ever needed to
        // revoke a *server's* session (`remote::session::ensure` keeps it for
        // that); a laptop's own sign-in has no analogous "forget" command, so
        // there is nothing here to keep it for.
        let (token, member, _session_id) = auth::login(
            &ctx.api,
            ctx.runner.as_ref(),
            &ctx.ui,
            &web_url,
            &version,
            &label,
        )
        .await?;

        ctx.keychain.set(&token).await?;
        ctx.api.set_token(Some(token));

        // The token is now live, so everything the server knows is reachable.
        ctx.org = Some(org::fetch_config(&ctx.api).await?);
        ctx.config.session_expires_at = Some(now_millis() + SESSION_TTL_MS);
        ctx.config.save(ctx.paths.as_ref()).await?;
        ctx.ui
            .note(&format!("signed in as {}", member.display_name()));
        ctx.member = Some(member);
        Ok(())
    }
}

/// Mirrors `SESSION_TTL_MS` in riabuild-web. Only used to decide when to refresh
/// early; the server remains the authority on whether a session is still good.
const SESSION_TTL_MS: u64 = 90 * 24 * 60 * 60 * 1000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Member;
    use crate::testing::test_ctx;

    fn member(status: &str) -> Member {
        Member {
            github_login: "ada".into(),
            member_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@clubria.dev".into(),
            role: "developer".into(),
            status: status.into(),
        }
    }

    #[tokio::test]
    async fn an_unsigned_machine_needs_signing_in() {
        let (ctx, _home) = test_ctx().await;
        assert!(matches!(Login.check(&ctx).await.unwrap(), Status::Needs(_)));
    }

    #[tokio::test]
    async fn a_live_session_is_satisfied() {
        let (mut ctx, _home) = test_ctx().await;
        ctx.member = Some(member("active"));
        ctx.config.session_expires_at = Some(now_millis() + 60 * 24 * 3600 * 1000);
        assert_eq!(Login.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_session_expiring_within_a_week_is_refreshed_early() {
        let (mut ctx, _home) = test_ctx().await;
        ctx.member = Some(member("active"));
        ctx.config.session_expires_at = Some(now_millis() + 3 * 24 * 3600 * 1000);
        assert!(matches!(Login.check(&ctx).await.unwrap(), Status::Needs(_)));
    }

    #[tokio::test]
    async fn a_suspended_account_stops_rather_than_looping_through_the_browser() {
        let (mut ctx, _home) = test_ctx().await;
        ctx.member = Some(member("suspended"));
        let error = Login.check(&ctx).await.unwrap_err().to_string();
        assert!(error.contains("reactivate"), "{error}");
    }
}

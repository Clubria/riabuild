//! Task 8 — cache the org's Claude Code settings.
//!
//! The file is handed to `claude --settings` by every account launcher —
//! `claude` and `claude-1` … `claude-N` — which layers it over that account's
//! own settings at launch. One cached file serves all of them: org policy is
//! org-wide by definition. Nothing is merged into anyone's `settings.json`.
//!
//! A recurring deep-merge cannot express removal, cannot tell org keys from
//! developer keys after the first run, and silently clobbers edits. Layering at
//! launch means org policy is always current, removals take effect, developer
//! edits survive, and there is no merge code to maintain.

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_api::org;

pub struct OrgSettings;

#[async_trait]
impl Task for OrgSettings {
    fn id(&self) -> TaskId {
        "org_settings"
    }

    fn title(&self) -> &str {
        "Team Claude Code settings"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &["login"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let file = ctx.paths.org_settings_file();
        if !tokio::fs::try_exists(&file).await.unwrap_or(false) {
            return Ok(Status::needs("the team settings have not been fetched yet"));
        }

        let Ok(text) = tokio::fs::read_to_string(&file).await else {
            return Ok(Status::needs("the cached team settings cannot be read"));
        };
        if serde_json::from_str::<serde_json::Value>(&text).is_err() {
            // `claude --settings` would fail on this at launch, so it counts as
            // a broken machine even though the file is present.
            return Ok(Status::needs("the cached team settings are not valid JSON"));
        }

        // Nothing to compare against until this machine is signed in, and the
        // question has to be *asked* before it can be answered — an
        // unauthenticated request gets a 401, which `?` turns into a hard error
        // and takes the whole run down with it.
        //
        // That is the difference between `riabuild --check` telling a developer
        // with an expired session "you are not signed in" and it refusing to
        // report anything at all, which is the moment that command matters
        // most. `login` runs first and this re-checks once it has. Same guard
        // `project` and `env_local` already use.
        if ctx.member.is_none() {
            return Ok(Status::needs("waiting for sign-in"));
        }

        // The authoritative comparison: what the server says it published.
        let remote = org::fetch_claude_settings(&ctx.api).await?;
        match ctx.config.org_settings_updated_at {
            Some(cached) if cached == remote.updated_at => Ok(Status::Satisfied),
            _ => Ok(Status::needs("the team settings changed")),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let remote = org::fetch_claude_settings(&ctx.api).await?;
        let file = ctx.paths.org_settings_file();
        if let Some(parent) = file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&file, serde_json::to_string_pretty(&remote.settings)?).await?;

        let updated_at = remote.updated_at;
        ctx.update_config(|config| config.org_settings_updated_at = Some(updated_at))
            .await?;
        Ok(())
    }
}

/// Brings this machine's copy of the team settings up to date, outside the task
/// engine.
///
/// `pub(crate)` alongside `claude_trust::trust_one` and its two neighbours, and
/// for the same reason: `riabuild claude new` creates an account and hands it
/// straight to the developer, so the next `riabuild` run is too late. The
/// difference is that this one is not per-account. One file serves every
/// launcher, which is exactly why nothing outside the engine was ensuring it —
/// and why a machine that had never completed a provisioning run gave a brand
/// new account no org policy at all, silently, since the launcher drops
/// `--settings` rather than naming a file that is not there.
///
/// The whole task, not a file test: `check()` compares what is cached against
/// what the server says it published, so this repairs a stale copy as well as a
/// missing one.
pub(crate) async fn ensure_cached(ctx: &mut Ctx) -> Result<()> {
    if OrgSettings.check(ctx).await? == Status::Satisfied {
        return Ok(());
    }

    // `check()` answers the signed-out machine without touching the network,
    // and `apply()` would then spend a request learning what it already knows:
    // there is no session to fetch with. `riabuild claude` is documented to
    // work with no session and no network, so this returns rather than hanging
    // a browser sign-in behind an HTTP timeout.
    if ctx.member.is_none() {
        return Err(anyhow::anyhow!("this machine is not signed in to riabuild"));
    }

    OrgSettings.apply(ctx).await?;
    // The invariant, kept where the engine cannot keep it: apply is always
    // followed by a re-run of check.
    match OrgSettings.check(ctx).await? {
        Status::Satisfied => Ok(()),
        Status::Needs(still) => Err(anyhow::anyhow!("{}", still.describe())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;

    #[tokio::test]
    async fn a_missing_cache_needs_fetching() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not been fetched"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_signed_out_machine_is_reported_not_thrown() {
        // Regression: with a valid cache on disk and no session, `check()` used
        // to ask the server anyway, take a 401, and turn `riabuild --check`
        // into a hard failure — on exactly the machine whose problem is that
        // the session expired. There is no runner output here because a
        // signed-out check must not reach the network at all.
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_file(&ctx.paths.org_settings_file(), r#"{"env":{}}"#).await;
        assert!(ctx.member.is_none());

        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("waiting for sign-in"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn ensuring_on_a_signed_out_machine_reports_that_rather_than_the_network() {
        // `riabuild claude new` calls this on a machine that may have no
        // session, no network, and nothing provisioned. `check` answers that
        // case without a request, so `apply` would spend a round trip — and a
        // reqwest timeout — learning there is no token to send. The wording is
        // the observable difference: an error that reached the network names
        // the host or the status, and this one names the sign-in.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(ctx.member.is_none());

        let error = ensure_cached(&mut ctx)
            .await
            .expect_err("nothing can be fetched without a session");
        assert!(error.to_string().contains("not signed in"), "{error}");
    }

    #[tokio::test]
    async fn a_corrupt_cache_is_detected_without_asking_the_server() {
        // No network here on purpose: invalid JSON on disk is enough to know the
        // machine is wrong.
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_file(&ctx.paths.org_settings_file(), "{ not json").await;
        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not valid JSON"),
            "{status:?}"
        );
    }
}

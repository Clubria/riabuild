//! Task 8 — cache the org's Claude Code settings.
//!
//! The file is handed to `claude --settings` by the `c` launcher, which layers
//! it over the developer's own profile settings at launch. Nothing is merged
//! into anyone's `settings.json`.
//!
//! A recurring deep-merge cannot express removal, cannot tell org keys from
//! developer keys after the first run, and silently clobbers edits. Layering at
//! launch means org policy is always current, removals take effect, developer
//! edits survive, and there is no merge code to maintain.

use super::{Ctx, Status, Task, TaskId};
use crate::api::org;
use anyhow::Result;
use async_trait::async_trait;

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

        ctx.config.org_settings_updated_at = Some(remote.updated_at);
        ctx.config.save(ctx.paths.as_ref()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};

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

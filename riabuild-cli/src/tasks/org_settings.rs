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
        if !file.exists() {
            return Ok(Status::needs("the team settings have not been fetched yet"));
        }

        let Ok(text) = std::fs::read_to_string(&file) else {
            return Ok(Status::needs("the cached team settings cannot be read"));
        };
        if serde_json::from_str::<serde_json::Value>(&text).is_err() {
            // `claude --settings` would fail on this at launch, so it counts as
            // a broken machine even though the file is present.
            return Ok(Status::needs("the cached team settings are not valid JSON"));
        }

        // The authoritative comparison: what the server says it published.
        let remote = org::fetch_claude_settings(&ctx.api)?;
        match ctx.config.org_settings_updated_at {
            Some(cached) if cached == remote.updated_at => Ok(Status::Satisfied),
            _ => Ok(Status::needs("the team settings changed")),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let remote = org::fetch_claude_settings(&ctx.api)?;
        let file = ctx.paths.org_settings_file();
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, serde_json::to_string_pretty(&remote.settings)?)?;

        ctx.config.org_settings_updated_at = Some(remote.updated_at);
        ctx.config.save(ctx.paths.as_ref())?;
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
        let (ctx, _home) = ctx_with(FakeRunner::new());
        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not been fetched"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_corrupt_cache_is_detected_without_asking_the_server() {
        // No network here on purpose: invalid JSON on disk is enough to know the
        // machine is wrong.
        let (ctx, _home) = ctx_with(FakeRunner::new());
        write_file(&ctx.paths.org_settings_file(), "{ not json");
        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not valid JSON"),
            "{status:?}"
        );
    }
}

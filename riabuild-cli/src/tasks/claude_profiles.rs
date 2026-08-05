//! Task 7 — a Claude Code profile of the developer's own.
//!
//! riabuild creates the profile directory and never writes into the developer's
//! `settings.json`. Org policy is layered at launch by the `c` shim instead —
//! see `org_settings` for why a recurring deep-merge is the wrong shape.

use super::{Ctx, Status, Task, TaskId};
use crate::runner::RunOptions;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use async_trait::async_trait;
use rand::RngCore;
use std::path::Path;

const MIN_VERSION: &str = "2.0.0";

pub struct ClaudeProfiles;

/// A v4 UUID for the profile directory name.
pub fn new_profile_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

pub fn looks_like_profile_id(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(expected, part)| part.len() == *expected)
        && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

async fn existing_profile(claude_dir: &Path) -> Option<String> {
    // tokio::fs::read_dir is a cursor rather than an iterator, so this cannot
    // stay a combinator chain: each step needs its own await.
    let mut entries = tokio::fs::read_dir(claude_dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let is_dir = entry
            .file_type()
            .await
            .map(|kind| kind.is_dir())
            .unwrap_or(false);
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if looks_like_profile_id(&name) {
            return Some(name);
        }
    }
    None
}

#[async_trait]
impl Task for ClaudeProfiles {
    fn id(&self) -> TaskId {
        "claude_profiles"
    }

    fn title(&self) -> &str {
        "Claude Code profile"
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
        if ctx.runner.which("claude").is_none() {
            return Ok(Status::needs("Claude Code is not installed"));
        }
        let reported = ctx
            .runner
            .run("claude", &["--version"], &RunOptions::default())
            .await?;
        if !version::at_least(reported.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!(
                "Claude Code is older than {MIN_VERSION}"
            )));
        }

        let claude_dir = ctx.paths.claude_dir();
        let Some(profile) = existing_profile(&claude_dir).await else {
            return Ok(Status::needs("no Claude Code profile yet"));
        };
        // A recorded profile that has since been deleted is drift a file-exists
        // check on the parent directory would miss.
        if !claude_dir.join(&profile).is_dir() {
            return Ok(Status::needs("the Claude Code profile is missing"));
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if ctx.runner.which("claude").is_none() {
            install_claude(ctx).await?;
        }

        let claude_dir = ctx.paths.claude_dir();
        tokio::fs::create_dir_all(&claude_dir).await?;

        // Not `unwrap_or_else`: the lookup awaits, and a closure cannot.
        let profile = match existing_profile(&claude_dir).await {
            Some(found) => found,
            None => new_profile_id(),
        };
        tokio::fs::create_dir_all(claude_dir.join(&profile)).await?;

        ctx.config.claude_profile = Some(profile);
        ctx.config.save(ctx.paths.as_ref()).await?;
        Ok(())
    }
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
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
    use std::sync::Arc;

    #[test]
    fn generates_well_formed_profile_ids() {
        let id = new_profile_id();
        assert!(looks_like_profile_id(&id), "{id}");
        assert_ne!(id, new_profile_id());
        // Version 4, variant 1 — the bits a UUID library would set.
        assert_eq!(id.chars().nth(14), Some('4'));
        assert!(matches!(id.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
    }

    #[test]
    fn rejects_directories_that_are_not_profiles() {
        assert!(!looks_like_profile_id("settings"));
        assert!(!looks_like_profile_id("not-a-uuid"));
        assert!(!looks_like_profile_id(""));
    }

    #[tokio::test]
    async fn a_missing_claude_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            ClaudeProfiles.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn an_installed_claude_without_a_profile_is_detected() {
        let (ctx, _home) =
            ctx_with(FakeRunner::new().with("claude --version", 0, "2.1.221 (Claude Code)", ""))
                .await;
        let status = ClaudeProfiles.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("profile"), "{status:?}");
    }

    #[tokio::test]
    async fn a_deleted_profile_directory_is_noticed() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        tokio::fs::create_dir_all(ctx.paths.claude_dir())
            .await
            .unwrap();
        ctx.config.claude_profile = Some(new_profile_id());
        ctx.runner =
            Arc::new(FakeRunner::new().with("claude --version", 0, "2.1.221 (Claude Code)", ""));
        // The directory recorded in config.json is gone from disk.
        assert!(matches!(
            ClaudeProfiles.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn a_profile_on_disk_is_satisfied() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let profile = new_profile_id();
        tokio::fs::create_dir_all(ctx.paths.claude_dir().join(&profile))
            .await
            .unwrap();
        ctx.config.claude_profile = Some(profile);
        ctx.runner =
            Arc::new(FakeRunner::new().with("claude --version", 0, "2.1.221 (Claude Code)", ""));
        assert_eq!(ClaudeProfiles.check(&ctx).await.unwrap(), Status::Satisfied);
    }
}

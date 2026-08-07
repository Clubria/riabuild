//! Task 11 — the Claude Code status line.
//!
//! The team's Claude Code settings name a command,
//! `node ~/.riabuild/claude-statusline.js`. This task is what puts that file
//! there.
//!
//! The script is compiled into the binary rather than served alongside the
//! settings that reference it. A status line is code Claude Code executes on
//! every render, and riabuild ships code through signed Homebrew releases —
//! the server sends the pointer, never the program.
//!
//! `node` resolves because `shell::path_with_riabuild` puts riabuild's own Node
//! and `~/.riabuild/bin` on `PATH` together: the account launchers are reachable
//! exactly when the interpreter they need is.

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;

/// The script itself, compiled in. A `brew upgrade` is the only thing that can
/// change what runs on a developer's machine.
pub const SCRIPT: &str = include_str!("../../assets/claude-statusline.js");

pub struct ClaudeStatusline;

#[async_trait]
impl Task for ClaudeStatusline {
    fn id(&self) -> TaskId {
        "claude_statusline"
    }

    fn title(&self) -> &str {
        "Claude Code status line"
    }

    /// Stays at 1. `check()` compares the installed file against the embedded
    /// copy byte for byte, so a script that changes in a release is drift the
    /// check already sees — there is nothing left for a version bump to catch.
    fn version(&self) -> u32 {
        1
    }

    /// Nothing. Writing a file needs no login, no Node, and no Claude Code; a
    /// dependency riabuild does not actually have would only cause re-runs.
    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let file = ctx.paths.claude_statusline_file();
        if !tokio::fs::try_exists(&file).await.unwrap_or(false) {
            return Ok(Status::needs("the status line is not installed yet"));
        }
        let Ok(installed) = tokio::fs::read_to_string(&file).await else {
            return Ok(Status::needs("the status line script cannot be read"));
        };
        // Hand-edited, truncated, or left by an older riabuild — all of them
        // mean this machine is not running the script this release ships.
        if installed != SCRIPT {
            return Ok(Status::needs(
                "the status line script is not the one this riabuild ships",
            ));
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let file = ctx.paths.claude_statusline_file();
        if let Some(parent) = file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&file, SCRIPT).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};

    #[tokio::test]
    async fn a_missing_script_needs_installing() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = ClaudeStatusline.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_script_from_an_older_release_is_replaced() {
        // The failure this exists for: an upgrade ships a new script, and a
        // file-exists check would call the old one satisfied forever.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_file(
            &ctx.paths.claude_statusline_file(),
            "// an older riabuild wrote this\n",
        )
        .await;

        let status = ClaudeStatusline.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the one this riabuild ships"),
            "{status:?}"
        );

        ClaudeStatusline.apply(&mut ctx).await.unwrap();
        assert_eq!(
            ClaudeStatusline.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ClaudeStatusline.apply(&mut ctx).await.unwrap();
        ClaudeStatusline.apply(&mut ctx).await.unwrap();

        let written = tokio::fs::read_to_string(ctx.paths.claude_statusline_file())
            .await
            .unwrap();
        assert_eq!(written, SCRIPT);
        assert_eq!(
            ClaudeStatusline.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    /// The script and the command the org settings name have to agree. They are
    /// edited in different repositories, so nothing but a test connects them.
    #[tokio::test]
    async fn the_installed_path_matches_the_command_the_org_settings_name() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let file = ctx.paths.claude_statusline_file();
        assert!(
            file.ends_with("claude-statusline.js"),
            "riabuild-web points `node ~/.riabuild/claude-statusline.js` at this file: {}",
            file.display()
        );
        assert!(file.starts_with(ctx.paths.root()), "{}", file.display());
    }
}

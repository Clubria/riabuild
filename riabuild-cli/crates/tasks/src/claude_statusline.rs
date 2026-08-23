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
//!
//! The script goes to `tools_root()` and never to `root()`. That distinction is
//! invisible on a laptop, where the two are one directory, and is the whole of
//! whether remote mode has a status line at all — see
//! `Paths::claude_statusline_file`.

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_paths::contract_tilde;
use riabuild_ui::Failure;

/// The script itself, compiled in. A `brew upgrade` is the only thing that can
/// change what runs on a developer's machine.
pub const SCRIPT: &str = include_str!("../assets/claude-statusline.js");

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
        if let Some(stale) = superseded_copy(ctx)
            && tokio::fs::try_exists(&stale).await.unwrap_or(false)
        {
            return Ok(Status::needs(
                "an older riabuild left a status line script in this developer's namespace",
            ));
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let file = ctx.paths.claude_statusline_file();

        // Written beside the target and renamed over it, never truncated in
        // place. On a server this path is shared by everyone with an account on
        // the box, and Claude Code re-runs the script on **every render** — so a
        // plain write hands whichever colleague is mid-render half a file. The
        // temporary is named for this process because two developers can be
        // running `riabuild` at the same moment, and a shared temporary name
        // would let one of them rename the other's half-written copy into place:
        // the very failure the rename exists to prevent, reintroduced one level
        // down. `config::write_atomic` is riabuild's one write with those
        // properties, so this asks for it rather than restating it.
        riabuild_paths::config::write_atomic(&file, SCRIPT.as_bytes()).await?;
        readable_by_every_developer(&file).await?;

        // The copy every server provisioned before this landed still has in its
        // namespace. Inert — nothing reads it — but it is byte-identical to the
        // live script, so anyone debugging a status line finds it, reads it,
        // and concludes the installed script is correct. Removing it is the
        // second half of moving the file, not tidying.
        //
        // The failure is reported rather than swallowed, and that is the whole
        // reason this is not a bare `let _ =`. `check()` looks for this file, so
        // a removal that quietly did nothing comes back as "an older riabuild
        // left a script in your namespace" on every run from here on, with
        // nothing naming the reason. Saying `remove` failed, and on what, is the
        // difference between one actionable error and a permanent unexplained
        // one.
        if let Some(stale) = superseded_copy(ctx)
            && tokio::fs::try_exists(&stale).await.unwrap_or(false)
        {
            tokio::fs::remove_file(&stale).await.map_err(|error| {
                Failure::new(
                    format!(
                        "removing the status line script an older riabuild left at {}",
                        contract_tilde(&stale, &ctx.paths.home())
                    ),
                    "delete that file and run `riabuild` again — the status line itself is \
                     already installed",
                )
                .detail(error.to_string())
            })?;
        }
        Ok(())
    }
}

/// Where this developer's own riabuild used to put the script on a server, when
/// that is somewhere other than where it puts it now.
///
/// `None` on a laptop, where the two roots are one directory and there is
/// nothing superseded to find — the guard that keeps this from proposing the
/// live script for deletion.
fn superseded_copy(ctx: &Ctx) -> Option<std::path::PathBuf> {
    let namespaced = ctx.paths.root().join("claude-statusline.js");
    (namespaced != ctx.paths.claude_statusline_file()).then_some(namespaced)
}

/// Puts the script back at `0644` after the atomic write, which lands `0600`.
///
/// **Not tidiness, and not a mode this file could inherit instead.**
/// `write_atomic` is private from the instant the temporary exists precisely so
/// that a file holding a secret is never briefly readable — the right default,
/// and the wrong answer here. This script holds nothing secret and lives under
/// `tools_root()`, which is one directory shared by *every* developer with an
/// account on a server; a `0600` copy is one only whoever ran `riabuild` last
/// can read, and Claude Code renders a status line whose command fails as **no
/// status line at all**. So every co-tenant would silently lose theirs, with
/// `check()` reporting satisfied — the same invisible absence, and the same
/// wrong reasoning about which root this file belongs to, that this module's
/// own header records having already shipped once.
async fn readable_by_every_developer(file: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).await?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;

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

    /// The same question asked on a **server**, which is the machine the laptop
    /// shape above cannot speak for: there `root()` is a per-developer namespace
    /// and only `tools_root()` is still `~/.riabuild`, so a path built on the
    /// wrong one of the two satisfies every assertion above and lands somewhere
    /// the org settings have never named.
    ///
    /// That is not hypothetical. It is what shipped: the script went to
    /// `~/.riabuild-remote/<member-id>/claude-statusline.js`, Claude Code ran
    /// `node` on `~/.riabuild/claude-statusline.js`, and a status line whose
    /// command fails renders as no status line at all — so remote mode never had
    /// one, silently, with the task reporting satisfied.
    #[tokio::test]
    async fn the_script_lands_where_the_org_settings_name_it_on_a_server() {
        let (ctx, home) = crate::testing::ctx_on_a_server(FakeRunner::new()).await;
        assert_ne!(
            ctx.paths.root(),
            ctx.paths.tools_root(),
            "this test is only meaningful where the two differ"
        );

        assert_eq!(
            ctx.paths.claude_statusline_file(),
            home.path().join(".riabuild/claude-statusline.js"),
            "riabuild-web names `node ~/.riabuild/claude-statusline.js`, and `~` on a \
             server is the shared account's home — not this developer's namespace"
        );
    }

    /// The script and the command the org settings name have to agree. They are
    /// edited in different repositories, so nothing but a test connects them.
    ///
    /// Asserted as the **whole path** the command expands to, not as a suffix
    /// and a prefix. The loose version this replaces — ends with the filename,
    /// starts with `root()` — is exactly what let the server bug through: both
    /// halves stayed true when `root()` moved out from under the command, so the
    /// test that existed to connect the two repositories went on passing while
    /// they disagreed.
    #[tokio::test]
    async fn the_installed_path_matches_the_command_the_org_settings_name() {
        let (ctx, home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(
            ctx.paths.claude_statusline_file(),
            home.path().join(".riabuild/claude-statusline.js"),
            "riabuild-web names `node ~/.riabuild/claude-statusline.js` — change one and \
             this is the only thing that connects them"
        );
    }

    /// The copy every server provisioned before the move still has, sitting in
    /// the developer's namespace where the org settings never pointed.
    ///
    /// It is byte-identical to the live script, which is what makes it worth
    /// removing rather than ignoring: it is the first file a developer debugging
    /// a missing status line will find, and reading it proves nothing.
    #[tokio::test]
    async fn a_copy_left_in_the_namespace_by_an_older_riabuild_is_reported_and_removed() {
        let (mut ctx, _home) = crate::testing::ctx_on_a_server(FakeRunner::new()).await;
        let stale = ctx.paths.root().join("claude-statusline.js");
        write_file(&stale, SCRIPT).await;
        // The live path is correct, so the byte comparison alone says satisfied:
        // without a check that looks in the namespace, `apply` would never run
        // and the stale copy would outlive every future release.
        write_file(&ctx.paths.claude_statusline_file(), SCRIPT).await;

        let status = ClaudeStatusline.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("namespace"), "{status:?}");

        ClaudeStatusline.apply(&mut ctx).await.unwrap();

        assert!(!tokio::fs::try_exists(&stale).await.unwrap());
        assert_eq!(
            ClaudeStatusline.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    /// A laptop has one root, so there is no superseded copy to find — and the
    /// path the namespace check would name is the live script itself. Without
    /// the guard, every laptop would report drift it could only fix by deleting
    /// the file it had just installed.
    #[tokio::test]
    async fn a_laptop_has_nothing_superseded_to_remove() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ClaudeStatusline.apply(&mut ctx).await.unwrap();

        assert_eq!(superseded_copy(&ctx), None);
        assert_eq!(
            ClaudeStatusline.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
        assert!(
            tokio::fs::try_exists(ctx.paths.claude_statusline_file())
                .await
                .unwrap()
        );
    }

    /// Nothing is left beside the script for Claude Code to trip over. The
    /// staged copy is named for this process, so a crash between the write and
    /// the rename would otherwise leave a permanent `.tmp` in the shared tree.
    #[tokio::test]
    async fn applying_leaves_no_temporary_behind() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ClaudeStatusline.apply(&mut ctx).await.unwrap();

        let mut entries = tokio::fs::read_dir(ctx.paths.tools_root()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".tmp"), "{name}");
        }
    }

    /// The script has to be readable by somebody other than whoever ran
    /// `riabuild` last.
    ///
    /// It lives under `tools_root()`, which every developer with an account on
    /// a server shares, and Claude Code runs it on every render. riabuild's
    /// atomic write lands `0600` — correct for a secret, and here it would
    /// silently take the status line away from every co-tenant while `check()`
    /// went on reporting the machine satisfied, because a status line whose
    /// command fails renders as no status line at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_installed_script_is_readable_by_a_co_tenant() {
        use std::os::unix::fs::PermissionsExt;

        let (mut ctx, _home) = crate::testing::ctx_on_a_server(FakeRunner::new()).await;
        ClaudeStatusline.apply(&mut ctx).await.unwrap();

        let file = ctx.paths.claude_statusline_file();
        let mode = tokio::fs::metadata(&file)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644, "{} is {mode:o}", file.display());
    }

    /// The status line and the prompt answer the same question — *which
    /// environment is this?* — from a JavaScript asset and a Rust constant that
    /// nothing but this test connects. Renaming one and not the other leaves a
    /// developer with two markers for one environment, and every other test
    /// here still passes.
    #[test]
    fn the_status_line_carries_the_same_label_as_the_prompt() {
        assert!(
            SCRIPT.contains(crate::shell::PROMPT_LABEL),
            "the status line has to say `{}`, like the prompt does",
            crate::shell::PROMPT_LABEL
        );
    }
}

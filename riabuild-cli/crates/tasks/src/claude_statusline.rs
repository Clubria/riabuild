//! Task 11 — the Claude Code status line.
//!
//! Claude Code runs a status line by executing a command on every render. The
//! command is `~/.riabuild/claude-statusline`, this task is what puts that file
//! there, and the file is one `exec` into `riabuild internal statusline` — the
//! same shape as every launcher in `~/.riabuild/bin` and for the same reasons.
//! See `shims`'s module header, and `statusline` for what actually draws.
//!
//! It was `node ~/.riabuild/claude-statusline.js` until 2026-09-05: five hundred
//! lines of JavaScript, compiled in with `include_str!` and written out
//! verbatim. Two things went with it. The **interpreter** — a status line that
//! needed Node on `PATH` on every render, in a process Claude Code cancels after
//! 300ms — and the **language**, which is the half that mattered: nothing in
//! that file could be type-checked, and every test of it needed a subprocess and
//! an interpreter to say anything at all.
//!
//! What has not changed is where the file goes and who may read it. It lands in
//! `tools_root()` and never in `root()` — a distinction invisible on a laptop,
//! where the two are one directory, and the whole of whether remote mode has a
//! status line at all. See `Paths::claude_statusline_file`.
//!
//! **riabuild writes the `statusLine` setting too, and the server never sends
//! one.** A command is a program, the org settings may name a program and never
//! carry one, and the only way to keep those two sentences true at once was an
//! equality check against a string two repositories had to agree on. `vetting`
//! drops any `statusLine` the server sends and `org_settings` writes this task's
//! own command in its place, so what executes on a laptop is chosen by the
//! binary that installs it and by nothing else.

use super::{Ctx, Status, Task, TaskId};
use crate::shims;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_paths::contract_tilde;
use riabuild_ui::Failure;
use std::path::PathBuf;

pub struct ClaudeStatusline;

#[async_trait]
impl Task for ClaudeStatusline {
    fn id(&self) -> TaskId {
        "claude_statusline"
    }

    fn title(&self) -> &str {
        "Claude Code status line"
    }

    /// Stays at 1. `check()` compares the installed file against what riabuild
    /// would write *now*, so a shim that changes in a release — including one
    /// that names a new riabuild binary — is drift the check already sees, and
    /// there is nothing left for a version bump to catch.
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
            return Ok(Status::needs("the status line command cannot be read"));
        };
        // Hand-edited, truncated, left by an older riabuild, or naming a
        // riabuild binary this upgrade has moved — all of them mean this
        // machine is not running the status line this release ships.
        if installed != script()? {
            return Ok(Status::needs(
                "the status line is not the one this riabuild ships",
            ));
        }
        for stale in superseded_copies(ctx) {
            if tokio::fs::try_exists(&stale).await.unwrap_or(false) {
                return Ok(Status::needs(
                    "an older riabuild left a status line script behind",
                ));
            }
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        // Written beside the target and renamed over it, never truncated in
        // place, and made executable in the same call. On a server this path is
        // shared by everyone with an account on the box and Claude Code runs it
        // on **every render**, so a plain write hands whichever colleague is
        // mid-render half a file. `shims::write_script` is the one call in this
        // crate with all three properties, which is why this asks for it rather
        // than restating them — and its `0755` is load-bearing for a second
        // reason: riabuild's atomic write lands `0600`, which would be a status
        // line only whoever ran `riabuild` last can execute, and a status line
        // whose command fails renders as *no status line at all*.
        shims::write_executable(&ctx.paths.claude_statusline_file(), &script()?).await?;

        // The JavaScript every machine provisioned before this still has, and
        // the copy every *server* provisioned before 2026-08-17 has in its
        // namespace as well. Both are inert — nothing runs either any more —
        // and both are the first file a developer debugging a status line will
        // find, read, and draw a conclusion from. Removing them is the second
        // half of moving the file, not tidying.
        //
        // The failure is reported rather than swallowed, and that is the whole
        // reason this is not a bare `let _ =`. `check()` looks for these files,
        // so a removal that quietly did nothing comes back as "an older riabuild
        // left a script behind" on every run from here on, with nothing naming
        // the reason.
        for stale in superseded_copies(ctx) {
            if tokio::fs::try_exists(&stale).await.unwrap_or(false) {
                tokio::fs::remove_file(&stale).await.map_err(|error| {
                    Failure::new(
                        format!(
                            "removing the status line script an older riabuild left at {}",
                            contract_tilde(&stale, &ctx.paths.home())
                        ),
                        "delete that file and run `riabuild` again — the status line itself \
                         is already installed",
                    )
                    .detail(error.to_string())
                })?;
            }
        }
        Ok(())
    }
}

/// The one line the installed file holds.
///
/// riabuild is named by absolute path, like every other generated file — see
/// `shims`'s module header. A bare `riabuild` would find another machine's copy
/// or none at all, and Claude Code renders a status line whose command fails as
/// nothing whatsoever.
fn script() -> Result<String> {
    Ok(shims::statusline_shim_script(&shims::running_binary()?))
}

/// The status line scripts older riabuilds left, wherever they left them.
///
/// Two, and on a laptop they are one path: `<tools>/claude-statusline.js` is
/// where the JavaScript lived on every machine, and `<root>/claude-statusline.js`
/// is where a server put it before 2026-08-17 — in the developer's namespace,
/// which the org settings never named. Neither is the live file, whose name has
/// no `.js` on it, so nothing here can propose the installed status line for
/// deletion.
fn superseded_copies(ctx: &Ctx) -> Vec<PathBuf> {
    let mut found = vec![ctx.paths.tools_root().join("claude-statusline.js")];
    let namespaced = ctx.paths.root().join("claude-statusline.js");
    if !found.contains(&namespaced) {
        found.push(namespaced);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;

    #[tokio::test]
    async fn a_missing_status_line_needs_installing() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = ClaudeStatusline.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "{status:?}"
        );
    }

    /// The failure this exists for: an upgrade ships a status line that names a
    /// new riabuild, and a file-exists check would call the old one satisfied
    /// forever.
    #[tokio::test]
    async fn a_status_line_from_an_older_release_is_replaced() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_file(
            &ctx.paths.claude_statusline_file(),
            "#!/bin/sh\nexec '/opt/riabuild/2026.08.28/riabuild' internal statusline\n",
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
        assert_eq!(written, script().unwrap());
        assert_eq!(
            ClaudeStatusline.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    /// The installed command has to be the one the org settings name, and the
    /// two are written in different files. `org_settings::installed_status_line`
    /// derives that string from this path rather than spelling it out, so this
    /// is what pins the path itself.
    #[tokio::test]
    async fn the_status_line_is_installed_where_the_settings_name_it() {
        let (ctx, home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(
            ctx.paths.claude_statusline_file(),
            home.path().join(".riabuild/claude-statusline")
        );
    }

    /// The same question asked on a **server**, which is the machine the laptop
    /// shape above cannot speak for: there `root()` is a per-developer namespace
    /// and only `tools_root()` is still `~/.riabuild`, so a path built on the
    /// wrong one of the two satisfies every assertion above and lands somewhere
    /// the settings have never named.
    ///
    /// That is not hypothetical. It is what shipped: the script went to
    /// `~/.riabuild-remote/<member-id>/claude-statusline.js`, Claude Code ran
    /// the copy at `~/.riabuild/`, and a status line whose command fails renders
    /// as no status line at all — so remote mode never had one, silently, with
    /// the task reporting satisfied throughout.
    #[tokio::test]
    async fn the_status_line_lands_in_the_shared_tools_root_on_a_server() {
        let (ctx, home) = crate::testing::ctx_on_a_server(FakeRunner::new()).await;
        assert_ne!(
            ctx.paths.root(),
            ctx.paths.tools_root(),
            "this test is only meaningful where the two differ"
        );

        assert_eq!(
            ctx.paths.claude_statusline_file(),
            home.path().join(".riabuild/claude-statusline"),
            "`~` in the settings command is the account's home on a server — not \
             this developer's namespace"
        );
    }

    /// The JavaScript every machine provisioned before this has. It is inert —
    /// nothing runs it any more — which is exactly what makes it worth removing
    /// rather than ignoring: it is the first file a developer debugging a status
    /// line will find, and reading it proves nothing about what runs.
    #[tokio::test]
    async fn the_javascript_an_older_riabuild_installed_is_reported_and_removed() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        // The live file is installed and correct, so the byte comparison alone
        // says satisfied: without a check that looks for the old one, `apply`
        // would never run and the JavaScript would outlive every release.
        ClaudeStatusline.apply(&mut ctx).await.unwrap();
        let stale = ctx.paths.tools_root().join("claude-statusline.js");
        write_file(&stale, "// the old status line\n").await;

        let status = ClaudeStatusline.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("older riabuild"),
            "{status:?}"
        );

        ClaudeStatusline.apply(&mut ctx).await.unwrap();

        assert!(!tokio::fs::try_exists(&stale).await.unwrap());
        assert_eq!(
            ClaudeStatusline.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    /// And the copy a server provisioned before 2026-08-17 left in the
    /// developer's namespace, which is a second path on that machine and the
    /// same path on a laptop.
    #[tokio::test]
    async fn a_copy_left_in_the_namespace_by_an_older_riabuild_is_removed_too() {
        let (mut ctx, _home) = crate::testing::ctx_on_a_server(FakeRunner::new()).await;
        let stale = ctx.paths.root().join("claude-statusline.js");
        write_file(&stale, "// the old status line\n").await;
        ClaudeStatusline.apply(&mut ctx).await.unwrap();

        assert!(!tokio::fs::try_exists(&stale).await.unwrap());
        assert_eq!(
            ClaudeStatusline.check(&ctx).await.unwrap(),
            Status::Satisfied
        );
    }

    /// Nothing is left beside the file for Claude Code to trip over. The staged
    /// copy is named for this process, so a crash between the write and the
    /// rename would otherwise leave a permanent `.tmp` in the shared tree.
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

    /// The file has to be runnable by somebody other than whoever ran `riabuild`
    /// last.
    ///
    /// It lives under `tools_root()`, which every developer with an account on a
    /// server shares, and Claude Code executes it on every render. riabuild's
    /// atomic write lands `0600` — correct for a secret, and here it would
    /// silently take the status line away from every co-tenant while `check()`
    /// went on reporting the machine satisfied.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_installed_status_line_is_runnable_by_a_co_tenant() {
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
        assert_eq!(mode, 0o755, "{} is {mode:o}", file.display());
    }
}

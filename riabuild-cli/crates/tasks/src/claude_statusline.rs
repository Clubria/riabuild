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

/// The script's own behaviour, asked of the script.
///
/// Everything above this point tests the *file* — that it arrives, where it
/// arrives, who can read it. None of that can answer the question the status
/// line now has to get right: *which repository is this?* That answer is
/// computed in JavaScript from a checkout on disk, so these tests run the
/// shipped bytes on `node` the way Claude Code does, against `.git` directories
/// written by hand.
///
/// Written by hand rather than by `git`, on purpose. The script reads
/// `.git/config` as a file instead of shelling out — Claude Code re-renders a
/// status line continuously, and a subprocess per render is a cost a marker
/// does not justify — so a fixture needs no `git` binary either, and these
/// tests pin the on-disk layout the script actually depends on.
#[cfg(test)]
mod rendering {
    use super::SCRIPT;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// Runs the status line the way Claude Code does — `node <script>`, the
    /// payload on stdin, the session's directory as the cwd — and returns what
    /// it drew.
    ///
    /// A missing `node` fails rather than skips. riabuild installs a Node and
    /// this whole file is about a script that runs on one, so "no interpreter
    /// here" is a machine that cannot check what it is shipping — and a test
    /// that quietly passes in that state is the "recorded intention read as
    /// coverage" this module's own history has already paid for once.
    fn render(cwd: &Path, payload: &str) -> String {
        render_as(cwd, payload, None)
    }

    /// The same, with `CLAUDE_CONFIG_DIR` set to `account` — which is how the
    /// launchers start Claude Code, and therefore what the status line inherits.
    ///
    /// `None` **removes** the variable rather than merely not setting it. These
    /// tests are themselves run from a Claude Code session more often than not,
    /// and that session's own account directory is in the environment: inherited
    /// once, every assertion below about a line with no account in it would be
    /// checking the developer's own login instead of the fixture, and would pass
    /// or fail depending on whose machine ran it.
    fn render_as(cwd: &Path, payload: &str, account: Option<&Path>) -> String {
        let script = cwd.join("claude-statusline-under-test.js");
        std::fs::write(&script, SCRIPT).unwrap();

        let mut command = Command::new("node");
        match account {
            Some(dir) => command.env("CLAUDE_CONFIG_DIR", dir),
            None => command.env_remove("CLAUDE_CONFIG_DIR"),
        };
        let mut child = match command
            .arg(&script)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => panic!(
                "these tests run the status line on `node`, and there is none on PATH — \
                 put any Node there to check the script this release ships"
            ),
            Err(error) => panic!("running node: {error}"),
        };
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "the status line exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A checkout whose `origin` is `url`.
    fn checkout(at: &Path, url: &str) {
        write(
            &at.join(".git").join("config"),
            &format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n"),
        );
    }

    /// A linked worktree of `checkout`, laid out the way `git worktree add`
    /// leaves one: a `.git` *file* naming a directory under the checkout's own
    /// `.git`, and a `commondir` in that directory pointing back at the config
    /// the two share.
    fn worktree(checkout: &Path, at: &Path, name: &str) {
        let gitdir = checkout.join(".git").join("worktrees").join(name);
        write(&gitdir.join("commondir"), "../..\n");
        write(&at.join(".git"), &format!("gitdir: {}\n", gitdir.display()));
    }

    fn payload_at(dir: &Path) -> String {
        format!(
            r#"{{"workspace":{{"current_dir":{:?}}}}}"#,
            dir.to_string_lossy()
        )
    }

    #[test]
    fn the_marker_names_the_repository_the_session_is_in() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(home.path(), &payload_at(&dir));
        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
    }

    /// The repository goes *inside* the parentheses. There is one marker to
    /// learn, the same one the prompt draws — not a marker with a second thing
    /// sitting next to it that reads as two environments.
    #[test]
    fn the_repository_is_part_of_the_marker_rather_than_beside_it() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(home.path(), &payload_at(&dir));
        assert!(
            !drawn.contains("(riabuild)"),
            "the bare marker must not be drawn beside the named one: {drawn:?}"
        );
    }

    /// A linked worktree's `.git` is a *file* with no `config` behind it, so a
    /// walk that only recognises a `.git` **directory** finds nothing there and
    /// reports "not a repository" — in the one place a developer most needs
    /// telling which repository they are in. Following the `gitdir:` line to
    /// `commondir` is what makes it resolve.
    ///
    /// The worktree here sits **beside** the checkout rather than under it, and
    /// that placement is the test. riabuild's own live in
    /// `.claude/worktrees/`, physically inside the checkout — where the walk
    /// upwards reaches the main `.git` on its own and returns the right answer
    /// for the wrong reason. A fixture in that shape passes with the `commondir`
    /// branch deleted, which makes it no coverage of the branch at all.
    /// `git worktree add` accepts any path, so this one is also a real layout
    /// and not a contrivance built to fail.
    #[test]
    fn a_linked_worktree_resolves_to_the_repository_it_belongs_to() {
        let home = tempfile::TempDir::new().unwrap();
        let main = home.path().join("riabuild");
        checkout(&main, "git@github.com:Clubria/riabuild.git");
        let wt = home.path().join("worktrees").join("feat-thing");
        worktree(&main, &wt, "feat-thing");

        let drawn = render(home.path(), &payload_at(&wt));
        assert!(drawn.contains("(riabuild · Clubria/riabuild)"), "{drawn:?}");
    }

    /// The shape riabuild actually produces: a worktree under the checkout's own
    /// `.claude/worktrees/`. It resolves through `commondir` like any other, and
    /// the walk upwards would reach the same repository anyway — which is why
    /// this is here as the layout riabuild ships rather than as the proof, and
    /// the test above is the one that holds the branch honest.
    #[test]
    fn a_worktree_under_the_checkout_names_the_same_repository() {
        let home = tempfile::TempDir::new().unwrap();
        let main = home.path().join("riabuild");
        checkout(&main, "git@github.com:Clubria/riabuild.git");
        let wt = main.join(".claude").join("worktrees").join("feat-thing");
        worktree(&main, &wt, "feat-thing");

        let drawn = render(home.path(), &payload_at(&wt));
        assert!(drawn.contains("(riabuild · Clubria/riabuild)"), "{drawn:?}");
    }

    /// A directory *inside* the checkout is still in the repository. The walk
    /// upwards is the whole reason that holds, and a developer spends most of
    /// their time several directories down.
    #[test]
    fn a_directory_below_the_checkout_is_still_in_the_repository() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "https://github.com/Clubria/payments.git");
        let deep = dir.join("crates").join("api").join("src");
        std::fs::create_dir_all(&deep).unwrap();

        let drawn = render(home.path(), &payload_at(&deep));
        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
    }

    /// git records the same repository in several spellings depending on how it
    /// was cloned, and a developer who cloned over HTTPS is in the same
    /// repository as one who cloned over SSH. `Repo::matches_remote` accepts
    /// exactly these on the Rust side; the status line has to agree.
    #[test]
    fn every_spelling_of_a_remote_names_the_same_repository() {
        for url in [
            "git@github.com:Clubria/payments.git",
            "git@github.com:Clubria/payments",
            "https://github.com/Clubria/payments.git",
            "https://github.com/Clubria/payments",
            "https://github.com/Clubria/payments/",
            "ssh://git@github.com/Clubria/payments.git",
        ] {
            let home = tempfile::TempDir::new().unwrap();
            let dir = home.path().join("payments");
            checkout(&dir, url);

            let drawn = render(home.path(), &payload_at(&dir));
            assert!(
                drawn.contains("(riabuild · Clubria/payments)"),
                "{url} drew {drawn:?}"
            );
        }
    }

    /// `origin` is the remote riabuild cloned from and the one the `project`
    /// task verifies against; another remote in the same file is somebody
    /// else's fork.
    #[test]
    fn a_second_remote_does_not_get_mistaken_for_origin() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        write(
            &dir.join(".git").join("config"),
            "[remote \"upstream\"]\n\turl = git@github.com:someone-else/fork.git\n\
             [remote \"origin\"]\n\turl = git@github.com:Clubria/payments.git\n",
        );

        let drawn = render(home.path(), &payload_at(&dir));
        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
    }

    /// Two states that are not a mistake to be recovered from: a checkout with
    /// no `origin`, and a directory that is no checkout at all. Both keep the
    /// bare marker, because the alternative is guessing a repository out of a
    /// directory name and being confidently wrong on the status line.
    #[test]
    fn a_checkout_with_no_origin_keeps_the_bare_marker() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("scratch");
        write(&dir.join(".git").join("config"), "[core]\n\tbare = false\n");

        let drawn = render(home.path(), &payload_at(&dir));
        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
        assert!(!drawn.contains('·'), "{drawn:?}");
    }

    #[test]
    fn somewhere_that_is_not_a_checkout_keeps_the_bare_marker() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("not-a-repo");
        std::fs::create_dir_all(&dir).unwrap();

        let drawn = render(home.path(), &payload_at(&dir));
        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
        assert!(!drawn.contains('·'), "{drawn:?}");
    }

    /// The session's *current* directory, not the one it was launched in. A
    /// developer who has cd'd into their second checkout is in that repository,
    /// and `project_dir` would still be naming the first.
    #[test]
    fn the_repository_is_where_the_session_is_now() {
        let home = tempfile::TempDir::new().unwrap();
        let started_in = home.path().join("ai-builders-hub");
        checkout(&started_in, "git@github.com:Clubria/ai-builders-hub.git");
        let moved_to = home.path().join("payments");
        checkout(&moved_to, "git@github.com:Clubria/payments.git");

        let drawn = render(
            &started_in,
            &format!(
                r#"{{"cwd":{:?},"workspace":{{"current_dir":{:?},"project_dir":{:?}}}}}"#,
                moved_to.to_string_lossy(),
                moved_to.to_string_lossy(),
                started_in.to_string_lossy()
            ),
        );
        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
    }

    /// A Claude Code that sends no directory at all still gets an answer: the
    /// script is run in the session's own directory, so its cwd is the fallback.
    #[test]
    fn a_payload_with_no_directory_falls_back_to_where_the_script_runs() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(&dir, "{}");
        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
    }

    /// The context bar is what was already there, and naming the repository
    /// must not have cost it. Both are drawn, in that order.
    #[test]
    fn the_context_bar_still_draws_beside_the_repository() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(
            home.path(),
            &format!(
                r#"{{"workspace":{{"current_dir":{:?}}},
                    "context_window":{{"remaining_percentage":40,"total_tokens":1000000}}}}"#,
                dir.to_string_lossy()
            ),
        );
        let marker = drawn.find("Clubria/payments").expect("the repository");
        let bar = drawn.find('█').expect("the context bar");
        assert!(marker < bar, "{drawn:?}");
        assert!(drawn.contains("72%"), "{drawn:?}");
    }

    /// A status line that throws renders as *no status line at all*, which is a
    /// worse answer than an undecorated marker — so every way the input can be
    /// wrong ends at the label rather than at a stack trace.
    #[test]
    fn a_payload_that_is_not_json_still_leaves_a_marker() {
        let home = tempfile::TempDir::new().unwrap();
        let drawn = render(home.path(), "not json at all");
        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
    }

    #[test]
    fn a_directory_that_does_not_exist_still_leaves_a_marker() {
        let home = tempfile::TempDir::new().unwrap();
        let drawn = render(home.path(), &payload_at(&home.path().join("gone")));
        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
    }

    /// One developer's riabuild namespace: `config.json` at the root, and the
    /// account directories under `claude/` beside it, which is the layout
    /// `Paths::config_file` and `Paths::claude_profile_dir` produce on a laptop
    /// and on a server alike.
    ///
    /// Returns the directory `CLAUDE_CONFIG_DIR` would name for each account, in
    /// the order the accounts were given — so a test can hand the second one to
    /// `render_as` and assert it is called `claude-2`.
    fn namespace(root: &Path, accounts: &[(&str, Option<&str>)]) -> Vec<std::path::PathBuf> {
        let uuids: Vec<&str> = accounts.iter().map(|(uuid, _)| *uuid).collect();
        write(
            &root.join("config.json"),
            &format!(
                r#"{{"claude_accounts":[{}]}}"#,
                uuids
                    .iter()
                    .map(|uuid| format!("{uuid:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        );
        accounts
            .iter()
            .map(|(uuid, email)| {
                let dir = root.join("claude").join(uuid);
                std::fs::create_dir_all(&dir).unwrap();
                // A directory with no `.claude.json` is an account nothing has
                // signed in yet — which is a real state and not a broken one.
                if let Some(email) = email {
                    write(
                        &dir.join(".claude.json"),
                        &format!(r#"{{"oauthAccount":{{"emailAddress":{email:?}}}}}"#),
                    );
                }
                dir
            })
            .collect()
    }

    /// The question the marker cannot answer: *which of my logins is this
    /// window?* Two accounts, and the second one has to say so — the number
    /// comes from position in `claude_accounts` and nothing else records it.
    #[test]
    fn the_account_names_the_launcher_and_the_email() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[
                ("uuid-one", Some("ada@clubria.com")),
                ("uuid-two", Some("grace@clubria.com")),
            ],
        );

        let drawn = render_as(home.path(), "{}", Some(&dirs[1]));
        assert!(drawn.contains("claude-2 · grace@clubria.com"), "{drawn:?}");
        // The *other* account's email must not be what a number lookup reaches
        // for: an off-by-one here names a colleague's login on this line.
        assert!(!drawn.contains("ada@clubria.com"), "{drawn:?}");
    }

    /// The account sits **beside** the marker, which is the opposite of where
    /// the repository goes and is deliberate. The repository answers *which
    /// environment is this*, the question the shell prompt also answers, so it
    /// belongs inside the one marker. The account answers *who am I here* — the
    /// prompt does not carry it, and it changes without the environment
    /// changing — so folding it in would grow the marker a clause the prompt
    /// does not share.
    #[test]
    fn the_account_sits_beside_the_marker_rather_than_inside_it() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[("uuid-one", Some("ada@clubria.com"))],
        );
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render_as(home.path(), &payload_at(&dir), Some(&dirs[0]));
        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
        assert!(
            drawn.find("Clubria/payments") < drawn.find("claude-1"),
            "the account comes after the closing parenthesis: {drawn:?}"
        );
    }

    /// A signed-out account still names its launcher. `claude-2` with nothing
    /// after it is the answer to "which window is this?", and it is also how a
    /// developer notices they are signed out of the one they are typing into —
    /// so the two halves are drawn independently rather than all-or-nothing.
    #[test]
    fn a_signed_out_account_still_names_its_launcher() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[("uuid-one", None), ("uuid-two", Some("ada@clubria.com"))],
        );

        let drawn = render_as(home.path(), "{}", Some(&dirs[0]));
        assert!(drawn.contains("claude-1"), "{drawn:?}");
        assert!(!drawn.contains('@'), "{drawn:?}");
    }

    /// The other half of that: a `claude` pointed at a config directory
    /// riabuild's own list does not contain. There is no launcher to name, so
    /// none is invented — but who is signed in there is still true and still
    /// worth saying.
    #[test]
    fn an_account_riabuild_does_not_list_still_names_who_is_signed_in() {
        let home = tempfile::TempDir::new().unwrap();
        let root = home.path().join("ns");
        namespace(&root, &[("uuid-one", Some("ada@clubria.com"))]);
        let stranger = root.join("claude").join("uuid-unlisted");
        write(
            &stranger.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"solo@clubria.com"}}"#,
        );

        let drawn = render_as(home.path(), "{}", Some(&stranger));
        assert!(drawn.contains("solo@clubria.com"), "{drawn:?}");
        assert!(
            !drawn.contains("claude-"),
            "an unlisted directory has no number, and claude-0 is not a launcher: {drawn:?}"
        );
    }

    /// **The property a file every developer shares has to have.** This script
    /// lives in `tools_root()` — one copy for the whole box — while the accounts
    /// live under `root()`, a per-developer namespace. The same bytes must
    /// therefore answer differently for two colleagues, which they can only do
    /// by reading the namespace out of the running session's environment rather
    /// than out of anything baked into the script.
    ///
    /// The failure this forbids is a status line that names a colleague's email,
    /// which is worse than naming none.
    #[test]
    fn two_developers_on_one_server_get_their_own_account() {
        let home = tempfile::TempDir::new().unwrap();
        let ada = namespace(
            &home.path().join("member-a"),
            &[("uuid-a", Some("ada@clubria.com"))],
        );
        let grace = namespace(
            &home.path().join("member-b"),
            &[
                ("uuid-b1", Some("someone@clubria.com")),
                ("uuid-b2", Some("grace@clubria.com")),
            ],
        );

        let hers = render_as(home.path(), "{}", Some(&ada[0]));
        assert!(hers.contains("claude-1 · ada@clubria.com"), "{hers:?}");
        assert!(!hers.contains("grace@clubria.com"), "{hers:?}");

        let theirs = render_as(home.path(), "{}", Some(&grace[1]));
        assert!(
            theirs.contains("claude-2 · grace@clubria.com"),
            "{theirs:?}"
        );
        assert!(!theirs.contains("ada@clubria.com"), "{theirs:?}");
    }

    /// A `claude` the launchers did not start has no `CLAUDE_CONFIG_DIR`, so
    /// there is no account to name and the line is the one that shipped before
    /// this. Not an error state: a developer's own install is a real thing to
    /// find on a laptop.
    #[test]
    fn a_claude_the_launchers_did_not_start_draws_no_account() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render_as(home.path(), &payload_at(&dir), None);
        assert_eq!(
            drawn.matches('·').count(),
            1,
            "only the repository's separator: {drawn:?}"
        );
        assert!(!drawn.contains("claude-"), "{drawn:?}");
    }

    /// The account is not computed from the payload, so a payload that will not
    /// parse must not take it off the line. Which account this window is goes
    /// missing exactly when something is already wrong, otherwise.
    #[test]
    fn the_account_survives_a_payload_that_will_not_parse() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[("uuid-one", Some("ada@clubria.com"))],
        );

        let drawn = render_as(home.path(), "not json at all", Some(&dirs[0]));
        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
        assert!(drawn.contains("claude-1 · ada@clubria.com"), "{drawn:?}");
    }

    /// Claude Code rewrites `.claude.json` while it runs, so a render can read
    /// it mid-write. That is a parse error and *not* a signed-out account: it
    /// draws no email, rather than a fragment of one or a stack trace.
    #[test]
    fn a_half_written_config_draws_no_email_rather_than_half_of_one() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(&home.path().join("ns"), &[("uuid-one", None)]);
        write(
            &dirs[0].join(".claude.json"),
            r#"{"oauthAccount":{"emailAdd"#,
        );

        let drawn = render_as(home.path(), "{}", Some(&dirs[0]));
        assert!(drawn.contains("claude-1"), "{drawn:?}");
        assert!(!drawn.contains("emailAdd"), "{drawn:?}");
        assert!(!drawn.contains('@'), "{drawn:?}");
    }

    /// A namespace with no `config.json` at all — a machine riabuild has not
    /// provisioned, or one whose config was set aside as unreadable. There is no
    /// list to find a number in, and the email is in a different file that is
    /// still perfectly good.
    #[test]
    fn a_missing_config_costs_the_number_and_not_the_email() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("ns").join("claude").join("uuid-one");
        write(
            &dir.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"ada@clubria.com"}}"#,
        );

        let drawn = render_as(home.path(), "{}", Some(&dir));
        assert!(drawn.contains("ada@clubria.com"), "{drawn:?}");
        assert!(!drawn.contains("claude-"), "{drawn:?}");
    }

    /// The context bar is what was already there, and neither the repository nor
    /// the account may have cost it. All three are drawn, in reading order.
    #[test]
    fn the_context_bar_still_draws_beside_the_account() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[("uuid-one", Some("ada@clubria.com"))],
        );
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render_as(
            home.path(),
            &format!(
                r#"{{"workspace":{{"current_dir":{:?}}},
                    "context_window":{{"remaining_percentage":40,"total_tokens":1000000}}}}"#,
                dir.to_string_lossy()
            ),
            Some(&dirs[0]),
        );
        let repo = drawn.find("Clubria/payments").expect("the repository");
        let who = drawn.find("ada@clubria.com").expect("the account");
        let bar = drawn.find('█').expect("the context bar");
        assert!(repo < who && who < bar, "{drawn:?}");
        assert!(drawn.contains("72%"), "{drawn:?}");
    }
}

/// What the status line *collects*, run on a real `node`.
///
/// Separate from `rendering` because these need an environment — the spool path
/// the launcher hands over — and because the two answer different questions.
/// `rendering` asks what a developer sees; this asks what leaves the machine,
/// which is the half with a privacy answer attached to it.
#[cfg(test)]
mod collecting {
    use super::SCRIPT;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// As `rendering::render`, plus the environment the Claude launcher sets
    /// for an account the developer marked. `RIABUILD_SELF` is deliberately
    /// never set here: a test that spawned a real flush would be a test that
    /// posts to riabuild-web.
    fn render_with_spool(cwd: &Path, spool: Option<&Path>, payload: &str) -> String {
        let script = cwd.join("claude-statusline-under-test.js");
        std::fs::write(&script, SCRIPT).unwrap();

        let mut command = Command::new("node");
        command
            .arg(&script)
            .current_dir(cwd)
            .env_remove("RIABUILD_USAGE_SPOOL")
            .env_remove("RIABUILD_SELF")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(spool) = spool {
            command.env("RIABUILD_USAGE_SPOOL", spool);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                panic!("these tests run the status line on `node`, and there is none on PATH")
            }
            Err(error) => panic!("running node: {error}"),
        };
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "the status line exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A payload of the shape Claude Code documents, with the fields this
    /// collects and several it must ignore.
    fn payload() -> String {
        serde_json::json!({
            "session_id": "sess-1",
            "model": { "id": "claude-opus-5", "display_name": "Opus" },
            "workspace": { "current_dir": "/tmp", "project_dir": "/tmp" },
            "cost": {
                "total_cost_usd": 0.5,
                "total_duration_ms": 45_000,
                "total_api_duration_ms": 2_300,
                "total_lines_added": 156,
                "total_lines_removed": 23
            },
            "context_window": {
                "remaining_percentage": 92,
                "total_input_tokens": 15_500,
                "total_output_tokens": 1_200
            },
            "rate_limits": {
                "five_hour": { "used_percentage": 23.5, "resets_at": 1_738_425_600u64 },
                "seven_day": { "used_percentage": 41.2, "resets_at": 1_738_857_600u64 }
            }
        })
        .to_string()
    }

    /// The default, and the whole of the privacy answer: with no spool in the
    /// environment nothing is written anywhere.
    #[test]
    fn an_untracked_account_writes_nothing() {
        let home = tempfile::TempDir::new().unwrap();

        let drawn = render_with_spool(home.path(), None, &payload());

        assert!(
            drawn.contains("(riabuild"),
            "the bar still renders: {drawn:?}"
        );
        let stray: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".ndjson"))
            .collect();
        assert!(stray.is_empty(), "nothing may be spooled: {stray:?}");
    }

    /// A tracked account writes exactly one line, and it carries the fields the
    /// server merges by maximum.
    #[test]
    fn a_tracked_account_spools_one_line_per_render() {
        let home = tempfile::TempDir::new().unwrap();
        let spool = home.path().join("usage").join("acc-uuid.ndjson");

        render_with_spool(home.path(), Some(&spool), &payload());
        render_with_spool(home.path(), Some(&spool), &payload());

        let written = std::fs::read_to_string(&spool).unwrap();
        let lines: Vec<_> = written.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "one line per render: {written:?}");

        let sample: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(sample["harness"], "claude");
        assert_eq!(sample["sessionId"], "sess-1");
        // The file name *is* the account, so nothing has to be passed twice.
        assert_eq!(sample["accountId"], "acc-uuid");
        assert_eq!(sample["costUsd"], 0.5);
        assert_eq!(sample["fiveHourPct"], 23.5);
        assert_eq!(sample["sevenDayPct"], 41.2);
    }

    /// The fields that read like session totals and are not.
    ///
    /// `context_window.total_input_tokens` is documented as the tokens
    /// *currently in the window* — zero before the first response, smaller
    /// again after every `/compact` — so merged by maximum it would report peak
    /// context size under a heading that said "tokens". This test is what stops
    /// it being added back because the payload obviously has it.
    #[test]
    fn no_token_count_is_ever_spooled() {
        let home = tempfile::TempDir::new().unwrap();
        let spool = home.path().join("usage").join("acc.ndjson");

        render_with_spool(home.path(), Some(&spool), &payload());

        let written = std::fs::read_to_string(&spool).unwrap();
        for forbidden in ["Token", "token", "15500", "1200"] {
            assert!(
                !written.contains(forbidden),
                "no token figure may be spooled ({forbidden}): {written}"
            );
        }
    }

    /// Nothing about *what* the developer was doing leaves the machine.
    ///
    /// The script has the repository in hand — it draws it in the marker — and
    /// the payload carries the transcript path beside it. Neither is collected,
    /// and a column that appeared later would have to pass this test first.
    #[test]
    fn nothing_about_the_work_itself_is_spooled() {
        let home = tempfile::TempDir::new().unwrap();
        let spool = home.path().join("usage").join("acc.ndjson");
        let payload = serde_json::json!({
            "session_id": "sess-1",
            "transcript_path": "/home/ada/.claude/projects/x/sess-1.jsonl",
            "workspace": {
                "current_dir": "/home/ada/Clubria/payments",
                "repo": { "host": "github.com", "owner": "Clubria", "name": "payments" }
            },
            "cost": { "total_cost_usd": 0.5 }
        })
        .to_string();

        render_with_spool(home.path(), Some(&spool), &payload);

        let written = std::fs::read_to_string(&spool).unwrap();
        for forbidden in ["payments", "Clubria", "transcript", "jsonl", "/home/ada"] {
            assert!(
                !written.contains(forbidden),
                "the work itself must not be spooled ({forbidden}): {written}"
            );
        }
    }

    /// An API-key or Console login has no `rate_limits`, and a session before
    /// its first response has no `cost`. Neither may become a zero.
    #[test]
    fn an_unmeasured_field_is_absent_rather_than_zero() {
        let home = tempfile::TempDir::new().unwrap();
        let spool = home.path().join("usage").join("acc.ndjson");

        render_with_spool(
            home.path(),
            Some(&spool),
            &serde_json::json!({ "session_id": "sess-1" }).to_string(),
        );

        let written = std::fs::read_to_string(&spool).unwrap();
        let sample: serde_json::Value = serde_json::from_str(written.trim()).unwrap();
        assert!(sample.get("costUsd").is_none(), "{written}");
        assert!(sample.get("fiveHourPct").is_none(), "{written}");
        assert_eq!(sample["sessionId"], "sess-1");
    }

    /// A render before Claude Code has a session is not a sample.
    #[test]
    fn a_payload_with_no_session_spools_nothing() {
        let home = tempfile::TempDir::new().unwrap();
        let spool = home.path().join("usage").join("acc.ndjson");

        render_with_spool(home.path(), Some(&spool), "{}");

        assert!(!spool.exists(), "no session, no sample");
    }

    /// A spool that cannot be written must not cost the developer their status
    /// line. The directory is made read-only so `appendFileSync` throws.
    #[cfg(unix)]
    #[test]
    fn a_spool_that_cannot_be_written_still_leaves_a_status_line() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::TempDir::new().unwrap();
        let locked = home.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        let drawn = render_with_spool(
            home.path(),
            Some(&locked.join("usage").join("acc.ndjson")),
            &payload(),
        );

        assert!(drawn.contains("(riabuild"), "{drawn:?}");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

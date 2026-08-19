//! Task 15 — git's GitHub credential helper, on every machine rather than only
//! the ones riabuild signed in itself.
//!
//! `github_cli` already settles this on one path: `sign_in::run_gh_auth` calls
//! [`own_git_credentials`] before it hands `gh` the terminal, because the
//! question `gh` would otherwise ask is unanswerable under a subdued child. But
//! that is the *sign-in* path, and a machine whose `gh` is already signed in
//! never reaches it:
//!
//! - a developer who ran `gh auth login` themselves before their first
//!   `riabuild`;
//! - a machine provisioned before that call existed;
//! - and every server `riabuild remote` sets up, where `internal seed-github`
//!   signs `gh` in with `gh auth login --with-token` over a non-interactive
//!   connection and does nothing else. That path is unattended by design — the
//!   whole point of the seed is that no device code is printed — so it is
//!   exactly the one that will never reach an interactive sign-in.
//!
//! On all three, `github_cli`'s `check()` is satisfied, so its `apply()` never
//! runs and neither does the `setup-git` inside it. Nothing looks wrong.
//! `gh repo clone` still works, because `gh` hands credentials to the one `git`
//! child it spawns and writes nothing down — so the checkout lands perfectly,
//! riabuild reports success, and the first `git push` the developer runs
//! *themselves* meets a username prompt. A machine that can clone but cannot
//! push is a machine riabuild did not finish, and this is the worst place for
//! it to fail: after the provisioner has said it is done.
//!
//! Making it a task rather than one more line in `github_cli` is what fixes it
//! for a machine already in that state. `apply()` runs only when a `check()`
//! fails, so an end state nothing checks is an end state riabuild reaches only
//! by accident of which other task happened to run.
//!
//! ## Why `check()` looks at *which* gh
//!
//! `gh` records the absolute path of the binary that ran it, so the helper
//! names one particular `gh` — and "some gh is the helper" is not the fact
//! riabuild needs. A developer's own Homebrew or apt `gh`, signed out, answers
//! git perfectly well and gives it nothing; the only `gh` known to hold the
//! sign-in `github_cli` verified is the one riabuild owns.
//!
//! That the path is version-pinned (`~/.riabuild/gh/<version>/bin/gh`) makes
//! this stricter than it strictly has to be, and deliberately so. A helper left
//! by an earlier pin keeps *working* — tool directories are per-version and
//! only `riabuild reset` removes one — so this is not a repair of something
//! broken. It costs one command on the run after a pin bump, and buys a
//! developer's `git push` going through the `gh` riabuild currently installs
//! and verifies rather than one it used to.

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;

pub struct GitCredentials;

/// The host riabuild sets git up for.
///
/// Named rather than left to `gh`'s "every host you are signed in to", for the
/// reason `github_cli` names it on `auth refresh`: riabuild is provisioning one
/// organisation on github.com, and a developer's own GitHub Enterprise sign-in
/// is not riabuild's to reconfigure.
const HOST: &str = "github.com";

/// The `git config` key `gh` writes, and the only one worth reading.
///
/// Read without `--global` on purpose. What matters is the helper git will
/// *use*, which is the system and global files together — not the one file
/// riabuild happens to write. A helper installed system-wide by a corporate
/// image is a real answer to "can this developer push?", and a check that read
/// only `--global` would report drift the `apply()` after it could not clear.
fn helper_key() -> String {
    format!("credential.https://{HOST}.helper")
}

/// Every configured helper for the host, in the order git would consult them.
///
/// An unset key exits non-zero, and so does a machine with no git on it at all;
/// both are "no helper here" as far as this task is concerned, and the second
/// is reported properly by the `apply()` that follows rather than by an error
/// thrown out of `check()`. Same reasoning as `project::origin_url`.
async fn helpers(ctx: &Ctx) -> Vec<String> {
    let key = helper_key();
    let Ok(output) = ctx
        .runner
        .run(
            "git",
            &["config", "--get-all", &key],
            &RunOptions::default(),
        )
        .await
    else {
        return Vec::new();
    };
    if !output.ok() {
        return Vec::new();
    }
    output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Splits a shell-command helper into its program and the rest, honouring the
/// single quotes `gh` puts round a path containing a space.
///
/// `gh` quotes only when it has to, so both `!/home/ada/…/gh auth git-credential`
/// and `!'/Users/ada smith/…/gh' auth git-credential` are things it writes.
/// Comparing the whole line against one literal string would pass on one
/// developer's machine and demand a pointless re-`apply()` on the next one's,
/// on every run, forever.
fn split_program(command: &str) -> (&str, &str) {
    if let Some(rest) = command.strip_prefix('\'')
        && let Some(end) = rest.find('\'')
    {
        return (&rest[..end], &rest[end + 1..]);
    }
    match command.split_once(char::is_whitespace) {
        Some((program, rest)) => (program, rest),
        None => (command, ""),
    }
}

/// Does this helper value hand the credential to the `gh` riabuild owns?
fn delegates_to(value: &str, gh: &str) -> bool {
    let Some(command) = value.strip_prefix('!') else {
        // A bare `helper = manager` or `helper = osxkeychain` names a
        // credential-helper binary rather than a shell command. Whatever that
        // one holds, it is not gh's token.
        return false;
    };
    let (program, rest) = split_program(command.trim());
    program == gh && rest.split_whitespace().eq(["auth", "git-credential"])
}

/// Makes `gh` git's credential helper for github.com.
///
/// The one place the command is spelled. `pub(crate)` for the caller outside
/// this task — `github_cli::sign_in::run_gh_auth`, which has to settle this
/// *before* it hands `gh` the terminal, because `gh` asks the question itself
/// otherwise and a `survey` prompt under a pty riabuild owns cannot be answered
/// at all. See the call site there for that whole story; it is a different
/// reason for the same command, not a second copy of this task.
///
/// Two spellings of one command would be two failure messages and two things to
/// keep in step with `check()` above, which is a drift this repository can do
/// without.
///
/// The write is `gh`'s rather than a `git config` of riabuild's, and not only to
/// avoid owning `gh`'s config format. `gh` writes *two* helper lines for the
/// host: an empty one, which tells git to discard every helper configured
/// before it, and then itself. Writing only the second would leave an
/// `osxkeychain` entry holding a years-old password answering first, which
/// fails in the one way a provisioner must never produce — quietly, on a
/// machine that reports itself as set up.
///
/// The token stays in gh's store, expires on gh's schedule, and is never copied
/// into a file of riabuild's own, so nothing written here is a secret riabuild
/// keeps — only a line saying who to ask.
///
/// `--force` is what lets the sign-in caller run this *before* github.com is an
/// authenticated host, which `setup-git` otherwise refuses. It costs this task
/// nothing, and one shared command is worth more than a saved flag.
pub(crate) async fn own_git_credentials(ctx: &mut Ctx) -> Result<()> {
    let done = ctx
        .runner
        .run(
            &ctx.gh(),
            &["auth", "setup-git", "--hostname", HOST, "--force"],
            &RunOptions::default(),
        )
        .await?;
    if done.ok() {
        return Ok(());
    }
    Err(Failure::new(
        "letting gh manage git's GitHub credentials",
        "Check that `git` is installed and that your global git config is writable, \
         then run `riabuild` again.",
    )
    .command(format!("gh auth setup-git --hostname {HOST} --force"))
    .detail(match done.stderr.trim() {
        "" => "that command failed and said nothing".to_string(),
        stderr => stderr.to_string(),
    })
    .into())
}

#[async_trait]
impl Task for GitCredentials {
    fn id(&self) -> TaskId {
        "git_credentials"
    }

    fn title(&self) -> &str {
        "Git credentials"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // `gh auth setup-git` writes the path of a gh that has to be installed,
        // and the sign-in it delegates to has to exist. The edge is also what
        // brings this task back after a gh pin bump: `github_cli` applying moves
        // `Ctx::gh`, and the helper has to follow it.
        &["github_cli"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let gh = ctx.gh();
        if !tokio::fs::try_exists(&gh).await.unwrap_or(false) {
            // Nothing to point git at yet. `github_cli` installs it, and the
            // dependency edge above brings this task back afterwards. Reporting
            // the helper as wrong here would be true and useless: the remedy is
            // not this task's.
            return Ok(Status::needs("riabuild has not installed gh yet"));
        }

        let configured = helpers(ctx).await;
        if configured.is_empty() {
            return Ok(Status::needs(format!(
                "git has no credential helper for {HOST}, so `git push` would ask for a password"
            )));
        }
        if !configured.iter().any(|value| delegates_to(value, &gh)) {
            // Quoted in full rather than summarised. The value is a helper
            // command, not a credential, and a developer looking at a machine
            // that will not push needs to see what is answering instead.
            return Ok(Status::needs(format!(
                "git asks `{}` for {HOST} credentials, not the gh riabuild owns",
                configured.join("`, `")
            )));
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        // Safe to run twice: gh replaces both of its lines rather than
        // appending a third, so a re-run rewrites what is already there.
        own_git_credentials(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, install_owned_tools};
    use riabuild_runner::FakeRunner;
    use std::sync::Arc;
    use tempfile::TempDir;

    const READ: &str = "git config --get-all credential.https://github.com.helper";
    const SETUP: &str = "gh auth setup-git";

    /// A provisioned machine whose git answers `helper` when asked for the
    /// credential helper.
    ///
    /// Built in two steps, with the runner swapped in afterwards, because the
    /// value under test is the path of the gh riabuild owns — which only exists
    /// once the fixture home does, so no stub can be written before then.
    async fn machine_where_git_says(
        helper: impl FnOnce(&str) -> String,
    ) -> (Ctx, TempDir, Arc<FakeRunner>) {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        install_owned_tools(&ctx).await;

        let stdout = helper(&ctx.gh());
        // An unset key is git exiting 1 with nothing on stdout, not exiting 0
        // with a blank line. Getting that wrong here would hide the difference
        // between "no helper" and "a helper that says nothing".
        let code = i32::from(stdout.trim().is_empty());
        let runner = Arc::new(
            FakeRunner::new()
                .with(READ, code, &stdout, "")
                .with(SETUP, 0, "", ""),
        );
        ctx.runner = runner.clone();
        (ctx, home, runner)
    }

    /// What `gh auth setup-git` leaves behind: the empty helper that severs the
    /// chain, then gh itself.
    fn set_up_by_gh(gh: &str) -> String {
        format!("\n!{gh} auth git-credential\n")
    }

    #[tokio::test]
    async fn a_machine_gh_has_set_git_up_on_is_satisfied() {
        let (ctx, _home, _runner) = machine_where_git_says(set_up_by_gh).await;
        assert_eq!(GitCredentials.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_path_gh_had_to_quote_still_counts() {
        let (ctx, _home, _runner) =
            machine_where_git_says(|gh| format!("\n!'{gh}' auth git-credential\n")).await;
        assert_eq!(GitCredentials.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_signed_in_gh_with_no_helper_written_is_detected() {
        // The seeded-server case, and the reported one: gh is signed in, so
        // `github_cli` is satisfied and never runs its own setup-git.
        let (ctx, _home, _runner) = machine_where_git_says(|_| String::new()).await;
        let status = GitCredentials.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("no credential helper"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn another_helper_holding_a_stale_password_is_not_accepted() {
        let (ctx, _home, _runner) = machine_where_git_says(|_| "osxkeychain\n".into()).await;
        let status = GitCredentials.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("osxkeychain"), "{status:?}");
    }

    #[tokio::test]
    async fn a_gh_riabuild_did_not_install_is_not_accepted() {
        // A developer's own Homebrew gh answers git perfectly well and may be
        // signed out. It is not the gh whose sign-in `github_cli` verified.
        let (ctx, _home, _runner) =
            machine_where_git_says(|_| "!/opt/homebrew/bin/gh auth git-credential\n".into()).await;
        let status = GitCredentials.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the gh riabuild owns"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_helper_left_by_an_earlier_pin_is_detected() {
        let (ctx, _home, _runner) = machine_where_git_says(|_| {
            "\n!/home/ada/.riabuild/gh/2.1.0/bin/gh auth git-credential\n".into()
        })
        .await;
        let status = GitCredentials.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the gh riabuild owns"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_gh_asked_for_something_other_than_a_credential_is_not_accepted() {
        let (ctx, _home, _runner) =
            machine_where_git_says(|gh| format!("!{gh} auth status\n")).await;
        assert!(matches!(
            GitCredentials.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn a_bare_machine_is_sent_to_the_task_that_installs_gh() {
        // `ctx_with` rather than the fixture above: no gh on disk.
        let (ctx, _home) = ctx_with(FakeRunner::new().with(READ, 1, "", "")).await;
        let status = GitCredentials.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("has not installed gh"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn applying_sets_git_up_for_github_only() {
        let (mut ctx, _home, runner) = machine_where_git_says(|_| String::new()).await;

        GitCredentials.apply(&mut ctx).await.unwrap();

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.ends_with("gh auth setup-git --hostname github.com --force")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn riabuild_never_writes_the_helper_itself() {
        // Pins the decision in `own_git_credentials`. A `git config` write here
        // would be riabuild reimplementing the half of gh's setup it can see
        // and dropping the half it cannot — the empty line that severs the
        // chain of helpers configured before it.
        let (mut ctx, _home, runner) = machine_where_git_says(|_| String::new()).await;

        GitCredentials.apply(&mut ctx).await.unwrap();

        let writes: Vec<_> = runner
            .calls()
            .into_iter()
            .filter(|call| call.contains("git config") && !call.contains("--get"))
            .collect();
        assert!(writes.is_empty(), "{writes:?}");
    }

    #[tokio::test]
    async fn a_failing_setup_git_names_the_command_and_the_reason() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new().with(READ, 1, "", "").with(
            SETUP,
            1,
            "",
            "failed to run git: exec: \"git\": not found",
        ))
        .await;
        install_owned_tools(&ctx).await;

        let error = GitCredentials.apply(&mut ctx).await.unwrap_err();
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("not a Failure: {error:#}"));
        assert_eq!(
            failure.command.as_deref(),
            Some("gh auth setup-git --hostname github.com --force")
        );
        assert!(failure.detail.contains("not found"), "{}", failure.detail);
    }

    #[tokio::test]
    async fn a_setup_git_that_fails_silently_still_says_something() {
        let (mut ctx, _home) = ctx_with(
            FakeRunner::new()
                .with(READ, 1, "", "")
                .with(SETUP, 1, "", ""),
        )
        .await;
        install_owned_tools(&ctx).await;

        let error = GitCredentials.apply(&mut ctx).await.unwrap_err();
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("not a Failure: {error:#}"));
        assert_eq!(failure.detail, "that command failed and said nothing");
    }
}

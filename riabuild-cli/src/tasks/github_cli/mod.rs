//! Task 2 — `gh`, installed, signed in, and able to see this developer's
//! Clubria membership.
//!
//! Ways this is wrong on a machine that worked last month: `gh` uninstalled,
//! `gh` logged out, a token that expired or was revoked, a token that cannot
//! read org membership, an invite never accepted, and a developer removed from
//! the org. Each one has a different remedy, so each one is detected
//! separately.
//!
//! This file holds the task itself — what `check` looks at, what `apply` does
//! about it, and installing `gh` when it is missing entirely. The two pieces
//! `apply` drives sit beside it: `membership` asks GitHub the org question and
//! decodes the answer, and `sign_in` is the browser round trip.

mod membership;
mod sign_in;

use membership::{Membership, membership};
use sign_in::{run_gh_auth, sign_in};

use super::{Ctx, Status, Task, TaskId};
use crate::runner::RunOptions;
use crate::shims;
use crate::tools;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use async_trait::async_trait;

const MIN_VERSION: &str = "2.40.0";
pub(super) const ORG: &str = "Clubria";

/// The scope riabuild *requests* when a token cannot read org membership.
///
/// Deliberately never the scope riabuild *tests for*. GitHub accepts
/// `admin:org`, `read:org`, `repo`, `user`, and `write:org` on the membership
/// endpoint, and folds `read:org` into `admin:org` when both are granted. See
/// `membership`.
pub(super) const ORG_SCOPE: &str = "read:org";

pub struct GithubCli;

#[async_trait]
impl Task for GithubCli {
    fn id(&self) -> TaskId {
        "github_cli"
    }

    fn title(&self) -> &str {
        "GitHub CLI"
    }

    /// Bumped to 2 when riabuild took ownership of `gh` instead of installing
    /// it with Homebrew. Every machine set up before that has a `gh` riabuild
    /// does not manage, and `check()` alone would keep accepting it.
    fn version(&self) -> u32 {
        2
    }

    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let gh = ctx.gh();
        if !tokio::fs::try_exists(&gh).await.unwrap_or(false) {
            return Ok(Status::needs(format!(
                "riabuild has not installed gh {} yet",
                tools::GH_VERSION
            )));
        }

        // The owned copy is a known version, so this catches a truncated or
        // corrupted install rather than an old release — which is why it
        // reports what it found rather than "gh is too old".
        let version_output = ctx
            .runner
            .run(&gh, &["--version"], &RunOptions::default())
            .await?;
        if !version::at_least(version_output.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!(
                "the gh in ~/.riabuild reports `{}`, which is not usable",
                version_output.trimmed()
            )));
        }

        if !ctx
            .runner
            .run(&gh, &["auth", "status"], &RunOptions::default())
            .await?
            .ok()
        {
            return Ok(Status::needs("gh is not signed in to GitHub"));
        }

        Ok(match membership(ctx).await? {
            Membership::Active => Status::Satisfied,
            other => Status::needs(other.describe()),
        })
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if !tokio::fs::try_exists(&ctx.gh()).await.unwrap_or(false) {
            install(ctx).await?;
        }

        if !ctx
            .runner
            .run(&ctx.gh(), &["auth", "status"], &RunOptions::default())
            .await?
            .ok()
        {
            sign_in(ctx).await?;
        }

        // Ask GitHub the question before asking the developer for anything.
        // Most tokens can already answer it — `gh auth login` grants `repo` by
        // default, which GitHub accepts here — so a browser round trip is the
        // exception, not the routine.
        let mut state = membership(ctx).await?;
        match state {
            // The token itself is no longer valid; refreshing scopes on a dead
            // token cannot work, so sign in from scratch.
            Membership::SignedOut => {
                sign_in(ctx).await?;
                state = membership(ctx).await?;
            }
            Membership::Forbidden => {
                ctx.ui.note(&format!(
                    "Adding the {ORG_SCOPE} permission to your GitHub token…"
                ));
                run_gh_auth(
                    ctx,
                    &[
                        "auth",
                        "refresh",
                        "--hostname",
                        "github.com",
                        "--scopes",
                        ORG_SCOPE,
                    ],
                    format!("adding the {ORG_SCOPE} permission to your GitHub token"),
                )
                .await?;
                state = membership(ctx).await?;
            }
            _ => {}
        }

        // Membership is GitHub's to grant. Everything riabuild could do about
        // it has now been done, so anything left is reported with the remedy
        // that actually applies rather than left to the engine's generic
        // "it did not take effect".
        match state {
            Membership::Active => Ok(()),
            Membership::Pending => Err(Failure::new(
                format!("checking your {ORG} GitHub membership"),
                format!("Accept the {ORG} invite in your email or at https://github.com/orgs/{ORG}/invitation, then run `riabuild` again."),
            )
            .detail(format!("GitHub reports your {ORG} membership as invited but not accepted"))
            .into()),
            Membership::NotAMember => Err(Failure::new(
                format!("checking your {ORG} GitHub membership"),
                format!("Ask your team lead to invite you to the {ORG} GitHub organisation, then accept the invite."),
            )
            .command(format!("gh api /user/memberships/orgs/{ORG}"))
            .detail(format!(
                "GitHub does not report you as a member of {ORG}. If you have \
                 already accepted the invite, your GitHub token may not be \
                 allowed to read organisation membership — \
                 `gh auth refresh -h github.com -s {ORG_SCOPE}` fixes that."
            ))
            .into()),
            other => Err(Failure::new(
                format!("checking your {ORG} GitHub membership"),
                "Run `riabuild` again; if it keeps failing, send this message to your team lead.",
            )
            .command(format!("gh api /user/memberships/orgs/{ORG}"))
            .detail(other.describe())
            .into()),
        }
    }
}

/// Downloads the pinned `gh` into `~/.riabuild/gh/<version>/`.
///
/// This used to be `brew install gh`, which meant riabuild could not set up a
/// machine without Homebrew on it and had nothing to offer on Linux at all.
///
/// `pub(crate)` for one caller outside this task: `internal::seed_github`, which
/// runs on a server *before* the setup pass that would otherwise install this,
/// and so has to be able to put `gh` there itself. See its doc comment.
pub(crate) async fn install(ctx: &mut Ctx) -> Result<()> {
    let release = tools::gh()?;
    ctx.ui.note(&format!("Downloading gh {}…", release.version));

    let tool_dir = ctx.paths.tool_dir(release.tool, release.version);
    tools::install(&release, &tool_dir).await.map_err(|error| {
        Failure::new(
            "installing the GitHub CLI",
            "Check your network connection and run `riabuild` again. If it keeps \
                 failing, send this to your team lead.",
        )
        .detail(format!("{error:#}"))
    })?;

    shims::write_tool(ctx, "gh", &release.binary_in(&tool_dir)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_and_runner, ctx_with, ctx_with_tools, install_owned_tools};
    use crate::ui::Ui;

    const MEMBERSHIP: &str = "gh api /user/memberships/orgs/Clubria";

    fn healthy() -> FakeRunner {
        FakeRunner::new()
            .with("gh --version", 0, "gh version 2.96.0 (2026-07-02)", "")
            .with(
                "gh auth status",
                0,
                "",
                "github.com\n  ✓ Logged in to github.com account ada\n  - Token scopes: 'gist', 'read:org', 'repo'",
            )
            .with(MEMBERSHIP, 0, r#"{"state":"active","role":"member"}"#, "")
    }

    async fn reason(runner: FakeRunner) -> String {
        let (ctx, _home) = ctx_with_tools(runner).await;
        format!("{:?}", GithubCli.check(&ctx).await.unwrap())
    }

    #[tokio::test]
    async fn a_healthy_machine_is_satisfied() {
        let (ctx, _home) = ctx_with_tools(healthy()).await;
        assert_eq!(GithubCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_gh_riabuild_has_not_installed_is_detected() {
        // A bare machine, and also a machine with a system gh on PATH: neither
        // is the binary riabuild verified, so both need the install.
        let (ctx, _home) = ctx_with(healthy()).await;
        let status = GithubCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("has not installed gh"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn an_old_gh_is_detected() {
        let runner = healthy().with("gh --version", 0, "gh version 2.10.0 (2023-01-01)", "");
        assert!(reason(runner).await.contains("not usable"));
    }

    #[tokio::test]
    async fn a_logged_out_gh_is_detected() {
        let runner = healthy().with("gh auth status", 1, "", "You are not logged into any hosts");
        assert!(reason(runner).await.contains("not signed in"));
    }

    #[tokio::test]
    async fn an_admin_org_token_can_read_membership() {
        // The bug this file exists to not have again. `admin:org` grants
        // everything `read:org` does, and GitHub does not list both, so a
        // check that looked for the literal string rejected a token that
        // worked — and `gh auth refresh` could never change its mind.
        let runner = healthy().with(
            "gh auth status",
            0,
            "",
            "github.com\n  ✓ Logged in to github.com account ada\n  - Token scopes: 'admin:org', 'gist', 'repo'",
        );
        let (ctx, _home) = ctx_with_tools(runner).await;
        assert_eq!(GithubCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_token_that_cannot_read_membership_is_detected() {
        let runner = healthy().with(MEMBERSHIP, 1, "", "gh: Forbidden (HTTP 403)");
        assert!(reason(runner).await.contains("may not read"));
    }

    #[tokio::test]
    async fn an_expired_token_is_told_apart_from_a_missing_permission() {
        // Different remedy: refreshing scopes on a dead token cannot work.
        let runner = healthy().with(MEMBERSHIP, 1, "", "gh: Bad credentials (HTTP 401)");
        assert!(reason(runner).await.contains("no longer valid"));
    }

    #[tokio::test]
    async fn a_pending_invite_is_not_membership() {
        let runner = healthy().with(MEMBERSHIP, 0, r#"{"state":"pending"}"#, "");
        assert!(reason(runner).await.contains("not been accepted"));
    }

    #[tokio::test]
    async fn being_removed_from_the_org_is_detected() {
        let runner = healthy().with(MEMBERSHIP, 1, "", "gh: Not Found (HTTP 404)");
        assert!(reason(runner).await.contains("not report you as a member"));
    }

    #[tokio::test]
    async fn an_unreachable_github_is_not_mistaken_for_a_rejection() {
        // A captive portal must not read as "you were removed from the org".
        let runner = healthy().with(MEMBERSHIP, 1, "", "dial tcp: lookup api.github.com");
        let described = reason(runner).await;
        assert!(described.contains("could not check"), "{described}");
        assert!(
            !described.contains("not report you as a member"),
            "{described}"
        );
    }

    #[tokio::test]
    async fn a_working_token_is_never_sent_through_a_browser() {
        // The regression that matters most: apply() must not demand a sign-in
        // from a developer whose token already answers the question.
        let (mut ctx, _home, runner) = ctx_and_runner(healthy()).await;
        install_owned_tools(&ctx).await;
        GithubCli.apply(&mut ctx).await.unwrap();
        let calls = runner.calls();
        assert!(
            !calls
                .iter()
                .any(|call| call.contains("auth login") || call.contains("auth refresh")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_permission_is_repaired_before_giving_up() {
        let runner = healthy()
            .with(MEMBERSHIP, 1, "", "gh: Forbidden (HTTP 403)")
            .with("gh auth refresh", 0, "", "");
        let (mut ctx, _home, calls) = ctx_and_runner(runner).await;
        install_owned_tools(&ctx).await;
        // `gh auth refresh` is an interactive sign-in, and `run_gh_auth`
        // refuses to start one with nobody to answer it. `ctx_with` models an
        // unattended machine, so this test says explicitly that a developer is
        // here — otherwise it would assert the refusal, not the repair.
        ctx.ui = Ui::new(true).assume_prompts_work(true);

        // The refresh runs, and because the stub still reports 403 afterwards,
        // apply() reports that rather than claiming success.
        let error = GithubCli.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("membership"), "{error}");
        assert!(
            calls
                .calls()
                .iter()
                .any(|call| call.contains("auth refresh")),
            "{:?}",
            calls.calls()
        );
        // The device-code flow prints through riabuild rather than over it.
        // Every other `gh` in this apply() is captured, so it is the only one
        // that asks for a pty.
        let subdued = calls.subdued_calls();
        assert_eq!(subdued.len(), 1, "{subdued:?}");
        assert!(subdued[0].contains("auth refresh"), "{subdued:?}");
    }

    #[tokio::test]
    async fn a_cancelled_sign_in_is_not_treated_as_success() {
        // gh exits non-zero when the developer abandons the device-code prompt.
        let runner = healthy()
            .with("gh auth status", 1, "", "You are not logged into any hosts")
            .with("gh auth login", 1, "", "");
        let (mut ctx, _home) = ctx_with_tools(runner).await;
        let error = GithubCli.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("signing you in"), "{error}");
    }

    #[tokio::test]
    async fn an_unattended_run_never_waits_on_a_browser_sign_in() {
        // The bug this covers hung `riabuild remote` forever. `gh auth login
        // --web` prints a device code and waits for a person; handed no
        // terminal it waits with no output and no timeout, so the container
        // test sat there until something outside killed it. Failing with the
        // remedy is the only useful thing riabuild can do here.
        let runner = healthy()
            .with("gh auth status", 1, "", "You are not logged into any hosts")
            .with("gh auth login", 0, "", "");
        let (mut ctx, _home, calls) = ctx_and_runner(runner).await;
        // The owned gh has to be on disk, or `apply()` stops at the install
        // step and this never reaches the sign-in it is about.
        install_owned_tools(&ctx).await;
        ctx.ui = Ui::new(true).assume_prompts_work(false);

        let error = GithubCli.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("GH_TOKEN"), "{error}");
        assert!(
            !calls.calls().iter().any(|call| call.contains("auth login")),
            "gh must never be started at all: reaching it is the hang. {:?}",
            calls.calls()
        );
    }

    #[tokio::test]
    async fn a_token_that_already_works_needs_no_terminal() {
        // The guard must not turn every unattended run into a failure. A
        // GH_TOKEN that already answers the membership question never reaches
        // a prompt, so it must still succeed with no terminal at all.
        let (mut ctx, _home) = ctx_with_tools(healthy()).await;
        ctx.ui = Ui::new(true).assume_prompts_work(false);
        GithubCli.apply(&mut ctx).await.unwrap();
    }

    #[tokio::test]
    async fn a_pending_invite_names_the_invite_rather_than_the_token() {
        let runner = healthy().with(MEMBERSHIP, 0, r#"{"state":"pending"}"#, "");
        let (mut ctx, _home) = ctx_with_tools(runner).await;
        let error = GithubCli.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("Accept the Clubria invite"), "{error}");
    }

    #[tokio::test]
    async fn every_gh_call_goes_to_the_copy_riabuild_owns() {
        // The point of owning gh: what riabuild verified and what riabuild runs
        // are the same binary. Calling the bare name would resolve through PATH
        // to whatever the developer happens to have — and during provisioning
        // ~/.riabuild/bin is not on PATH, so it would usually miss the owned
        // copy even when one is installed.
        let (mut ctx, _home, runner) = ctx_and_runner(healthy()).await;
        install_owned_tools(&ctx).await;
        GithubCli.apply(&mut ctx).await.unwrap();

        let owned = ctx.gh();
        let gh_calls: Vec<String> = runner
            .calls()
            .into_iter()
            .filter(|call| call.contains("gh"))
            .collect();
        assert!(!gh_calls.is_empty());
        for call in &gh_calls {
            assert!(call.starts_with(&owned), "ran `{call}`, not {owned}");
        }
    }
}

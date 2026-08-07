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

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        if ctx.runner.which("gh").is_none() {
            return Ok(Status::needs("gh is not installed"));
        }

        let version_output = ctx
            .runner
            .run("gh", &["--version"], &RunOptions::default())
            .await?;
        if !version::at_least(version_output.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!("gh is older than {MIN_VERSION}")));
        }

        if !ctx
            .runner
            .run("gh", &["auth", "status"], &RunOptions::default())
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
        if ctx.runner.which("gh").is_none() {
            install(ctx).await?;
        }

        if !ctx
            .runner
            .run("gh", &["auth", "status"], &RunOptions::default())
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

async fn install(ctx: &mut Ctx) -> Result<()> {
    if ctx.runner.which("brew").is_none() {
        return Err(Failure::new(
            "installing the GitHub CLI",
            "Install Homebrew from https://brew.sh, then run `riabuild` again.",
        )
        .detail("riabuild installs tools with Homebrew and could not find `brew`")
        .into());
    }
    ctx.ui.note("Installing gh with Homebrew…");
    let output = ctx
        .runner
        .run("brew", &["install", "gh"], &RunOptions::default())
        .await?;
    if !output.ok() {
        return Err(Failure::new(
            "installing the GitHub CLI",
            "Run `brew install gh` yourself and read what it says, then run `riabuild` again.",
        )
        .command("brew install gh")
        .detail(output.stderr)
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_and_runner, ctx_with};
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
        let (ctx, _home) = ctx_with(runner).await;
        format!("{:?}", GithubCli.check(&ctx).await.unwrap())
    }

    #[tokio::test]
    async fn a_healthy_machine_is_satisfied() {
        let (ctx, _home) = ctx_with(healthy()).await;
        assert_eq!(GithubCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_missing_gh_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            GithubCli.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn an_old_gh_is_detected() {
        let runner = healthy().with("gh --version", 0, "gh version 2.10.0 (2023-01-01)", "");
        assert!(reason(runner).await.contains("older"));
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
        let (ctx, _home) = ctx_with(runner).await;
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
    }

    #[tokio::test]
    async fn a_cancelled_sign_in_is_not_treated_as_success() {
        // gh exits non-zero when the developer abandons the device-code prompt.
        let runner = healthy()
            .with("gh auth status", 1, "", "You are not logged into any hosts")
            .with("gh auth login", 1, "", "");
        let (mut ctx, _home) = ctx_with(runner).await;
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
        let (mut ctx, _home) = ctx_with(healthy()).await;
        ctx.ui = Ui::new(true).assume_prompts_work(false);
        GithubCli.apply(&mut ctx).await.unwrap();
    }

    #[tokio::test]
    async fn a_pending_invite_names_the_invite_rather_than_the_token() {
        let runner = healthy().with(MEMBERSHIP, 0, r#"{"state":"pending"}"#, "");
        let (mut ctx, _home) = ctx_with(runner).await;
        let error = GithubCli.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("Accept the Clubria invite"), "{error}");
    }
}

//! Task 2 — `gh`, installed, signed in, and able to see this developer's
//! Clubria membership.
//!
//! Ways this is wrong on a machine that worked last month: `gh` uninstalled,
//! `gh` logged out, a token that expired or was revoked, a token that cannot
//! read org membership, an invite never accepted, and a developer removed from
//! the org. Each one has a different remedy, so each one is detected
//! separately.

use super::{Ctx, Status, Task, TaskId};
use crate::runner::RunOptions;
use crate::shims;
use crate::tools;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use async_trait::async_trait;

const MIN_VERSION: &str = "2.40.0";
const ORG: &str = "Clubria";

/// The scope riabuild *requests* when a token cannot read org membership.
///
/// Deliberately never the scope riabuild *tests for*. GitHub accepts
/// `admin:org`, `read:org`, `repo`, `user`, and `write:org` on the membership
/// endpoint, and folds `read:org` into `admin:org` when both are granted. See
/// `membership`.
const ORG_SCOPE: &str = "read:org";

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

/// What GitHub says when asked whether this developer is in the org.
///
/// One variant per remedy: anything that collapses two of these together ends
/// up telling a developer to do something that cannot help.
#[derive(Debug, PartialEq, Eq)]
enum Membership {
    Active,
    /// Invited, but the invite has not been accepted.
    Pending,
    /// GitHub answered, and the answer is no.
    NotAMember,
    /// The token is gone — expired, revoked, or signed out from under us.
    SignedOut,
    /// The token is valid but may not read organisation membership.
    Forbidden,
    /// Rate limit, outage, captive portal, corporate proxy.
    Unreadable(String),
}

impl Membership {
    fn describe(&self) -> String {
        match self {
            Membership::Active => format!("you are an active member of {ORG}"),
            Membership::Pending => format!("your {ORG} invite has not been accepted yet"),
            Membership::NotAMember => {
                format!("GitHub does not report you as a member of {ORG}")
            }
            Membership::SignedOut => "your GitHub sign-in is no longer valid".into(),
            Membership::Forbidden => {
                format!("your GitHub token may not read {ORG} membership")
            }
            Membership::Unreadable(why) => {
                format!("could not check your {ORG} membership: {why}")
            }
        }
    }
}

/// Asks GitHub the only question this task actually cares about.
///
/// This replaced a test for the literal string `read:org` in `gh auth status`,
/// which asked a different question and got it wrong in both directions.
/// GitHub accepts `admin:org`, `read:org`, `repo`, `user`, or `write:org` here,
/// and folds `read:org` into `admin:org` when both are granted — so a developer
/// holding `admin:org` was told they lacked permission, sent through a browser
/// sign-in that could not add a scope they already had, and told to try again.
/// Forever: no run of `gh auth refresh` can make that string appear.
async fn membership(ctx: &Ctx) -> Result<Membership> {
    let output = ctx
        .runner
        .run(
            &ctx.gh(),
            &["api", &format!("/user/memberships/orgs/{ORG}")],
            &RunOptions::default(),
        )
        .await?;

    if output.ok() {
        // Tolerant of pretty-printed bodies: `gh api` emits compact JSON today,
        // and a formatting change should not read as "not a member".
        let body: String = output
            .stdout
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if body.contains(r#""state":"active""#) {
            return Ok(Membership::Active);
        }
        if body.contains(r#""state":"pending""#) {
            return Ok(Membership::Pending);
        }
        return Ok(Membership::Unreadable(
            "GitHub replied without a membership state".into(),
        ));
    }

    Ok(match http_status(&output.stderr) {
        Some(401) => Membership::SignedOut,
        Some(403) => Membership::Forbidden,
        // GitHub returns 404 rather than 403 when there is simply no
        // membership to report. The `NotAMember` remedy names the scope case
        // too, because this endpoint is the only evidence available here.
        Some(404) => Membership::NotAMember,
        _ => Membership::Unreadable(first_line(&output.stderr)),
    })
}

/// `gh` reports a failed API call as `gh: Not Found (HTTP 404)` on stderr.
fn http_status(stderr: &str) -> Option<u16> {
    stderr
        .split("(HTTP ")
        .nth(1)?
        .split(')')
        .next()?
        .trim()
        .parse()
        .ok()
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("gh gave no explanation")
        .to_string()
}

async fn sign_in(ctx: &mut Ctx) -> Result<()> {
    // Interactive on purpose: this is a browser sign-in, and there is no
    // non-interactive path that does not involve pasting a token.
    ctx.ui.note("Opening GitHub to sign you in…");
    run_gh_auth(
        ctx,
        &[
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--git-protocol",
            "https",
            "--web",
            "--scopes",
            ORG_SCOPE,
        ],
        "signing you in to GitHub",
    )
    .await
}

/// Runs an interactive `gh auth` command and insists that it worked.
///
/// The exit code used to be discarded. Cancelling the device-code prompt left
/// riabuild convinced it had signed the developer in, and the only symptom was
/// a later check failing for a reason that did not mention the sign-in.
async fn run_gh_auth(ctx: &mut Ctx, args: &[&str], attempting: impl Into<String>) -> Result<()> {
    let code = ctx
        .runner
        .run_interactive(&ctx.gh(), args, &RunOptions::default())
        .await?;
    if code != 0 {
        return Err(Failure::new(
            attempting,
            "Run `riabuild` again and finish the GitHub sign-in in your browser.",
        )
        .command(format!("gh {}", args.join(" ")))
        .detail(format!("that command exited with status {code}"))
        .into());
    }
    Ok(())
}

/// Downloads the pinned `gh` into `~/.riabuild/gh/<version>/`.
///
/// This used to be `brew install gh`, which meant riabuild could not set up a
/// machine without Homebrew on it and had nothing to offer on Linux at all.
async fn install(ctx: &mut Ctx) -> Result<()> {
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
        let (mut ctx, _home) = ctx_with_tools(runner).await;
        let error = GithubCli.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("signing you in"), "{error}");
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

    #[test]
    fn an_http_status_is_read_out_of_ghs_message() {
        assert_eq!(http_status("gh: Not Found (HTTP 404)"), Some(404));
        assert_eq!(http_status("gh: Forbidden (HTTP 403)\n"), Some(403));
        assert_eq!(http_status("dial tcp: no such host"), None);
    }
}

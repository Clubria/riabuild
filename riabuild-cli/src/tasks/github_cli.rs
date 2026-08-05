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
use crate::ui::Failure;
use crate::version;
use anyhow::Result;

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

    fn check(&self, ctx: &Ctx) -> Result<Status> {
        if ctx.runner.which("gh").is_none() {
            return Ok(Status::needs("gh is not installed"));
        }

        let version_output = ctx
            .runner
            .run("gh", &["--version"], &RunOptions::default())?;
        if !version::at_least(version_output.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!("gh is older than {MIN_VERSION}")));
        }

        if !ctx
            .runner
            .run("gh", &["auth", "status"], &RunOptions::default())?
            .ok()
        {
            return Ok(Status::needs("gh is not signed in to GitHub"));
        }

        Ok(match membership(ctx)? {
            Membership::Active => Status::Satisfied,
            other => Status::needs(other.describe()),
        })
    }

    fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if ctx.runner.which("gh").is_none() {
            install(ctx)?;
        }

        if !ctx
            .runner
            .run("gh", &["auth", "status"], &RunOptions::default())?
            .ok()
        {
            sign_in(ctx)?;
        }

        // Ask GitHub the question before asking the developer for anything.
        // Most tokens can already answer it — `gh auth login` grants `repo` by
        // default, which GitHub accepts here — so a browser round trip is the
        // exception, not the routine.
        let mut state = membership(ctx)?;
        match state {
            // The token itself is no longer valid; refreshing scopes on a dead
            // token cannot work, so sign in from scratch.
            Membership::SignedOut => {
                sign_in(ctx)?;
                state = membership(ctx)?;
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
                )?;
                state = membership(ctx)?;
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
fn membership(ctx: &Ctx) -> Result<Membership> {
    let output = ctx.runner.run(
        "gh",
        &["api", &format!("/user/memberships/orgs/{ORG}")],
        &RunOptions::default(),
    )?;

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

fn sign_in(ctx: &mut Ctx) -> Result<()> {
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
}

/// Runs an interactive `gh auth` command and insists that it worked.
///
/// The exit code used to be discarded. Cancelling the device-code prompt left
/// riabuild convinced it had signed the developer in, and the only symptom was
/// a later check failing for a reason that did not mention the sign-in.
fn run_gh_auth(ctx: &mut Ctx, args: &[&str], attempting: impl Into<String>) -> Result<()> {
    let code = ctx
        .runner
        .run_interactive("gh", args, &RunOptions::default())?;
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

fn install(ctx: &mut Ctx) -> Result<()> {
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
        .run("brew", &["install", "gh"], &RunOptions::default())?;
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

    fn reason(runner: FakeRunner) -> String {
        let (ctx, _home) = ctx_with(runner);
        format!("{:?}", GithubCli.check(&ctx).unwrap())
    }

    #[test]
    fn a_healthy_machine_is_satisfied() {
        let (ctx, _home) = ctx_with(healthy());
        assert_eq!(GithubCli.check(&ctx).unwrap(), Status::Satisfied);
    }

    #[test]
    fn a_missing_gh_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new());
        assert!(matches!(GithubCli.check(&ctx).unwrap(), Status::Needs(_)));
    }

    #[test]
    fn an_old_gh_is_detected() {
        let runner = healthy().with("gh --version", 0, "gh version 2.10.0 (2023-01-01)", "");
        assert!(reason(runner).contains("older"));
    }

    #[test]
    fn a_logged_out_gh_is_detected() {
        let runner = healthy().with("gh auth status", 1, "", "You are not logged into any hosts");
        assert!(reason(runner).contains("not signed in"));
    }

    #[test]
    fn an_admin_org_token_can_read_membership() {
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
        let (ctx, _home) = ctx_with(runner);
        assert_eq!(GithubCli.check(&ctx).unwrap(), Status::Satisfied);
    }

    #[test]
    fn a_token_that_cannot_read_membership_is_detected() {
        let runner = healthy().with(MEMBERSHIP, 1, "", "gh: Forbidden (HTTP 403)");
        assert!(reason(runner).contains("may not read"));
    }

    #[test]
    fn an_expired_token_is_told_apart_from_a_missing_permission() {
        // Different remedy: refreshing scopes on a dead token cannot work.
        let runner = healthy().with(MEMBERSHIP, 1, "", "gh: Bad credentials (HTTP 401)");
        assert!(reason(runner).contains("no longer valid"));
    }

    #[test]
    fn a_pending_invite_is_not_membership() {
        let runner = healthy().with(MEMBERSHIP, 0, r#"{"state":"pending"}"#, "");
        assert!(reason(runner).contains("not been accepted"));
    }

    #[test]
    fn being_removed_from_the_org_is_detected() {
        let runner = healthy().with(MEMBERSHIP, 1, "", "gh: Not Found (HTTP 404)");
        assert!(reason(runner).contains("not report you as a member"));
    }

    #[test]
    fn an_unreachable_github_is_not_mistaken_for_a_rejection() {
        // A captive portal must not read as "you were removed from the org".
        let runner = healthy().with(MEMBERSHIP, 1, "", "dial tcp: lookup api.github.com");
        let described = reason(runner);
        assert!(described.contains("could not check"), "{described}");
        assert!(
            !described.contains("not report you as a member"),
            "{described}"
        );
    }

    #[test]
    fn a_working_token_is_never_sent_through_a_browser() {
        // The regression that matters most: apply() must not demand a sign-in
        // from a developer whose token already answers the question.
        let (mut ctx, _home, runner) = ctx_and_runner(healthy());
        GithubCli.apply(&mut ctx).unwrap();
        let calls = runner.calls();
        assert!(
            !calls
                .iter()
                .any(|call| call.contains("auth login") || call.contains("auth refresh")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_missing_permission_is_repaired_before_giving_up() {
        let runner = healthy()
            .with(MEMBERSHIP, 1, "", "gh: Forbidden (HTTP 403)")
            .with("gh auth refresh", 0, "", "");
        let (mut ctx, _home, calls) = ctx_and_runner(runner);

        // The refresh runs, and because the stub still reports 403 afterwards,
        // apply() reports that rather than claiming success.
        let error = GithubCli.apply(&mut ctx).unwrap_err().to_string();
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

    #[test]
    fn a_cancelled_sign_in_is_not_treated_as_success() {
        // gh exits non-zero when the developer abandons the device-code prompt.
        let runner = healthy()
            .with("gh auth status", 1, "", "You are not logged into any hosts")
            .with("gh auth login", 1, "", "");
        let (mut ctx, _home) = ctx_with(runner);
        let error = GithubCli.apply(&mut ctx).unwrap_err().to_string();
        assert!(error.contains("signing you in"), "{error}");
    }

    #[test]
    fn a_pending_invite_names_the_invite_rather_than_the_token() {
        let runner = healthy().with(MEMBERSHIP, 0, r#"{"state":"pending"}"#, "");
        let (mut ctx, _home) = ctx_with(runner);
        let error = GithubCli.apply(&mut ctx).unwrap_err().to_string();
        assert!(error.contains("Accept the Clubria invite"), "{error}");
    }

    #[test]
    fn an_http_status_is_read_out_of_ghs_message() {
        assert_eq!(http_status("gh: Not Found (HTTP 404)"), Some(404));
        assert_eq!(http_status("gh: Forbidden (HTTP 403)\n"), Some(403));
        assert_eq!(http_status("dial tcp: no such host"), None);
    }
}

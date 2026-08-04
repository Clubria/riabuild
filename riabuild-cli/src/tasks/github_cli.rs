//! Task 2 — `gh`, authenticated, with `read:org`, and in the Clubria org.
//!
//! Four separate ways this can be wrong on a machine that worked last month:
//! `gh` uninstalled, `gh` logged out, a token that lost `read:org` after a
//! re-login, and a developer removed from the org.

use super::{Ctx, Status, Task, TaskId};
use crate::runner::RunOptions;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;

const MIN_VERSION: &str = "2.40.0";
const ORG: &str = "Clubria";

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

        // `gh auth status` writes its report to stderr on some versions and
        // stdout on others, so both are searched.
        let status = ctx
            .runner
            .run("gh", &["auth", "status"], &RunOptions::default())?;
        if !status.ok() {
            return Ok(Status::needs("gh is not signed in to GitHub"));
        }
        let report = format!("{}{}", status.stdout, status.stderr);
        if !report.contains("read:org") {
            return Ok(Status::needs(
                "your GitHub token is missing the read:org permission",
            ));
        }

        let membership = ctx.runner.run(
            "gh",
            &["api", &format!("/user/memberships/orgs/{ORG}")],
            &RunOptions::default(),
        )?;
        if !membership.ok() {
            return Ok(Status::needs(format!(
                "GitHub does not report you as a member of {ORG}"
            )));
        }
        if !membership.stdout.contains("\"state\":\"active\"") {
            return Ok(Status::needs(format!(
                "your {ORG} membership is not active yet"
            )));
        }

        Ok(Status::Satisfied)
    }

    fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if ctx.runner.which("gh").is_none() {
            install(ctx)?;
        }

        let status = ctx
            .runner
            .run("gh", &["auth", "status"], &RunOptions::default())?;
        let report = format!("{}{}", status.stdout, status.stderr);

        if !status.ok() {
            // Interactive on purpose: this is a browser sign-in, and there is no
            // non-interactive path that does not involve pasting a token.
            ctx.ui.note("Opening GitHub to sign you in…");
            ctx.runner.run_interactive(
                "gh",
                &[
                    "auth",
                    "login",
                    "--hostname",
                    "github.com",
                    "--git-protocol",
                    "https",
                    "--web",
                    "--scopes",
                    "read:org",
                ],
                &RunOptions::default(),
            )?;
        } else if !report.contains("read:org") {
            ctx.ui
                .note("Adding the read:org permission to your GitHub token…");
            ctx.runner.run_interactive(
                "gh",
                &[
                    "auth",
                    "refresh",
                    "--hostname",
                    "github.com",
                    "--scopes",
                    "read:org",
                ],
                &RunOptions::default(),
            )?;
        }

        // Membership is GitHub's to grant. If it is still missing after signing
        // in, riabuild cannot fix it and says who can.
        let membership = ctx.runner.run(
            "gh",
            &["api", &format!("/user/memberships/orgs/{ORG}")],
            &RunOptions::default(),
        )?;
        if !membership.ok() {
            return Err(Failure::new(
                format!("checking your {ORG} GitHub membership"),
                format!("Ask your team lead to invite you to the {ORG} GitHub organisation, then accept the invite."),
            )
            .command(format!("gh api /user/memberships/orgs/{ORG}"))
            .detail(membership.stderr)
            .into());
        }

        Ok(())
    }
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
    use crate::testing::ctx_with;

    fn healthy() -> FakeRunner {
        FakeRunner::new()
            .with("gh --version", 0, "gh version 2.96.0 (2026-07-02)", "")
            .with(
                "gh auth status",
                0,
                "",
                "github.com\n  ✓ Logged in to github.com account ada\n  - Token scopes: 'gist', 'read:org', 'repo'",
            )
            .with(
                "gh api /user/memberships/orgs/Clubria",
                0,
                r#"{"state":"active","role":"member"}"#,
                "",
            )
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
        let (ctx, _home) = ctx_with(runner);
        let status = GithubCli.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("older"), "{status:?}");
    }

    #[test]
    fn a_logged_out_gh_is_detected() {
        let runner = healthy().with("gh auth status", 1, "", "You are not logged into any hosts");
        let (ctx, _home) = ctx_with(runner);
        let status = GithubCli.check(&ctx).unwrap();
        assert!(
            format!("{status:?}").contains("not signed in"),
            "{status:?}"
        );
    }

    #[test]
    fn a_token_that_lost_read_org_is_detected() {
        // The subtle one: gh is installed, current, and logged in — and still
        // cannot answer the question riabuild needs answered.
        let runner = healthy().with(
            "gh auth status",
            0,
            "",
            "github.com\n  ✓ Logged in to github.com account ada\n  - Token scopes: 'gist', 'repo'",
        );
        let (ctx, _home) = ctx_with(runner);
        let status = GithubCli.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("read:org"), "{status:?}");
    }

    #[test]
    fn a_pending_invite_is_not_membership() {
        let runner = healthy().with(
            "gh api /user/memberships/orgs/Clubria",
            0,
            r#"{"state":"pending","role":"member"}"#,
            "",
        );
        let (ctx, _home) = ctx_with(runner);
        let status = GithubCli.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("not active"), "{status:?}");
    }

    #[test]
    fn being_removed_from_the_org_is_detected() {
        let runner = healthy().with("gh api /user/memberships/orgs/Clubria", 1, "", "HTTP 404");
        let (ctx, _home) = ctx_with(runner);
        let status = GithubCli.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("member"), "{status:?}");
    }
}

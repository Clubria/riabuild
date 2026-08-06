//! Task 3 — the Infisical CLI.
//!
//! **No token is installed.** Credentials are brokered per use by riabuild-web
//! and piped into `infisical export`. A long-lived Infisical credential on a
//! laptop is exactly what the brokering design exists to avoid.

use super::{Ctx, Status, Task, TaskId};
use crate::runner::RunOptions;
use crate::shims;
use crate::tools;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use async_trait::async_trait;

const MIN_VERSION: &str = "0.30.0";

pub struct InfisicalCli;

#[async_trait]
impl Task for InfisicalCli {
    fn id(&self) -> TaskId {
        "infisical_cli"
    }

    fn title(&self) -> &str {
        "Infisical CLI"
    }

    /// Bumped to 2 when riabuild took ownership of `infisical` instead of
    /// installing it with Homebrew.
    fn version(&self) -> u32 {
        2
    }

    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let infisical = ctx.infisical();
        if !tokio::fs::try_exists(&infisical).await.unwrap_or(false) {
            return Ok(Status::needs(format!(
                "riabuild has not installed infisical {} yet",
                tools::INFISICAL_VERSION
            )));
        }
        let output = ctx
            .runner
            .run(&infisical, &["--version"], &RunOptions::default())
            .await?;
        // Infisical prints its version banner on stderr in some builds, so both
        // streams are searched.
        let reported = format!("{}{}", output.stdout, output.stderr);
        if !version::at_least(&reported, MIN_VERSION) {
            return Ok(Status::needs(format!(
                "the infisical in ~/.riabuild reports `{}`, which is not usable",
                reported.trim()
            )));
        }
        Ok(Status::Satisfied)
    }

    /// Downloads the pinned `infisical` into `~/.riabuild/infisical/<version>/`.
    ///
    /// Still installs **no token**. Credentials are brokered per use and piped
    /// into `infisical export`; owning the binary changes nothing about that.
    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let release = tools::infisical()?;
        ctx.ui
            .note(&format!("Downloading infisical {}…", release.version));

        let tool_dir = ctx.paths.tool_dir(release.tool, release.version);
        tools::install(&release, &tool_dir).await.map_err(|error| {
            Failure::new(
                "installing the Infisical CLI",
                "Check your network connection and run `riabuild` again. If it keeps \
                 failing, send this to your team lead.",
            )
            .detail(format!("{error:#}"))
        })?;

        shims::write_tool(ctx, "infisical", &release.binary_in(&tool_dir)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_and_runner, ctx_with, ctx_with_tools, install_owned_tools};

    fn reporting(version: &str) -> FakeRunner {
        FakeRunner::new().with("infisical --version", 0, version, "")
    }

    #[tokio::test]
    async fn a_current_infisical_is_satisfied() {
        let (ctx, _home) = ctx_with_tools(reporting("Infisical CLI v0.43.120")).await;
        assert_eq!(InfisicalCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_version_banner_on_stderr_is_still_read() {
        // Some builds print it there; searching only stdout reads as "not
        // usable" on a perfectly good install.
        let runner =
            FakeRunner::new().with("infisical --version", 0, "", "Infisical CLI v0.43.120");
        let (ctx, _home) = ctx_with_tools(runner).await;
        assert_eq!(InfisicalCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_corrupted_install_is_detected() {
        // The owned copy is a known version, so a low one means a truncated or
        // half-written download rather than an old release.
        let (ctx, _home) = ctx_with_tools(reporting("Infisical CLI v0.12.0")).await;
        let status = InfisicalCli.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not usable"), "{status:?}");
    }

    #[tokio::test]
    async fn an_infisical_riabuild_has_not_installed_is_detected() {
        // Including a system infisical on PATH: it is not the binary riabuild
        // verified, so it does not count.
        let (ctx, _home) = ctx_with(reporting("Infisical CLI v0.43.120")).await;
        let status = InfisicalCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("has not installed infisical"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn the_check_never_looks_for_a_stored_credential() {
        // Guards the design rule: presence of a token is not part of "healthy",
        // because riabuild never installs one. Owning the binary changed where
        // infisical comes from, not that rule.
        let (ctx, _home, runner) = ctx_and_runner(reporting("Infisical CLI v0.43.120")).await;
        install_owned_tools(&ctx).await;
        InfisicalCli.check(&ctx).await.unwrap();
        let calls = runner.calls();
        assert!(
            !calls.iter().any(|call| call.contains("login")),
            "{calls:?}"
        );
    }
}

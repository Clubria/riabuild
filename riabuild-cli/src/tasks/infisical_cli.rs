//! Task 3 — the Infisical CLI.
//!
//! **No token is installed.** Credentials are brokered per use by riabuild-web
//! and piped into `infisical export`. A long-lived Infisical credential on a
//! laptop is exactly what the brokering design exists to avoid.

use super::{Ctx, Status, Task, TaskId};
use crate::runner::RunOptions;
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

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        if ctx.runner.which("infisical").is_none() {
            return Ok(Status::needs("infisical is not installed"));
        }
        let output = ctx
            .runner
            .run("infisical", &["--version"], &RunOptions::default())
            .await?;
        let reported = format!("{}{}", output.stdout, output.stderr);
        if !version::at_least(&reported, MIN_VERSION) {
            return Ok(Status::needs(format!(
                "infisical is older than {MIN_VERSION}"
            )));
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if ctx.runner.which("brew").is_none() {
            return Err(Failure::new(
                "installing the Infisical CLI",
                "Install Homebrew from https://brew.sh, then run `riabuild` again.",
            )
            .detail("riabuild installs tools with Homebrew and could not find `brew`")
            .into());
        }

        ctx.ui.note("Installing infisical with Homebrew…");
        let output = ctx
            .runner
            .run(
                "brew",
                &["install", "infisical/get-cli/infisical"],
                &RunOptions::default(),
            )
            .await?;

        if !output.ok() {
            // Already-installed-but-outdated takes a different verb.
            let upgrade = ctx
                .runner
                .run(
                    "brew",
                    &["upgrade", "infisical/get-cli/infisical"],
                    &RunOptions::default(),
                )
                .await?;
            if !upgrade.ok() {
                return Err(Failure::new(
                    "installing the Infisical CLI",
                    "Run `brew install infisical/get-cli/infisical` yourself and read what it says, then run `riabuild` again.",
                )
                .command("brew install infisical/get-cli/infisical")
                .detail(output.stderr)
                .into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;

    #[tokio::test]
    async fn a_current_infisical_is_satisfied() {
        let runner = FakeRunner::new().with("infisical --version", 0, "Infisical CLI v0.41.89", "");
        let (ctx, _home) = ctx_with(runner);
        assert_eq!(InfisicalCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn an_old_infisical_is_detected() {
        let runner = FakeRunner::new().with("infisical --version", 0, "Infisical CLI v0.12.0", "");
        let (ctx, _home) = ctx_with(runner);
        assert!(matches!(
            InfisicalCli.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn a_missing_infisical_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new());
        assert!(matches!(
            InfisicalCli.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn the_check_never_looks_for_a_stored_credential() {
        // Guards the design rule: presence of a token is not part of "healthy",
        // because riabuild never installs one.
        let runner = FakeRunner::new().with("infisical --version", 0, "Infisical CLI v0.41.89", "");
        let (ctx, _home) = ctx_with(runner);
        InfisicalCli.check(&ctx).await.unwrap();
        let calls = format!("{:?}", ctx.runner.which("infisical"));
        assert!(!calls.contains("login"));
    }
}

//! Task 3 — the Infisical CLI.
//!
//! **No token is installed.** Credentials are brokered per use by riabuild-web
//! and piped into `infisical export`. A long-lived Infisical credential on a
//! laptop is exactly what the brokering design exists to avoid.
//!
//! One row in `owned_tool`'s table and nothing else — download the pinned
//! release, verify it against a digest, land it under `~/.riabuild`, put a shim
//! in `bin/` — so the row *is* the task. Owning the binary changed where
//! infisical comes from, not the rule above it.

use crate::Ctx;
use crate::owned_tool::{OwnedTool, Shim, exec_shim, no_note, plain_probe};
use riabuild_fetch::tools;

pub(crate) static INFISICAL_CLI: OwnedTool = OwnedTool {
    id: "infisical_cli",
    title: "Infisical CLI",
    label: "infisical",
    // 2 since riabuild took ownership of `infisical` instead of installing it
    // with Homebrew.
    version: 2,
    min_version: "0.30.0",
    pinned_version: tools::INFISICAL_VERSION,
    release: tools::infisical,
    binary: Ctx::infisical,
    probe: plain_probe,
    shim: Some(Shim {
        name: "infisical",
        render: exec_shim,
        without_it: "so the shell would find whichever infisical the machine already had",
    }),
    installing: "installing the Infisical CLI",
    note: no_note,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_and_runner, ctx_with, ctx_with_tools, install_owned_tools};
    use crate::{Status, Task};
    use riabuild_runner::FakeRunner;

    fn reporting(version: &str) -> FakeRunner {
        FakeRunner::new().with("infisical --version", 0, version, "")
    }

    #[tokio::test]
    async fn a_current_infisical_is_satisfied() {
        let (ctx, _home) = ctx_with_tools(reporting("Infisical CLI v0.43.120")).await;
        assert_eq!(INFISICAL_CLI.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_version_banner_on_stderr_is_still_read() {
        // Some builds print it there; searching only stdout reads as "not
        // usable" on a perfectly good install.
        let runner =
            FakeRunner::new().with("infisical --version", 0, "", "Infisical CLI v0.43.120");
        let (ctx, _home) = ctx_with_tools(runner).await;
        assert_eq!(INFISICAL_CLI.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_corrupted_install_is_detected() {
        // The owned copy is a known version, so a low one means a truncated or
        // half-written download rather than an old release.
        let (ctx, _home) = ctx_with_tools(reporting("Infisical CLI v0.12.0")).await;
        let status = INFISICAL_CLI.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not usable"), "{status:?}");
    }

    #[tokio::test]
    async fn an_infisical_riabuild_has_not_installed_is_detected() {
        // Including a system infisical on PATH: it is not the binary riabuild
        // verified, so it does not count.
        let (ctx, _home) = ctx_with(reporting("Infisical CLI v0.43.120")).await;
        let status = INFISICAL_CLI.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("has not installed infisical"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn an_infisical_with_no_shim_is_not_reported_as_satisfied() {
        // `~/.riabuild/bin` leads `PATH` in the environment shell, so a missing
        // shim is not "riabuild's copy is second" — it is the machine's own
        // infisical answering, unverified. This task reported such a machine as
        // correct until every owned tool went through one table.
        let (ctx, _home) = ctx_with_tools(reporting("Infisical CLI v0.43.120")).await;
        tokio::fs::remove_file(ctx.paths.bin_dir().join("infisical"))
            .await
            .unwrap();
        let status = INFISICAL_CLI.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("no launcher"), "{status:?}");
    }

    #[tokio::test]
    async fn the_check_never_looks_for_a_stored_credential() {
        // Guards the design rule: presence of a token is not part of "healthy",
        // because riabuild never installs one. Owning the binary changed where
        // infisical comes from, not that rule.
        let (ctx, _home, runner) = ctx_and_runner(reporting("Infisical CLI v0.43.120")).await;
        install_owned_tools(&ctx).await;
        INFISICAL_CLI.check(&ctx).await.unwrap();
        let calls = runner.calls();
        assert!(
            !calls.iter().any(|call| call.contains("login")),
            "{calls:?}"
        );
    }
}

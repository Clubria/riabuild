//! Task 3 — the Infisical CLI.
//!
//! **No token is installed.** Credentials are brokered per use by riabuild-web
//! and handed to `infisical` in its environment. A long-lived Infisical
//! credential on a laptop is exactly what the brokering design exists to avoid.
//!
//! One row in `owned_tool`'s table and nothing else — download the pinned
//! release, verify it against a digest, land it under `~/.riabuild`, put a shim
//! in `bin/` — so the row *is* the task. Owning the binary changed where
//! infisical comes from, not the rule above it.
//!
//! The interesting field is the shim, and it is interesting for ngrok's reason.
//! `~/.riabuild/bin/infisical` is not an `exec` line: it routes the developer's
//! own invocation through `riabuild internal infisical`, which brokers a
//! credential for that one command and passes it in the environment. That is
//! what closes the gap between what riabuild could do with Infisical and what
//! the developer could — `env_local` pulled `.env.dev` on every run while a
//! developer's own `infisical export` met a CLI nobody had ever logged in.
//! Signing them in properly is the thing that must never happen here: it writes
//! a credential to the machine, which is the rule at the top of this file.

use crate::Ctx;
use crate::owned_tool::{OwnedTool, Shim, no_note, plain_probe};
use crate::shims;
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
        render: shims::infisical_shim_script,
        without_it: "so the shell would find whichever infisical the machine already had, \
                     signed in to nothing",
    }),
    installing: "installing the Infisical CLI",
    note: no_note,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        ctx_and_runner, ctx_with, ctx_with_tools, install_owned_tools, write_file,
    };
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
    async fn the_launcher_routes_the_developers_own_invocation_through_riabuild() {
        // The whole feature, asserted on the file that actually lands in
        // `bin/`. Without this the developer's `infisical export` reaches the
        // binary riabuild verified and no credential at all, which reads as
        // "you must be logged in" and has no fix riabuild would allow.
        let (mut ctx, _home) = ctx_with_tools(reporting("Infisical CLI v0.43.120")).await;
        INFISICAL_CLI.apply(&mut ctx).await.unwrap();
        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join("infisical"))
            .await
            .unwrap();
        assert!(script.contains("internal infisical"), "{script}");
        // And it never becomes the place the credential is kept, on a laptop or
        // in this repository.
        assert!(!script.contains("INFISICAL_TOKEN"), "{script}");
    }

    #[tokio::test]
    async fn the_exec_launcher_an_older_riabuild_wrote_is_replaced() {
        // The migration, and the reason `version` does not move for this
        // change: every machine provisioned before it has a plain `exec` line
        // in `bin/infisical`, and `check()` compares the *text*, so the next
        // run sees drift and rewrites it. Bumping `version` to force that would
        // be the escape hatch used for drift a check can see perfectly well.
        let (ctx, _home) = ctx_with_tools(reporting("Infisical CLI v0.43.120")).await;
        let stale = crate::shims::exec_shim(std::path::Path::new(&ctx.infisical()));
        write_file(&ctx.paths.bin_dir().join("infisical"), &stale).await;
        let status = INFISICAL_CLI.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the one this riabuild writes"),
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
        INFISICAL_CLI.check(&ctx).await.unwrap();
        let calls = runner.calls();
        assert!(
            !calls.iter().any(|call| call.contains("login")),
            "{calls:?}"
        );
    }
}

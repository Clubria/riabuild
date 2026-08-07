//! `riabuild internal ...` — the hidden subcommands riabuild invokes on itself
//! over SSH. Not for people.
//!
//! Both concern the per-session GitHub credential on a server: `gh-sweep`
//! clears what a session that died without cleaning up left behind, and
//! `seed-github` takes the token the laptop pipes over and hands it to `gh`.
//! The marker mechanics they sit on are in `gh_session`; the laptop side
//! that invokes them is in `remote/`.

use crate::config;
use crate::gh_session;
use crate::runner::RunOptions;
use crate::scope;
use crate::tasks::Ctx;
use anyhow::Result;

pub(crate) async fn gh_sweep(ctx: &Ctx) -> Result<i32> {
    // Run by the laptop before seeding, so a dead session's leftovers
    // go before the new credential arrives rather than after.
    let runtime = gh_session::choose_runtime_dir(
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("TMPDIR").ok().as_deref(),
    )
    .await?;
    let dir =
        gh_session::GhSession::attach(&runtime, &scope::member_id_from_root(ctx.paths.as_ref())?)
            .await?;
    gh_session::sweep(&dir, ctx.runner.clone(), config::now_secs()).await?;
    Ok(0)
}

pub(crate) async fn seed_github(ctx: &Ctx) -> Result<i32> {
    // `tokio::io`, not `std::io`: a blocking read on the current-thread
    // runtime stalls every other future on it, which is the invariant in
    // riabuild-cli/CLAUDE.md.
    use tokio::io::AsyncReadExt;
    let mut token = String::new();
    tokio::io::stdin().read_to_string(&mut token).await?;
    accept_github_token(ctx, &token).await
}

/// The server half of `remote::seed::seed_github`: hands the GitHub token the
/// laptop piped over SSH on to `gh`, again on stdin.
///
/// The token reaches `gh` only on stdin — never in argv, because `ps` is
/// world-readable and on a shared server it shows every other developer's
/// command lines — and is never logged. `gh` writes its own `hosts.yml`, with
/// its own permissions, into the `GH_CONFIG_DIR` the scoped runner supplies;
/// riabuild never hand-writes that file.
///
/// Taking the token as an argument rather than reading stdin itself is what
/// makes that guarantee assertable: the caller above reads the *process's*
/// stdin, which under `cargo test` is the terminal, so a test driving the
/// subcommand end to end would block on EOF instead of asserting anything.
async fn accept_github_token(ctx: &Ctx, token: &str) -> Result<i32> {
    let output = ctx
        .runner
        .run(
            "gh",
            &["auth", "login", "--with-token"],
            &RunOptions {
                stdin: Some(token.trim().as_bytes().to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(if output.ok() { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_ctx;
    use crate::config::{State, UserConfig};
    use crate::keychain::{self, MemoryKeychain};
    use crate::paths::{Paths, RealPaths};
    use crate::runner::{CommandRunner, FakeRunner};
    use crate::ui::Ui;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Hands the `TempDir` back as well: dropping it deletes the tree the
    /// `Ctx`'s `Paths` point at, so a test that writes anything needs it alive
    /// for the duration. The caller keeps its own handle on the `FakeRunner`,
    /// because what these tests assert on is *what was run*, not the result.
    fn ctx_with_runner(scope: &scope::Scope, fake: Arc<FakeRunner>) -> (Ctx, TempDir) {
        let home = TempDir::new().expect("tempdir");
        let paths: Arc<dyn Paths> = Arc::new(RealPaths::rooted_at(home.path()));
        let runner: Arc<dyn CommandRunner> = fake;
        let keychain: Arc<dyn keychain::Keychain> = Arc::new(MemoryKeychain::default());
        let ctx = build_ctx(
            scope,
            paths,
            runner,
            keychain,
            Ui::new(true),
            UserConfig::default(),
            State::default(),
            false,
        );
        (ctx, home)
    }

    #[tokio::test]
    async fn the_github_token_reaches_gh_on_stdin_and_never_in_argv() {
        // On a shared server `ps` shows every developer's command lines, so a
        // token in argv is a token handed to everyone logged in. Both halves
        // are asserted deliberately: dropping `stdin:` from the call site
        // leaves argv clean, so an argv-only test stays green while `gh` is
        // handed an empty pipe; passing the token as an extra argument as well
        // would leave stdin correct, so a stdin-only test stays green while
        // `ps` leaks it.
        let fake = Arc::new(FakeRunner::new().with("gh auth login --with-token", 0, "", ""));
        let (ctx, _home) = ctx_with_runner(&scope::Scope::read(Some("build-01")), fake.clone());

        let token = "gho_averysecretgithubtoken";
        assert_eq!(
            accept_github_token(&ctx, &format!("{token}\n"))
                .await
                .expect("gh runs"),
            0
        );

        assert_eq!(
            fake.stdin_text_of("gh auth login").as_deref(),
            Some(token),
            "the token must arrive on stdin, trailing newline trimmed"
        );
        for call in fake.calls() {
            assert!(
                !call.contains(token),
                "the token must not appear in any argument list: {call}"
            );
        }
        assert_eq!(fake.calls(), vec!["gh auth login --with-token".to_string()]);
    }

    #[tokio::test]
    async fn a_gh_that_rejects_the_token_is_a_nonzero_exit() {
        // The failure has to travel back over SSH as an exit code — a seeding
        // run that reported success while `gh` refused the token would leave
        // the shell hop to discover it, with no credential and no explanation.
        let fake =
            Arc::new(FakeRunner::new().with("gh auth login --with-token", 1, "", "bad token"));
        let (ctx, _home) = ctx_with_runner(&scope::Scope::read(Some("build-01")), fake);
        assert_eq!(
            accept_github_token(&ctx, "gho_expired")
                .await
                .expect("gh runs"),
            1
        );
    }
}

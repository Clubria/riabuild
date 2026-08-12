//! Handing the terminal to `gh auth` — the sign-in, and the scope refresh that
//! shares its machinery.
//!
//! Both are browser round trips that block on a person, which makes them the
//! two places this task can hang rather than fail. So both go through one
//! function that refuses to start without a terminal and insists on the exit
//! code afterwards.

use super::ORG_SCOPE;
use crate::runner::RunOptions;
use crate::tasks::Ctx;
use crate::ui::Failure;
use anyhow::Result;

pub(super) async fn sign_in(ctx: &mut Ctx) -> Result<()> {
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
pub(super) async fn run_gh_auth(
    ctx: &mut Ctx,
    args: &[&str],
    attempting: impl Into<String>,
) -> Result<()> {
    // Both commands that reach here open a device-code flow and wait for a
    // person. With no terminal that wait never ends: `gh` does not time out,
    // so riabuild hangs with no output until something outside kills it. An
    // unattended run has to be told what to do instead, and there is a real
    // answer — `GH_TOKEN` needs no browser.
    //
    // This guard is not riabuild's single chokepoint for delegated prompts —
    // an earlier version of this comment, and R15 in `decisions.md`, both
    // said so and both were wrong. It is the only *unbounded* one. The other
    // `run_interactive` sites each have something that ends them:
    // `auth::login`'s browser wait is bounded at 180s, `shell::open` is
    // skipped by `--no-shell`, and `update.rs` re-execs riabuild itself.
    // `remote::authorise`'s `ssh-copy-id` is the real second delegated-prompt
    // site and is deliberately *not* guarded here: it typically errors rather
    // than hanging with no controlling terminal, which if true makes a guard
    // unnecessary — but nothing proves it, because `e2e/remote/run.sh`
    // sidesteps that path with an ssh-agent. Treat it as unproven, not as
    // covered.
    if !ctx.ui.interactive() {
        return Err(Failure::new(
            attempting,
            "Set GH_TOKEN to a GitHub token with the `read:org` permission, \
             or run `gh auth login` yourself at a terminal first.",
        )
        .command(format!("gh {}", args.join(" ")))
        .detail("this needs a browser sign-in and there is no terminal to prompt on")
        .into());
    }

    // The `gh` riabuild owns, by absolute path. `~/.riabuild/bin` is not on
    // `PATH` during provisioning, so the bare name would start an unverified
    // `gh` — or none at all — and this is the sign-in every later check rests
    // on. See `Ctx::gh`.
    let code = ctx
        .runner
        .run_interactive(
            &ctx.gh(),
            args,
            &RunOptions {
                // Both commands that reach here open a device-code flow, which
                // is text and a wait for a person — it survives line discipline
                // intact. `gh`'s arrow-key selection prompt would not, which is
                // why this is set here rather than on every `gh` invocation.
                subdued: Some(ctx.ui.theme()),
                ..Default::default()
            },
        )
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

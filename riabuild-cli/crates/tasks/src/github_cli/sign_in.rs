//! Handing the terminal to `gh auth` — the sign-in, and the scope refresh that
//! shares its machinery.
//!
//! Both are browser round trips that block on a person, which makes them the
//! two places this task can hang rather than fail. So both go through one
//! function that refuses to start without a terminal, settles the one question
//! `gh` would otherwise ask before it authenticates anything, and insists on the
//! exit code afterwards.

use super::ORG_SCOPE;
use crate::Ctx;
use crate::git_credentials::own_git_credentials;
use anyhow::Result;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;

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
    // `remote::authorise`'s key copy was the second delegated-prompt site,
    // and is no longer one at all: it is a captured `ssh` rather than a
    // `run_interactive`, and the prompt behind it is `ui::secret`, which
    // fails outright when there is no `/dev/tty` to ask on. Nothing is piped
    // to that child's stdin either, so `run` hands it `Stdio::null()` and
    // even an OpenSSH too old to honour `SSH_ASKPASS_REQUIRE` reads EOF and
    // exits rather than waiting for a person who is not there.
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

    // Before the terminal is handed over, not after: the question this settles
    // is asked *before* `gh` authenticates anything, and it cannot be answered
    // once it has been put.
    //
    // `gh` opens both of these commands with
    //
    // ```text
    // ? Authenticate Git with your GitHub credentials? (Y/n)
    // ```
    //
    // whenever the git protocol is https and the helper configured for the host
    // is not `gh` itself — `login_flow.go`'s `Interactive && gitProtocol ==
    // "https"`, then `GitCredentialFlow.Prompt`, which returns early when
    // `helper.IsOurs()`. `refresh.go` carries the same pair, which is why this
    // belongs here rather than in `sign_in` alone.
    //
    // riabuild cannot allow it to be asked, because under a subdued child it
    // cannot be answered. It is a `survey` prompt, and `survey` measures the
    // terminal by parking the cursor at `ESC[999;999f` and reading the reply to
    // `ESC[6n`. `subdue` drops both — a child does not get to move riabuild's
    // cursor — and riabuild answers no terminal query, so that read never
    // returns and every keystroke after it is swallowed by a parser waiting for
    // a cursor report that is not coming. The prompt is not slow to answer; it
    // is unanswerable. `riabuild remote` sat on that line, ignoring `y`, until
    // something outside killed it.
    //
    // So riabuild answers it in advance, which is where the answer belonged.
    // This is a decision riabuild owns: it clones with `gh repo clone` over
    // https and the developer pushes back over it, `gh`'s own default here is
    // yes, and a developer pressing Y has decided nothing riabuild was not
    // going to decide for them. Settling it first also earns the `workflow`
    // scope, which `gh` requests only once this question is — the scope a
    // `git push` touching `.github/workflows/` needs.
    //
    // A failure is loud rather than skipped. Carrying on would hand `gh` the
    // terminal with the prompt still to come, and this codebase would rather
    // present a hang as a red job than as a slow one.
    //
    // The command itself lives in `git_credentials`, the task that owns this
    // end state on every machine — including the ones that never reach a
    // sign-in at all, which is the case this call cannot cover.
    own_git_credentials(ctx).await?;

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
                //
                // "Text and a wait for a person" is true of this flow only
                // because the `own_git_credentials` call above has removed the
                // one `survey` prompt it used to open with. It was not true
                // when this comment was first written, and the cost was a hang:
                // see that call for why a `survey` prompt under a pty riabuild
                // owns cannot be answered at all.
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

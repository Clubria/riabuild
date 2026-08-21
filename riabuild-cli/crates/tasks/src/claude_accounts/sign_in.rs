//! The one browser round trip provisioning makes for Claude Code.

use crate::Ctx;
use anyhow::Result;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;

/// The one browser round trip provisioning makes for Claude Code.
///
/// Mirrors `github_cli::sign_in`, including checking the exit code: a developer
/// who abandons the browser must not leave riabuild convinced this machine is
/// ready, with the only symptom a later failure that says nothing about a
/// sign-in.
pub(super) async fn sign_in(ctx: &mut Ctx, id: &str) -> Result<()> {
    // Checked before the terminal is handed over, and the one thing this
    // function must do before anything else. `claude auth login` waits for a
    // browser round trip somebody has to finish, so on a machine with nobody on
    // the other end it does not fail — it waits. On CI it opened a browser and
    // sat there until the job's own timeout killed it, half an hour later, with
    // nothing on stdout to say why. An unattended run has to be refused in a
    // sentence rather than hung.
    if !ctx.ui.interactive() {
        return Err(Failure::new(
            "signing you in to Claude Code",
            "Run `riabuild` yourself from a terminal — the sign-in opens a browser and someone has to finish it.",
        )
        .command("claude auth login")
        .detail("riabuild has no terminal to hand the sign-in to, and will not wait for one")
        .into());
    }

    ctx.ui
        .note("Opening your browser to sign in to Claude Code…");
    let claude = ctx.claude();
    let dir = ctx.paths.claude_profile_dir(id);
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };

    let code = ctx
        .runner
        .run_interactive(&claude, &["auth", "login"], &options)
        .await?;
    if code != 0 {
        return Err(Failure::new(
            "signing you in to Claude Code",
            "Run `riabuild` again and finish the Claude Code sign-in in your browser.",
        )
        .command("claude auth login")
        .detail(format!("that command exited with status {code}"))
        .into());
    }
    Ok(())
}

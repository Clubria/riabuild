//! Which Codex the machine has, and the environment the question is asked
//! in.
//!
//! Codex is a Node script, so a `codex --version` that does not carry
//! riabuild's own Node answers about the developer's machine rather than
//! about the install riabuild made — which is why the probe's environment is
//! as carefully built as the install's.

use super::{PACKAGE_VERSION, path_led_by};
use crate::Ctx;
use anyhow::Result;
use riabuild_runner::RunOptions;
use riabuild_version as version;

/// Whether riabuild has to install the Codex CLI before it can be used.
///
/// The existence test comes first and is not optional, for the reason
/// `claude_accounts::install_needed` gives: `RealRunner::run` returns `Err` when
/// the program is not there — a spawn failure, not an exit code — so asking
/// `--version` first would make the missing-binary case propagate an `anyhow`
/// chain instead of reaching the install.
///
/// An installed copy below the floor routes here too: the install is the
/// upgrade path as well.
pub(super) async fn install_needed(ctx: &Ctx) -> Result<bool> {
    let codex = ctx.codex();
    if !tokio::fs::try_exists(&codex).await.unwrap_or(false) {
        return Ok(true);
    }
    let reported = ctx
        .runner
        .run(&codex, &["--version"], &probe_options(ctx))
        .await?;
    // Equality, not a floor. The floor answers "can the launcher work against
    // this?"; the question here is "is this the Codex riabuild installs?", and
    // a machine running something else — newer included — is a machine whose
    // behaviour nobody in the org has reproduced. Reinstalling converges it.
    Ok(!reported.ok() || !version::same(reported.trimmed(), PACKAGE_VERSION))
}

/// The environment a `codex --version` probe runs in.
///
/// `CODEX_HOME` is named rather than left unset, and that is not tidiness. An
/// unset one sends Codex to `~/.codex` — a directory riabuild does not own, on a
/// machine where the developer may be running their own Codex out of it. A
/// check has no business reading it and less business creating it. This is the
/// same rule `CLAUDE.md` states for `cwd`: the inputs riabuild did not choose
/// are the ones a check must not leave to chance.
///
/// Profile 1, not `codex_dir()`. That is the *parent* of the nine now, and
/// Codex writes its sqlite state and logs into whatever it is handed — so
/// naming the parent would strew a tenth profile's worth of files in among the
/// nine, on every run, for a probe that only wants a version string.
///
/// `PATH` is named for the same reason, and it is what makes this probe answer
/// at all on a machine that is not a laptop. npm installs `bin/codex` as a
/// symlink to a `codex.js` whose shebang is `#!/usr/bin/env node`, so asking
/// Codex its version asks `PATH` for a Node first. riabuild's own Node is the
/// one that has to answer: on a **managed server** riabuild runs under a
/// non-interactive SSH exec whose `PATH` is `/usr/local/bin:/usr/bin:/bin` and
/// carries no Node at all, so `codex --version` exits 127 with `env: 'node':
/// No such file or directory`. `check()` reads that as "the Codex CLI is not
/// installed", `apply()` then installs it perfectly well — `install_codex` is
/// the one call here that already puts riabuild's Node on `PATH` — and the
/// re-check after it says the same thing again, which is the hard error a
/// developer cannot get past by running riabuild again.
///
/// A laptop hides this: the developer's own nvm or Homebrew Node answers, and
/// the probe passes for a reason riabuild did not arrange and cannot rely on.
/// Claude Code hides it further by not having it — `@anthropic-ai/claude-code`
/// ships a native `bin/claude.exe`, so `claude_accounts` probing with a bare
/// `RunOptions::default()` is correct there. That is a fact about that package
/// rather than a pattern to copy.
pub(super) fn probe_options(ctx: &Ctx) -> RunOptions {
    let mut env = vec![(
        "CODEX_HOME".to_string(),
        ctx.paths
            .codex_profile_dir(1)
            .to_string_lossy()
            .into_owned(),
    )];
    // Only where a Node is pinned. Without one `ctx.codex()` is the bare name,
    // there is no riabuild Node to put in front, and both callers have already
    // refused to run anything by the time that could matter.
    if let Some(version) = &ctx.config.node_version {
        env.push(path_led_by(&ctx.paths.node_dir(version).join("bin")));
    }
    RunOptions {
        env,
        ..Default::default()
    }
}

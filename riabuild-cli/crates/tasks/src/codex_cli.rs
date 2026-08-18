//! The Codex CLI, and the launcher that runs it.
//!
//! riabuild installs Codex with the Node it owns and points it at config
//! directories under `~/.riabuild/`, the same way it does Claude Code. It does
//! **not** sign anyone in: a Codex sign-in is a developer's own OpenAI account,
//! nothing riabuild brokers, and nothing the onboarding path is blocked on.
//! `codex-3 login` is one command away when they want it, and it lands in that
//! profile's `CODEX_HOME` because the launcher is what put them there.
//!
//! The generated launchers are `shims::codex`: `codex-1` … `codex-9`, each with
//! its own `CODEX_HOME`, and `codex` for the first. Codex keeps sign-ins apart
//! per `CODEX_HOME` exactly as Claude Code does per `CLAUDE_CONFIG_DIR`, so the
//! nine are nine real accounts — see that module for the evidence, and for why
//! `--yolo` is a default rather than an imposition.

use super::{Ctx, Status, Task, TaskId};
use crate::shims;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;
use std::path::Path;

/// The npm package. Codex ships a per-platform binary underneath it, which npm
/// resolves — riabuild does not pick the platform, and must not.
const PACKAGE: &str = "@openai/codex";

/// The version every behaviour this task and its launcher depend on was
/// verified against.
///
/// Three of them, none documented in a way that survives a major version:
/// `--yolo` is accepted as a *global* option ahead of any subcommand, Codex
/// refuses to start against a `CODEX_HOME` that does not exist, and it rejects
/// `--yolo` beside `--ask-for-approval` or a second time. The launcher is built
/// on all three. Raising the floor costs nothing — `install_codex` installs
/// whatever npm calls latest.
const MIN_VERSION: &str = "0.147.0";

pub struct CodexCli;

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
async fn install_needed(ctx: &Ctx) -> Result<bool> {
    let codex = ctx.codex();
    if !tokio::fs::try_exists(&codex).await.unwrap_or(false) {
        return Ok(true);
    }
    let reported = ctx
        .runner
        .run(&codex, &["--version"], &probe_options(ctx))
        .await?;
    Ok(!reported.ok() || !version::at_least(reported.trimmed(), MIN_VERSION))
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
fn probe_options(ctx: &Ctx) -> RunOptions {
    RunOptions {
        env: vec![(
            "CODEX_HOME".to_string(),
            ctx.paths
                .codex_profile_dir(1)
                .to_string_lossy()
                .into_owned(),
        )],
        ..Default::default()
    }
}

#[async_trait]
impl Task for CodexCli {
    fn id(&self) -> TaskId {
        "codex_cli"
    }

    fn title(&self) -> &str {
        "Codex CLI"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // Codex is installed with the Node riabuild owns, and the launcher
        // records that Node's path — so a toolchain that moved has to re-run
        // this task rather than leave a launcher pointing at a Node that is
        // gone.
        &["toolchain"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        // Existence before invocation — see `install_needed`. Safe here because
        // `depends_on(["toolchain"])` pins a Node first, so `ctx.codex()` is an
        // absolute path under riabuild's own tree by the time this runs. The
        // bare name it falls back to before a Node is pinned would not be:
        // `try_exists("codex")` resolves against the current directory.
        let codex = ctx.codex();
        if !tokio::fs::try_exists(&codex).await.unwrap_or(false) {
            return Ok(Status::needs("the Codex CLI is not installed"));
        }
        let reported = ctx
            .runner
            .run(&codex, &["--version"], &probe_options(ctx))
            .await?;
        if !reported.ok() {
            return Ok(Status::needs("the Codex CLI is not installed"));
        }
        if !version::at_least(reported.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!(
                "the Codex CLI {} is older than {MIN_VERSION}",
                reported.trimmed()
            )));
        }

        // Codex refuses to start against a `CODEX_HOME` that is not there, so
        // every profile directory is part of "this machine is correct", not a
        // detail of having installed something once. Each is named, because
        // "a Codex config directory is missing" does not say which of nine to
        // deal with.
        for profile in 1..=shims::codex::PROFILES {
            if !tokio::fs::try_exists(ctx.paths.codex_profile_dir(profile))
                .await
                .unwrap_or(false)
            {
                return Ok(Status::needs(format!(
                    "Codex profile {profile}'s config directory is missing"
                )));
            }
        }

        // Each launcher is compared against what riabuild would generate *now*,
        // not merely tested for existence. That is what makes this check see
        // the three ways one goes stale on a machine that ran this task six
        // weeks ago: a Node upgrade moves the binary it records, a riabuild
        // upgrade changes the flags it passes, and a developer can edit it. An
        // existence test would report every one of those machines as correct.
        //
        // All ten, not just `codex`. A developer who lives in `codex-4` would
        // otherwise be the one person this check cannot help.
        for name in shims::codex::launcher_names() {
            if let Some(detail) = launcher_drift(ctx, &name).await {
                return Ok(Status::needs(detail));
            }
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if install_needed(ctx).await? {
            install_codex(ctx).await?;
        }
        // Before the launchers, because a launcher is what a developer runs
        // next and Codex will not start without its directory.
        for profile in 1..=shims::codex::PROFILES {
            tokio::fs::create_dir_all(ctx.paths.codex_profile_dir(profile)).await?;
        }
        shims::codex::write(ctx).await?;
        Ok(())
    }
}

/// Why the launcher named `name` is not the one riabuild would write, or `None`
/// when it is.
///
/// Named rather than folded into `check()` so the three states — absent,
/// unreadable, different — each get a sentence a developer can act on, and they
/// are three rather than two on purpose: "is missing" printed for a launcher
/// sitting right there at mode 000 sends the developer looking for the wrong
/// problem. Either way it is drift, and `apply()` rewrites it.
async fn launcher_drift(ctx: &Ctx, name: &str) -> Option<String> {
    let path = ctx.paths.bin_dir().join(name);
    let found = match tokio::fs::read_to_string(&path).await {
        Ok(found) => found,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(format!("{} is missing", path.display()));
        }
        Err(error) => {
            return Some(format!("{} could not be read: {error}", path.display()));
        }
    };
    let wanted = shims::codex::launcher_script(
        &ctx.paths.codex_profile_dir(shims::codex::profile_of(name)),
        &ctx.codex(),
        &ctx.paths.bin_dir(),
    );
    (found != wanted).then(|| format!("{} is not the launcher riabuild writes", path.display()))
}

async fn install_codex(ctx: &mut Ctx) -> Result<()> {
    let node_version = match ctx.config.node_version.clone() {
        Some(pinned) => pinned,
        // Not `unwrap_or_else`: the fallback awaits, and a closure cannot.
        None => super::toolchain::desired_node(ctx.project_dir().as_deref()).await,
    };
    let node_dir = ctx.paths.node_dir(&node_version);
    let node_bin = node_dir.join("bin");
    let npm = node_bin.join("npm");

    if !tokio::fs::try_exists(&npm).await.unwrap_or(false) {
        return Err(Failure::new(
            "installing the Codex CLI",
            "Run `riabuild` again — the Node install has to finish first.",
        )
        .detail(format!("{} does not exist", npm.display()))
        .into());
    }

    ctx.ui.note("Installing the Codex CLI…");
    // `--prefix` names the tree `Ctx::codex()` reads, and names it on the
    // command line so a `prefix` line in the developer's own `~/.npmrc` cannot
    // redirect the install. Without it, `check()` reports Codex as missing on a
    // machine that has just installed it — and keeps installing it, every run,
    // forever.
    let prefix = node_dir.to_string_lossy().into_owned();
    let output = ctx
        .runner
        .run(
            &npm.to_string_lossy(),
            &["install", "-g", "--prefix", &prefix, PACKAGE],
            &RunOptions {
                env: npm_env(&node_bin),
                ..Default::default()
            },
        )
        .await?;
    if !output.ok() {
        return Err(Failure::new(
            "installing the Codex CLI",
            "Install it yourself with `npm install -g @openai/codex`, then run `riabuild` again.",
        )
        .command("npm install -g @openai/codex")
        .detail(output.stderr)
        .into());
    }
    Ok(())
}

/// The environment `npm` has to run in for `-g` to mean riabuild's Node.
///
/// The same reasoning as `claude_accounts::npm_env`, and deliberately its own
/// copy rather than a shared helper: these are two lines each task owns, and a
/// shared one would have to be reached through a module that exists only to
/// hold it.
///
/// `bin/npm` in the Node tarball is a symlink to a script whose shebang is
/// `#!/usr/bin/env node`, so the machine's own Node interprets it whenever one
/// is on `PATH` — and npm derives its global prefix from `process.execPath`,
/// which would then be a Node riabuild does not own.
fn npm_env(node_bin: &Path) -> Vec<(String, String)> {
    let ambient = std::env::var("PATH").unwrap_or_default();
    vec![(
        "PATH".to_string(),
        format!("{}:{ambient}", node_bin.display()),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;
    use std::sync::Arc;

    const VERSION: &str = "codex --version";
    const NODE: &str = "22.23.1";

    fn installed() -> FakeRunner {
        FakeRunner::new().with(VERSION, 0, "codex-cli 0.147.0", "")
    }

    /// A ctx whose Codex binary is where `ctx.codex()` says it is.
    ///
    /// The contents are irrelevant — every invocation goes through
    /// `FakeRunner` — but it has to exist, because its existence is what tells
    /// a provisioned machine from a bare one.
    async fn ctx_with_codex(runner: FakeRunner) -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = ctx_with(runner).await;
        // Written to disk, not just into `ctx.config`: `apply` updates the
        // config under the lock, which reloads, and a pin that was never on
        // disk would be discarded there.
        ctx.update_config(|config| config.node_version = Some(NODE.into()))
            .await
            .unwrap();
        write_file(Path::new(&ctx.codex()), "#!/bin/sh\n").await;
        (ctx, home)
    }

    /// The machine this task is trying to produce.
    async fn ready() -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = ctx_with_codex(installed()).await;
        CodexCli.apply(&mut ctx).await.expect("apply");
        (ctx, home)
    }

    #[tokio::test]
    async fn a_machine_with_no_codex_is_asked_to_install_one() {
        let (ctx, _home) = ctx_with(installed()).await;
        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_codex_riabuild_has_not_installed_is_never_run() {
        // `RealRunner::run` returns `Err` on a missing program, so a check that
        // probed `--version` first would propagate a spawn failure instead of
        // asking for an install.
        let (mut ctx, _home) = ctx_with(installed()).await;
        let runner = Arc::new(installed());
        ctx.runner = runner.clone();
        ctx.config.node_version = Some(NODE.into());

        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "{status:?}"
        );
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    #[tokio::test]
    async fn an_old_codex_is_detected() {
        let (ctx, _home) =
            ctx_with_codex(FakeRunner::new().with(VERSION, 0, "codex-cli 0.140.0", "")).await;
        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("older than"), "{status:?}");
    }

    #[tokio::test]
    async fn the_version_probe_never_reads_the_developers_own_codex_home() {
        // Unset, CODEX_HOME is `~/.codex` — a directory riabuild does not own
        // and a check has no business touching. Profile 1 rather than the
        // parent of the nine: Codex writes sqlite state into whatever it is
        // handed, and the parent is not a profile.
        let (ctx, _home) = ctx_with_codex(installed()).await;
        let options = probe_options(&ctx);
        let (key, value) = options.env.first().expect("the probe names a CODEX_HOME");
        assert_eq!(key, "CODEX_HOME");
        assert_eq!(
            value,
            &ctx.paths
                .codex_profile_dir(1)
                .to_string_lossy()
                .into_owned()
        );
    }

    #[tokio::test]
    async fn every_profile_gets_a_directory_and_a_launcher() {
        let (ctx, _home) = ready().await;
        for profile in 1..=shims::codex::PROFILES {
            assert!(
                tokio::fs::try_exists(ctx.paths.codex_profile_dir(profile))
                    .await
                    .unwrap(),
                "profile {profile} has no directory"
            );
            assert!(
                tokio::fs::try_exists(ctx.paths.bin_dir().join(format!("codex-{profile}")))
                    .await
                    .unwrap(),
                "codex-{profile} was not written"
            );
        }
        assert!(
            tokio::fs::try_exists(ctx.paths.bin_dir().join("codex"))
                .await
                .unwrap()
        );
    }

    /// The regression that would make nine launchers worthless.
    ///
    /// Nine scripts that all export the same `CODEX_HOME` look right in every
    /// other test — they are present, executable, carry `--yolo`, and run — and
    /// yet every one of them opens the same account. Codex keeps sign-ins apart
    /// per `CODEX_HOME` and by nothing else, so *distinct* is the whole feature.
    #[tokio::test]
    async fn the_nine_launchers_open_nine_different_accounts() {
        let (ctx, _home) = ready().await;
        let mut homes = std::collections::BTreeSet::new();
        for profile in 1..=shims::codex::PROFILES {
            let script =
                tokio::fs::read_to_string(ctx.paths.bin_dir().join(format!("codex-{profile}")))
                    .await
                    .unwrap();
            let home = ctx.paths.codex_profile_dir(profile);
            let line = format!("CODEX_HOME=\"{}\"", home.display());
            assert!(
                script.contains(&line),
                "codex-{profile} does not pin {line}"
            );
            homes.insert(home);
        }
        assert_eq!(
            homes.len(),
            shims::codex::PROFILES,
            "the launchers share a CODEX_HOME, so they share an account"
        );
    }

    #[tokio::test]
    async fn the_bare_name_is_the_first_profile() {
        // `codex` and `codex-1` are one account reached by two names, the shape
        // `claude` and `claude-1` already have.
        let (ctx, _home) = ready().await;
        let bare = tokio::fs::read_to_string(ctx.paths.bin_dir().join("codex"))
            .await
            .unwrap();
        let first = tokio::fs::read_to_string(ctx.paths.bin_dir().join("codex-1"))
            .await
            .unwrap();
        assert_eq!(bare, first);
        assert!(
            bare.contains(
                &ctx.paths
                    .codex_profile_dir(1)
                    .to_string_lossy()
                    .into_owned()
            ),
            "{bare}"
        );
    }

    #[tokio::test]
    async fn a_deleted_profile_directory_names_the_profile() {
        // "a Codex config directory is missing" does not say which of nine to
        // deal with, and the developer has to act on it by hand.
        let (ctx, _home) = ready().await;
        tokio::fs::remove_dir_all(ctx.paths.codex_profile_dir(4))
            .await
            .unwrap();

        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("profile 4"),
            "the profile is not named: {status:?}"
        );
    }

    #[tokio::test]
    async fn a_deleted_numbered_launcher_is_drift() {
        // Not just `codex`: a developer who lives in codex-7 would otherwise be
        // the one person this check cannot help.
        let (ctx, _home) = ready().await;
        tokio::fs::remove_file(ctx.paths.bin_dir().join("codex-7"))
            .await
            .unwrap();

        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("codex-7"), "{status:?}");
    }

    #[tokio::test]
    async fn applying_leaves_a_satisfied_machine() {
        // The engine re-runs `check()` after `apply()`, so this is the property
        // that keeps the task from wedging: a still-failing check is a hard
        // error, and one this task could never repair would be reported on
        // every run forever.
        let (ctx, _home) = ready().await;
        assert_eq!(CodexCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_twice_changes_nothing() {
        let (mut ctx, _home) = ready().await;
        CodexCli.apply(&mut ctx).await.expect("a second apply");
        assert_eq!(CodexCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_deleted_config_directory_is_drift() {
        // Codex refuses to start without it, so a machine missing it is broken
        // rather than merely untidy.
        let (ctx, _home) = ready().await;
        tokio::fs::remove_dir_all(ctx.paths.codex_dir())
            .await
            .unwrap();

        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("config directory is missing"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_deleted_launcher_is_drift() {
        let (ctx, _home) = ready().await;
        tokio::fs::remove_file(ctx.paths.bin_dir().join("codex"))
            .await
            .unwrap();

        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("missing"), "{status:?}");
    }

    /// A launcher riabuild cannot read is drift too, and says so in its own
    /// words. Reporting "is missing" for a file sitting right there would send
    /// the developer looking for the wrong problem.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_launcher_is_drift_and_does_not_claim_to_be_missing() {
        use std::os::unix::fs::PermissionsExt;
        let (ctx, _home) = ready().await;
        let path = ctx.paths.bin_dir().join("codex");
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
            .await
            .unwrap();

        // Root reads a 000 file regardless, and this suite runs in containers
        // that sometimes are root. Asserting on a permission the kernel did not
        // actually enforce would be a test that passes for the wrong reason, so
        // ask whether the mode took effect rather than assuming it did.
        let enforced = tokio::fs::read_to_string(&path).await.is_err();
        let status = CodexCli.check(&ctx).await.unwrap();
        // Restored before any assertion can leave the tempdir unremovable.
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();

        if !enforced {
            return;
        }
        assert_ne!(status, Status::Satisfied, "{status:?}");
        assert!(
            !format!("{status:?}").contains("is missing"),
            "a file that is present must not be reported as missing: {status:?}"
        );
    }

    #[tokio::test]
    async fn an_edited_launcher_is_drift() {
        // The check compares contents, not existence — otherwise a developer
        // who removed the `--yolo` line, or a riabuild upgrade that changed
        // what the launcher passes, both read as a correct machine.
        let (ctx, _home) = ready().await;
        write_file(
            &ctx.paths.bin_dir().join("codex"),
            "#!/bin/sh\nexec codex\n",
        )
        .await;

        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the launcher riabuild writes"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_launcher_left_pointing_at_a_node_that_moved_is_drift() {
        // The launcher records an absolute path under one Node. A toolchain
        // upgrade is an `UpstreamChanged` re-run, but the check has to see it
        // too — otherwise a `--check` reports a machine whose launcher will
        // silently fall back to whatever `codex` is on PATH.
        let (mut ctx, _home) = ready().await;
        ctx.update_config(|config| config.node_version = Some("24.19.0".into()))
            .await
            .unwrap();
        write_file(Path::new(&ctx.codex()), "#!/bin/sh\n").await;

        let status = CodexCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the launcher riabuild writes"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn applying_installs_codex_before_running_it() {
        // The task whose job is installing Codex must reach the install. There
        // is no npm on this machine, so that is as far as it gets — which is
        // the point.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let runner = Arc::new(installed());
        ctx.runner = runner.clone();
        ctx.config.node_version = Some(NODE.into());

        let error = CodexCli.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("installing the Codex CLI"), "{error}");
        assert!(
            !runner.calls().iter().any(|call| call.contains(VERSION)),
            "{:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn codex_is_installed_where_riabuild_looks_for_it() {
        // `npm install -g` decides where a binary lands from the Node that
        // *interprets* npm and from any `prefix` in the developer's own
        // `~/.npmrc` — neither of which is riabuild's Node. Left to npm, Codex
        // installs beside whatever Node is on `PATH`, and `check()` reports it
        // missing on a machine that has just installed it, forever.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.node_version = Some(NODE.into());
        let node_dir = ctx.paths.node_dir(NODE);
        write_file(&node_dir.join("bin").join("npm"), "#!/bin/sh\n").await;

        let runner = Arc::new(FakeRunner::new().with("npm install", 0, "", ""));
        ctx.runner = runner.clone();

        install_codex(&mut ctx).await.expect("the install runs");

        let call = runner
            .calls()
            .into_iter()
            .find(|call| call.contains("install"))
            .expect("npm was run");
        assert!(
            call.contains(&format!("--prefix {}", node_dir.display())),
            "{call}"
        );
        assert!(call.contains(PACKAGE), "{call}");
    }

    #[test]
    fn the_install_runs_npm_under_riabuilds_own_node() {
        let bin = Path::new("/Users/ada/.riabuild/node/22.23.1/bin");
        let env = npm_env(bin);
        let (key, value) = env.first().expect("npm gets an environment");
        assert_eq!(key, "PATH");
        assert!(
            value.starts_with("/Users/ada/.riabuild/node/22.23.1/bin:"),
            "{value}"
        );
    }

    #[tokio::test]
    async fn nothing_signs_the_developer_in() {
        // riabuild installs Codex and points it somewhere; whose OpenAI account
        // it uses is the developer's business. A `codex login` here would open
        // a browser in the middle of provisioning for a sign-in nothing is
        // blocked on.
        let (mut ctx, _home) = ctx_with_codex(installed()).await;
        let runner = Arc::new(installed());
        ctx.runner = runner.clone();

        CodexCli.apply(&mut ctx).await.expect("apply");
        assert!(
            !runner.calls().iter().any(|call| call.contains("login")),
            "{:?}",
            runner.calls()
        );
    }
}

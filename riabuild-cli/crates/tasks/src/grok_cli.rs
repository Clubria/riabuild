//! Grok Build, and the nine launchers that run it.
//!
//! riabuild downloads xAI's coding agent from its own mirror, verifies it
//! against a digest committed to this repository, and points it at config
//! directories under `~/.riabuild/`, the same way it does Claude Code and
//! Codex. It does **not** sign anyone in: a Grok sign-in is the developer's own
//! xAI account, nothing riabuild brokers, and nothing the onboarding path is
//! blocked on. `grok-3 login` is one command away when they want it, and it
//! lands in that profile's `GROK_HOME` because the launcher is what put them
//! there.
//!
//! The generated launchers are `shims::grok`: `grok-1` … `grok-9`, each with
//! its own `GROK_HOME`, and `grok` for the first. Every one adds
//! `--permission-mode bypassPermissions`. See that module for the evidence that
//! `GROK_HOME` really does separate sign-ins, and for why the bypass is a
//! default the launcher stands aside from rather than one it imposes.
//!
//! **riabuild does not run `x.ai/cli/install.sh`, and must not start.** That
//! script is a provisioner of its own and collides with this one at every
//! point: it downloads a floating "latest stable" it verifies against nothing,
//! writes `~/.grok/bin` and `~/.grok/config.toml`, symlinks into
//! `~/.local/bin` and `/usr/local/bin`, and appends a `PATH` block to the
//! developer's `.bashrc`, `.zshrc` or `config.fish` — after backing the file up
//! and, on macOS, editing `.bash_profile` too. riabuild owns all of that, and
//! `shell.rs`'s rule that riabuild gets the last word in a generated rcfile is
//! precisely what a stray `export PATH="$HOME/.grok/bin:$PATH"` would defeat.

use super::{Ctx, Status, Task, TaskId};
use crate::shims;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_fetch::tools;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;

/// Low enough that only a truncated or half-written download fails it. A *bump*
/// of the pinned version is caught by the path instead — `ctx.grok()` names the
/// version, so a new pin is a file that is not there yet.
///
/// The version the launcher's behaviour was actually read out of is 1.0.5; that
/// is recorded in `shims::grok` beside the flags it depends on, and pinned by
/// the `#[ignore]`d smoke tests there rather than by a floor here.
const MIN_VERSION: &str = "1.0.0";

/// The environment a `grok --version` probe runs in.
///
/// `GROK_HOME` is named rather than left unset, and that is not tidiness. Grok
/// Build **creates** a `GROK_HOME` that is not there, so an unset one does not
/// merely read `~/.grok` — it brings that directory into existence on a machine
/// where the developer may be running their own Grok out of it, or may have
/// deliberately never had one. A check has no business reading it and less
/// business creating it. Verified against 1.0.5: `GROK_HOME=/tmp/nope grok
/// --version` leaves a new `/tmp/nope` behind.
///
/// Profile 1, not `grok_dir()`. That is the *parent* of the nine, and Grok
/// Build writes sessions, logs and `config.toml` into whatever it is handed —
/// so naming the parent would strew a tenth profile's worth of files in among
/// the nine, on every run, for a probe that only wants a version string.
///
/// No `PATH`, unlike the Codex probe next door. Codex is a Node script whose
/// shebang sends the machine looking for a `node` first; Grok Build is a
/// static-pie executable that needs nothing on `PATH` — verified against 1.0.5
/// — so supplying one would be cargo-culting the fix to a different tool's bug.
fn probe_options(ctx: &Ctx) -> RunOptions {
    RunOptions {
        env: vec![(
            "GROK_HOME".to_string(),
            ctx.paths.grok_profile_dir(1).to_string_lossy().into_owned(),
        )],
        ..Default::default()
    }
}

/// What the Grok Build riabuild owns has to say for itself.
#[derive(Debug, PartialEq, Eq)]
enum Installed {
    /// There and runnable.
    Usable,
    /// Not there at all.
    Missing,
    /// There, and reporting this — which is not a version we can use.
    Unusable(String),
}

/// Asks the installed Grok Build what it is.
///
/// Asked by `check()` and again by `apply()`, which is the point. Asking about
/// the *version* rather than the file's existence is what keeps `apply()`'s
/// shortcut honest: a truncated download would otherwise be left in place for a
/// `check()` that could then never go green — a check its own repair cannot
/// satisfy. That matters more here than for any other owned tool, because the
/// download is 134–167 MB and therefore the most likely of them to arrive
/// half-written.
async fn installed(ctx: &Ctx) -> Result<Installed> {
    let grok = ctx.grok();
    // Existence before invocation: `RealRunner::run` returns `Err` when the
    // program is not there — a spawn failure, not an exit code — so asking
    // `--version` first would propagate an `anyhow` chain instead of reaching
    // the install.
    if !tokio::fs::try_exists(&grok).await.unwrap_or(false) {
        return Ok(Installed::Missing);
    }
    let output = ctx
        .runner
        .run(&grok, &["--version"], &probe_options(ctx))
        .await?;
    // `grok --version` answers on stdout — `grok 1.0.5 (5115b46bc9)`, verified
    // against 1.0.5 — and stderr is read too, for the reason `ngrok` reads it:
    // a build that banners on the other stream reads as unusable.
    let reported = format!("{}{}", output.stdout, output.stderr);
    if version::at_least(&reported, MIN_VERSION) {
        Ok(Installed::Usable)
    } else {
        Ok(Installed::Unusable(reported.trim().to_string()))
    }
}

pub struct GrokCli;

#[async_trait]
impl Task for GrokCli {
    fn id(&self) -> TaskId {
        "grok_cli"
    }

    fn title(&self) -> &str {
        "Grok Build"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // Nothing. Grok Build is a static binary riabuild downloads whole —
        // no Node, so no `toolchain` edge, which is the one thing that makes
        // this task cheaper than `codex_cli` rather than a copy of it.
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        match installed(ctx).await? {
            Installed::Usable => {}
            Installed::Missing => {
                return Ok(Status::needs(format!(
                    "riabuild has not installed Grok Build {} yet",
                    tools::GROK_VERSION
                )));
            }
            // The owned copy is a known version, so a low one means a truncated
            // or half-written download rather than an old release.
            Installed::Unusable(reported) => {
                return Ok(Status::needs(format!(
                    "the Grok Build in ~/.riabuild reports `{reported}`, which is not usable"
                )));
            }
        }

        // Each is named, because "a Grok config directory is missing" does not
        // say which of nine to deal with. Grok Build would recreate one itself
        // on first use, so this is not the hard failure the equivalent Codex
        // check guards against — it is riabuild telling the truth about the
        // nine accounts it says it made.
        for profile in 1..=shims::grok::PROFILES {
            if !tokio::fs::try_exists(ctx.paths.grok_profile_dir(profile))
                .await
                .unwrap_or(false)
            {
                return Ok(Status::needs(format!(
                    "Grok profile {profile}'s config directory is missing"
                )));
            }
        }

        // Each launcher is compared against what riabuild would generate *now*,
        // not merely tested for existence. That is what makes this check see
        // the three ways one goes stale on a machine that ran this task six
        // weeks ago: a version bump moves the binary it records, a riabuild
        // upgrade changes the flags it passes, and a developer can edit it. An
        // existence test would report every one of those machines as correct —
        // including one whose `grok` had lost the bypass flag, which is the
        // drift with no visible symptom at all.
        //
        // All ten, not just `grok`. A developer who lives in `grok-4` would
        // otherwise be the one person this check cannot help.
        for name in shims::grok::launcher_names() {
            if let Some(detail) = launcher_drift(ctx, &name).await {
                return Ok(Status::needs(detail));
            }
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let release = tools::grok()?;
        let tool_dir = ctx.paths.tool_dir(release.tool, release.version);

        // Not unconditional, for the reason `ngrok::apply` gives and more so:
        // this `apply()` runs for a drifted launcher far more often than for a
        // missing binary, and re-fetching 167 MB to rewrite twenty lines of
        // shell would make every riabuild release a download each laptop pays
        // for.
        if installed(ctx).await? != Installed::Usable {
            ctx.ui
                .note(&format!("Downloading Grok Build {}…", release.version));
            tools::install(&release, &tool_dir).await.map_err(|error| {
                Failure::new(
                    "installing Grok Build",
                    "Check your network connection and run `riabuild` again. If it keeps \
                     failing, send this to your team lead.",
                )
                .detail(format!("{error:#}"))
            })?;
        }

        // Before the launchers, so that a profile riabuild has announced is one
        // that is already there.
        for profile in 1..=shims::grok::PROFILES {
            tokio::fs::create_dir_all(ctx.paths.grok_profile_dir(profile)).await?;
        }
        shims::grok::write(ctx).await?;
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
    let wanted = shims::grok::launcher_script(
        &ctx.paths.grok_profile_dir(shims::grok::profile_of(name)),
        &ctx.grok(),
        &ctx.paths.bin_dir(),
    );
    (found != wanted).then(|| format!("{} is not the launcher riabuild writes", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;
    use std::path::Path;
    use std::sync::Arc;

    fn reporting(version: &str) -> FakeRunner {
        FakeRunner::new().with("grok --version", 0, version, "")
    }

    fn installed_runner() -> FakeRunner {
        reporting("grok 1.0.5 (5115b46bc9)")
    }

    /// A ctx whose Grok binary is where `ctx.grok()` says it is.
    ///
    /// The contents are irrelevant — every invocation goes through
    /// `FakeRunner` — but it has to exist, because its existence is what tells
    /// a provisioned machine from a bare one.
    async fn ctx_with_grok(runner: FakeRunner) -> (Ctx, tempfile::TempDir) {
        let (ctx, home) = ctx_with(runner).await;
        write_file(Path::new(&ctx.grok()), "#!/bin/sh\n").await;
        (ctx, home)
    }

    /// The machine this task is trying to produce.
    async fn ready() -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = ctx_with_grok(installed_runner()).await;
        GrokCli.apply(&mut ctx).await.expect("apply");
        (ctx, home)
    }

    #[tokio::test]
    async fn a_machine_with_no_grok_is_asked_to_install_one() {
        let (ctx, _home) = ctx_with(installed_runner()).await;
        let status = GrokCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("has not installed Grok Build"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_grok_riabuild_has_not_installed_is_never_run() {
        // `RealRunner::run` returns `Err` on a missing program, so a check that
        // probed `--version` first would propagate a spawn failure instead of
        // asking for an install.
        let (mut ctx, _home) = ctx_with(installed_runner()).await;
        let runner = Arc::new(installed_runner());
        ctx.runner = runner.clone();

        GrokCli.check(&ctx).await.unwrap();
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    #[tokio::test]
    async fn a_corrupted_binary_is_detected_rather_than_kept() {
        // The download is 167 MB — the likeliest of riabuild's owned tools to
        // arrive half-written. "Is the file there?" would leave it in place for
        // ever: `check()` would go on saying it is unusable and the `apply()`
        // after it would go on skipping the download.
        let (ctx, _home) = ctx_with_grok(reporting("grok 0.1.0")).await;
        assert_eq!(
            installed(&ctx).await.unwrap(),
            Installed::Unusable("grok 0.1.0".into())
        );
        let status = GrokCli.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not usable"), "{status:?}");
    }

    #[tokio::test]
    async fn the_version_probe_never_reads_or_creates_the_developers_own_grok_home() {
        // Unset, GROK_HOME is `~/.grok` — and Grok Build *creates* one that is
        // not there, so an unset probe does not merely read a directory
        // riabuild does not own, it conjures it. Profile 1 rather than the
        // parent of the nine: Grok writes sessions and config into whatever it
        // is handed, and the parent is not a profile.
        let (ctx, _home) = ctx_with_grok(installed_runner()).await;
        let options = probe_options(&ctx);
        let (key, value) = options.env.first().expect("the probe names a GROK_HOME");
        assert_eq!(key, "GROK_HOME");
        assert_eq!(
            value,
            &ctx.paths.grok_profile_dir(1).to_string_lossy().into_owned()
        );
    }

    #[tokio::test]
    async fn the_probe_carries_no_path_because_grok_needs_none() {
        // Codex needs one because it is a Node script; copying that here would
        // be cargo-culting the fix to a different tool's bug.
        let (ctx, _home) = ctx_with_grok(installed_runner()).await;
        assert!(
            !probe_options(&ctx).env.iter().any(|(key, _)| key == "PATH"),
            "{:?}",
            probe_options(&ctx).env
        );
    }

    #[tokio::test]
    async fn every_profile_gets_a_directory_and_a_launcher() {
        let (ctx, _home) = ready().await;
        for profile in 1..=shims::grok::PROFILES {
            assert!(
                tokio::fs::try_exists(ctx.paths.grok_profile_dir(profile))
                    .await
                    .unwrap(),
                "profile {profile} has no directory"
            );
            assert!(
                tokio::fs::try_exists(ctx.paths.bin_dir().join(format!("grok-{profile}")))
                    .await
                    .unwrap(),
                "grok-{profile} was not written"
            );
        }
        assert!(
            tokio::fs::try_exists(ctx.paths.bin_dir().join("grok"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn every_launcher_bypasses_permissions() {
        // The feature, asserted on the files that actually landed rather than
        // on the generator. A launcher set where only `grok` carried the flag
        // would pass every other test here.
        let (ctx, _home) = ready().await;
        for name in shims::grok::launcher_names() {
            let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join(&name))
                .await
                .unwrap();
            assert!(
                script.contains("--permission-mode bypassPermissions"),
                "{name} does not bypass permissions"
            );
        }
    }

    #[tokio::test]
    async fn a_deleted_profile_directory_names_the_profile() {
        let (ctx, _home) = ready().await;
        tokio::fs::remove_dir_all(ctx.paths.grok_profile_dir(4))
            .await
            .unwrap();

        let status = GrokCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("profile 4"),
            "the profile is not named: {status:?}"
        );
    }

    #[tokio::test]
    async fn a_deleted_numbered_launcher_is_drift() {
        // Not just `grok`: a developer who lives in grok-7 would otherwise be
        // the one person this check cannot help.
        let (ctx, _home) = ready().await;
        tokio::fs::remove_file(ctx.paths.bin_dir().join("grok-7"))
            .await
            .unwrap();

        let status = GrokCli.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("grok-7"), "{status:?}");
    }

    #[tokio::test]
    async fn a_launcher_that_lost_the_bypass_is_drift() {
        // The drift with no visible symptom: Grok Build still starts, still
        // works, and asks for approval on every tool call. An existence check
        // would call this machine correct.
        let (ctx, _home) = ready().await;
        write_file(
            &ctx.paths.bin_dir().join("grok"),
            "#!/bin/sh\nexec grok \"$@\"\n",
        )
        .await;

        let status = GrokCli.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the launcher riabuild writes"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn applying_leaves_a_satisfied_machine() {
        // The engine re-runs `check()` after `apply()`, so this is the property
        // that keeps the task from wedging: a still-failing check is a hard
        // error, and one this task could never repair would be reported on
        // every run forever.
        let (ctx, _home) = ready().await;
        assert_eq!(GrokCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_twice_changes_nothing() {
        let (mut ctx, _home) = ready().await;
        GrokCli.apply(&mut ctx).await.expect("a second apply");
        assert_eq!(GrokCli.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn nothing_signs_the_developer_in() {
        // riabuild installs Grok Build and points it somewhere; whose xAI
        // account it uses is the developer's business. A `grok login` here
        // would open a browser in the middle of provisioning for a sign-in
        // nothing is blocked on.
        let (mut ctx, _home) = ctx_with_grok(installed_runner()).await;
        let runner = Arc::new(installed_runner());
        ctx.runner = runner.clone();

        GrokCli.apply(&mut ctx).await.expect("apply");
        assert!(
            !runner.calls().iter().any(|call| call.contains("login")),
            "{:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn nothing_runs_xais_install_script() {
        // It is a competing provisioner: it writes ~/.grok/bin, symlinks into
        // /usr/local/bin, and appends a PATH block to the developer's rcfile —
        // which is exactly what would demote ~/.riabuild/bin and quietly break
        // the claude launcher and the clipboard shims beside it.
        let (mut ctx, _home) = ctx_with_grok(installed_runner()).await;
        let runner = Arc::new(installed_runner());
        ctx.runner = runner.clone();

        GrokCli.apply(&mut ctx).await.expect("apply");
        for call in runner.calls() {
            assert!(!call.contains("install.sh"), "{call}");
            assert!(!call.contains("x.ai"), "{call}");
            assert!(!call.contains("curl"), "{call}");
        }
    }

    /// A launcher riabuild cannot read is drift too, and says so in its own
    /// words. Reporting "is missing" for a file sitting right there would send
    /// the developer looking for the wrong problem.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_launcher_is_drift_and_does_not_claim_to_be_missing() {
        use std::os::unix::fs::PermissionsExt;
        let (ctx, _home) = ready().await;
        let path = ctx.paths.bin_dir().join("grok");
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
            .await
            .unwrap();

        // Root reads a 000 file regardless, and this suite runs in containers
        // that sometimes are root. Asserting on a permission the kernel did not
        // actually enforce would be a test that passes for the wrong reason.
        let enforced = tokio::fs::read_to_string(&path).await.is_err();
        let status = GrokCli.check(&ctx).await.unwrap();
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
}

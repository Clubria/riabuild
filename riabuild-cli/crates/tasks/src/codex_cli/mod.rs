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

mod install;
mod probe;

use install::install_codex;
use probe::{install_needed, probe_options};

use super::{Ctx, Resource, Status, Task, TaskId};
use crate::shims;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_version as version;
use std::path::Path;

/// The npm package. Codex ships a per-platform binary underneath it, which npm
/// resolves — riabuild does not pick the platform, and must not.
const PACKAGE: &str = "@openai/codex";

/// The exact version riabuild installs.
///
/// A constant for the reason `tools::GH_VERSION` and its neighbours are
/// constants: what riabuild puts on a laptop is versioned, auditable, and ships
/// through a signed release, rather than being whatever npm called `latest` the
/// morning a developer happened to run this. This used to be a bare
/// `npm install -g @openai/codex`, which meant two developers onboarding a week
/// apart got two different Codexes and a bug that reproduced on one did not
/// reproduce on the other.
///
/// **Integrity.** npm resolves an exact version to one entry in the packument
/// and verifies the tarball it downloads against that entry's `dist.integrity`
/// — a sha512 the registry publishes, not one riabuild or riabuild-web supplies
/// — and refuses to unpack a mismatch. Naming the version is what makes that
/// check mean something: `latest` verifies whatever it resolved to, which is a
/// statement about the download rather than about the software. `@openai/codex`
/// also carries an npm provenance attestation, and its per-platform binaries
/// arrive as `optionalDependencies` that npm resolves and verifies the same way.
///
/// Bumping this is a code change, and `version()` goes up beside it so every
/// existing install converges.
const PACKAGE_VERSION: &str = "0.149.0";

/// The version every behaviour this task and its launcher depend on was
/// verified against.
///
/// Three of them, none documented in a way that survives a major version:
/// `--yolo` is accepted as a *global* option ahead of any subcommand, Codex
/// refuses to start against a `CODEX_HOME` that does not exist, and it rejects
/// `--yolo` beside `--ask-for-approval` or a second time. The launcher is built
/// on all three.
///
/// Kept beside [`PACKAGE_VERSION`] rather than replaced by it, and the two say
/// different things. This is the oldest Codex the launcher works against, which
/// is what a machine carrying somebody else's install has to clear; that one is
/// what riabuild puts there. A test asserts the pin is not below the floor.
const MIN_VERSION: &str = "0.147.0";

/// What `npm install -g` is given: an exact version, never a range and never
/// `latest`. See [`PACKAGE_VERSION`].
fn package_spec() -> String {
    format!("{PACKAGE}@{PACKAGE_VERSION}")
}

pub struct CodexCli;

#[async_trait]
impl Task for CodexCli {
    fn id(&self) -> TaskId {
        "codex_cli"
    }

    fn title(&self) -> &str {
        "Codex CLI"
    }

    /// 2 for `PACKAGE_VERSION`. A machine provisioned before the pin has
    /// whatever npm called `latest` that day, and `check()` now disagrees with
    /// it — but only once the engine asks, which is what the bump is for.
    fn version(&self) -> u32 {
        2
    }

    fn depends_on(&self) -> &[TaskId] {
        // Codex is installed with the Node riabuild owns, and the launcher
        // records that Node's path — so a toolchain that moved has to re-run
        // this task rather than leave a launcher pointing at a Node that is
        // gone.
        &["toolchain"]
    }

    fn writes(&self) -> &[Resource] {
        &["node_prefix"]
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
        // Above the floor and still not the pin: someone else's `npm install
        // -g`, or a riabuild release ago. Reported separately from "older
        // than", because the remedy reads differently — nothing is broken, this
        // machine is simply not running the Codex the org runs.
        if !version::same(reported.trimmed(), PACKAGE_VERSION) {
            return Ok(Status::needs(format!(
                "the Codex CLI is {}, and riabuild installs {PACKAGE_VERSION}",
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
    // A launcher naming a riabuild this process cannot locate would be drift
    // nothing could repair, so a failure here is reported as a launcher riabuild
    // cannot vouch for rather than swallowed into "no drift".
    let riabuild = match shims::running_binary() {
        Ok(riabuild) => riabuild,
        Err(error) => return Some(format!("{error:#}")),
    };
    let wanted = shims::codex::launcher_script(
        &riabuild,
        &ctx.paths.codex_profile_dir(shims::codex::profile_of(name)),
        &ctx.codex(),
        &ctx.paths.bin_dir(),
    );
    (found != wanted).then(|| format!("{} is not the launcher riabuild writes", path.display()))
}

/// `PATH` with `dir` in front of whatever riabuild itself was started with.
///
/// Prepended rather than replacing: the ambient `PATH` is how `npm` finds the
/// `sh`, `git` and `tar` it shells out to, and a probe that cleared it would
/// trade one missing-program failure for several.
///
/// Every Node-script tool in this task needs it — npm to install under the
/// right Node, `codex --version` to start at all — which is why it is one
/// function rather than the two copies of the same `format!` this file used to
/// carry. It stays local to `codex_cli` all the same, for the reason `npm_env`
/// gives: a shared helper would need a module that exists only to hold it.
pub(super) fn path_led_by(dir: &Path) -> (String, String) {
    let ambient = std::env::var("PATH").unwrap_or_default();
    ("PATH".to_string(), format!("{}:{ambient}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::install::npm_env;
    use super::*;
    use crate::testing::{Bounds, ctx_with, write_file};
    use riabuild_runner::{FakeRunner, RunOptions};
    use std::sync::Arc;

    const VERSION: &str = "codex --version";
    const NODE: &str = "22.23.1";

    /// Reports the version riabuild pins, because that is what a machine
    /// riabuild provisioned has.
    fn installed() -> FakeRunner {
        FakeRunner::new().with(VERSION, 0, &format!("codex-cli {PACKAGE_VERSION}"), "")
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
        let runner = Arc::new(installed().with("npm install", 0, "", ""));
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

    /// The pin is what riabuild installs and the floor is what the launcher
    /// needs; a pin below the floor would install a Codex `check()` then
    /// rejects, forever.
    #[test]
    fn the_pinned_version_is_not_below_the_floor() {
        assert!(
            version::at_least(PACKAGE_VERSION, MIN_VERSION),
            "{PACKAGE_VERSION} is below {MIN_VERSION}"
        );
    }

    /// A Codex above the floor but not the pinned one is still drift. Before
    /// the pin this machine was reported as correct, and what it was running
    /// was whatever npm called `latest` on the day it was provisioned.
    #[tokio::test]
    async fn a_codex_that_is_not_the_pinned_one_is_reported() {
        let (ctx, _home) =
            ctx_with_codex(FakeRunner::new().with(VERSION, 0, "codex-cli 0.148.0", "")).await;
        let status = CodexCli.check(&ctx).await.unwrap();
        let spelled = format!("{status:?}");
        assert!(spelled.contains("0.148.0"), "{spelled}");
        assert!(spelled.contains(PACKAGE_VERSION), "{spelled}");
    }

    /// What actually reaches npm. The three things that matter are all in one
    /// argv, and all three used to be absent: the exact version, so what lands
    /// is auditable and npm has one packument entry to verify `dist.integrity`
    /// against; `--ignore-scripts`, so no lifecycle script runs before anything
    /// has looked at what was installed; and `--prefix`, so a `prefix` line in
    /// the developer's own `~/.npmrc` cannot redirect the install.
    #[tokio::test]
    async fn the_install_names_an_exact_version_and_runs_no_lifecycle_scripts() {
        let (mut ctx, _home) = ctx_with(installed()).await;
        let runner = Arc::new(installed().with("npm install", 0, "", ""));
        ctx.runner = runner.clone();
        ctx.update_config(|config| config.node_version = Some(NODE.into()))
            .await
            .unwrap();
        // `install_codex` refuses without one, and it is riabuild's own.
        write_file(
            &ctx.paths.node_dir(NODE).join("bin").join("npm"),
            "#!/bin/sh\n",
        )
        .await;

        install_codex(&mut ctx).await.expect("install");

        let install = runner
            .calls()
            .into_iter()
            .find(|call| call.contains("install"))
            .expect("npm was asked to install something");
        assert!(
            install.contains(&format!("@openai/codex@{PACKAGE_VERSION}")),
            "{install}"
        );
        assert!(install.contains("--ignore-scripts"), "{install}");
        assert!(install.contains("--prefix"), "{install}");
        assert!(
            !install.contains("codex latest") && !install.ends_with("@openai/codex"),
            "an unpinned spec: {install}"
        );
    }

    /// A package download is not a call that has hung, and must not be held to
    /// the ceiling for one. Pinned against the literal and against the default
    /// beside it, so dropping the explicit bound fails here rather than on a
    /// developer's slow link.
    #[tokio::test]
    async fn installing_codex_is_given_its_own_patience() {
        let (mut ctx, _home) = ctx_with(installed()).await;
        let bounds = Bounds::default();
        ctx.runner = bounds.watching(Arc::new(installed().with("npm install", 0, "", "")));
        ctx.update_config(|config| config.node_version = Some(NODE.into()))
            .await
            .unwrap();
        write_file(
            &ctx.paths.node_dir(NODE).join("bin").join("npm"),
            "#!/bin/sh\n",
        )
        .await;

        install_codex(&mut ctx).await.expect("install");

        assert_eq!(
            bounds.of("install -g"),
            Some(std::time::Duration::from_secs(1800))
        );
        assert_ne!(
            bounds.of("install -g"),
            RunOptions::default().timeout,
            "tens of megabytes off the registry is not a ten-minute call"
        );
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

    /// The bug that stopped remote mode dead: `check()` reporting "not
    /// installed" for a Codex it had just installed.
    ///
    /// Asserted on the call the runner actually recorded rather than on
    /// `probe_options`, because the two ways this regresses are a probe built
    /// correctly and not used — `install_needed` reaching for
    /// `RunOptions::default()` the way `claude_accounts` legitimately does — and
    /// a `PATH` that names riabuild's Node somewhere other than the front.
    #[tokio::test]
    async fn the_version_probe_runs_codex_under_riabuilds_own_node() {
        let (mut ctx, _home) = ctx_with_codex(installed()).await;
        let runner = Arc::new(installed());
        ctx.runner = runner.clone();

        CodexCli.check(&ctx).await.unwrap();

        let env = runner.env_of(&format!("{} --version", ctx.codex()));
        let (_, path) = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .expect("the probe names a PATH");
        let node_bin = ctx.paths.node_dir(NODE).join("bin");
        assert!(
            path.starts_with(&node_bin.to_string_lossy().into_owned()),
            "riabuild's own Node does not lead the probe's PATH: {path}"
        );
        // Prepended, not replacing: npm and Codex both shell out, and a probe
        // that cleared PATH would trade one missing program for several.
        assert!(path.contains(':'), "the ambient PATH was discarded: {path}");
    }

    #[tokio::test]
    async fn a_machine_with_no_node_of_its_own_still_answers_the_probe() {
        // The server case in one line: riabuild under a non-interactive SSH
        // exec, whose PATH carries no Node at all. Nothing here can make the
        // fake runner honour a PATH, so what is asserted is that riabuild
        // supplies one rather than borrowing whatever the machine had — which
        // on a laptop is the developer's nvm, and on a server is nothing.
        let (ctx, _home) = ctx_with_codex(installed()).await;
        let options = probe_options(&ctx);
        assert!(
            options.env.iter().any(|(key, _)| key == "PATH"),
            "the probe leaves Node to whatever the machine happens to have: {:?}",
            options.env
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
    /// Nine launchers that all name the same `CODEX_HOME` look right in every
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
            let named = format!("--home '{}'", home.display());
            assert!(
                script.contains(&named),
                "codex-{profile} does not name {named}"
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

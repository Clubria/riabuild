//! ngrok, and the shim that authenticates it.
//!
//! **No authtoken is installed.** The token is the team's, it is long-lived,
//! and it is fetched from riabuild-web on every invocation by the generated
//! `~/.riabuild/bin/ngrok` — never written into `ngrok.yml`, a shell rcfile, or
//! this machine at all. That is why this task does not touch it: a task that
//! fetched one would broker a live credential, and write an audit row, on every
//! `riabuild` run, to answer a question nobody asked.
//!
//! It also keeps provisioning honest. A team whose lead has not set a token
//! still provisions green, and the gap is reported by `ngrok` itself at the
//! moment it matters.

use super::{Ctx, Status, Task, TaskId};
use crate::shims;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_fetch::tools;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;

/// Low enough that only a truncated or half-written download fails it. A *bump*
/// of the pinned version is caught by the path instead — `ctx.ngrok()` names
/// the version, so a new pin is a file that is not there yet.
const MIN_VERSION: &str = "3.0.0";

/// What to say when the org has no authtoken set, or nothing when there is
/// nothing to say.
///
/// Read from the timestamp on `/api/v1/org/config`, which every run already
/// fetches — never by asking for the token itself, which brokers a live
/// credential and writes an audit row.
///
/// `None` org means riabuild never reached the server this run. That is not the
/// same fact as "no token is set", and reporting it as one would send an
/// offline developer to bother their lead about nothing.
fn missing_authtoken_note(org: Option<&riabuild_api::org::OrgConfig>) -> Option<String> {
    let org = org?;
    if org.has_ngrok_authtoken() {
        return None;
    }
    Some(
        "Your team lead has not set an ngrok authtoken in the riabuild dashboard yet, \
         so ngrok will run unauthenticated until they do."
            .to_string(),
    )
}

/// What the ngrok riabuild owns has to say for itself.
#[derive(Debug, PartialEq, Eq)]
enum Installed {
    /// There and runnable.
    Usable,
    /// Not there at all.
    Missing,
    /// There, and reporting this — which is not a version we can use.
    Unusable(String),
}

/// Asks the installed ngrok what it is.
///
/// Asked by `check()` and again by `apply()`, which is the point. Asking about
/// the *version* rather than the file's existence is what keeps `apply()`'s
/// shortcut honest: a truncated download would otherwise be left in place for a
/// `check()` that could then never go green — a check its own repair cannot
/// satisfy.
async fn installed(ctx: &Ctx) -> Result<Installed> {
    let ngrok = ctx.ngrok();
    if !tokio::fs::try_exists(&ngrok).await.unwrap_or(false) {
        return Ok(Installed::Missing);
    }
    let output = ctx
        .runner
        .run(&ngrok, &["--version"], &RunOptions::default())
        .await?;
    // `ngrok --version` and `ngrok version` both answer on stdout, verified
    // against 3.39.11 — stderr is read too, for the reason `infisical_cli`
    // reads it: a build that banners on the other stream reads as unusable.
    let reported = format!("{}{}", output.stdout, output.stderr);
    if version::at_least(&reported, MIN_VERSION) {
        Ok(Installed::Usable)
    } else {
        Ok(Installed::Unusable(reported.trim().to_string()))
    }
}

pub struct Ngrok;

#[async_trait]
impl Task for Ngrok {
    fn id(&self) -> TaskId {
        "ngrok"
    }

    fn title(&self) -> &str {
        "ngrok"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let ngrok = ctx.ngrok();
        match installed(ctx).await? {
            Installed::Usable => {}
            Installed::Missing => {
                return Ok(Status::needs(format!(
                    "riabuild has not installed ngrok {} yet",
                    tools::NGROK_VERSION
                )));
            }
            // The owned copy is a known version, so a low one means a truncated
            // or half-written download rather than an old release.
            Installed::Unusable(reported) => {
                return Ok(Status::needs(format!(
                    "the ngrok in ~/.riabuild reports `{reported}`, which is not usable"
                )));
            }
        }

        // The shim is the whole feature: without it `ngrok` is either absent
        // from `PATH` or — worse — a copy the developer installed themselves,
        // which riabuild would then be silently not authenticating. Comparing
        // the text rather than the file's existence is what catches a shim
        // written by an older riabuild, whose own path moved underneath it.
        let shim = ctx.paths.bin_dir().join("ngrok");
        let wanted =
            shims::ngrok_shim_script(&shims::running_binary()?, std::path::Path::new(&ngrok));
        match tokio::fs::read_to_string(&shim).await {
            Ok(found) if found == wanted => Ok(Status::Satisfied),
            Ok(_) => Ok(Status::needs(
                "the ngrok launcher in ~/.riabuild/bin is not the one this riabuild writes",
            )),
            Err(_) => Ok(Status::needs(
                "ngrok is installed but has no launcher in ~/.riabuild/bin, so it would run without the team's authtoken",
            )),
        }
    }

    /// Downloads the pinned `ngrok` into `~/.riabuild/ngrok/<version>/` and
    /// writes the launcher that authenticates it.
    ///
    /// Still installs **no token**.
    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let release = tools::ngrok()?;
        let tool_dir = ctx.paths.tool_dir(release.tool, release.version);

        // Not unconditional. This `apply()` runs for a drifted launcher far
        // more often than for a missing binary — the launcher names riabuild's
        // own versioned path, so every riabuild release rewrites it — and
        // fetching 12 MB to rewrite six lines of shell would make each release
        // a download every laptop pays for.
        if installed(ctx).await? != Installed::Usable {
            ctx.ui
                .note(&format!("Downloading ngrok {}…", release.version));
            tools::install(&release, &tool_dir).await.map_err(|error| {
                Failure::new(
                    "installing ngrok",
                    "Check your network connection and run `riabuild` again. If it keeps \
                     failing, send this to your team lead.",
                )
                .detail(format!("{error:#}"))
            })?;
        }

        shims::write_ngrok_shim(
            ctx,
            &shims::running_binary()?,
            &release.binary_in(&tool_dir),
        )
        .await?;

        if let Some(note) = missing_authtoken_note(ctx.org.as_ref()) {
            ctx.ui.note(&note);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, ctx_with_tools, write_file};
    use riabuild_runner::FakeRunner;

    fn reporting(version: &str) -> FakeRunner {
        FakeRunner::new().with("ngrok --version", 0, version, "")
    }

    /// A machine where riabuild installed ngrok *and* wrote its launcher.
    async fn install_shim(ctx: &Ctx) {
        let ngrok = ctx.ngrok();
        shims::write_ngrok_shim(
            ctx,
            &shims::running_binary().expect("the test binary has a path"),
            std::path::Path::new(&ngrok),
        )
        .await
        .expect("write the shim");
    }

    #[test]
    fn a_team_with_no_authtoken_yet_is_told_on_the_run_that_installs_ngrok() {
        // Said once, where it is actionable, and without asking the server for
        // a token: `/api/v1/org/config` carries the timestamp, and brokering
        // the value to discover it is absent would write an audit row on every
        // run.
        let mut org = crate::testing::org_config();
        org.ngrok_authtoken_updated_at = 0;
        let note = missing_authtoken_note(Some(&org)).expect("a note");
        assert!(note.contains("team lead"), "{note}");
    }

    #[test]
    fn a_team_that_has_one_is_told_nothing() {
        let mut org = crate::testing::org_config();
        org.ngrok_authtoken_updated_at = 1_755_000_000;
        assert!(missing_authtoken_note(Some(&org)).is_none());
    }

    #[test]
    fn a_run_that_never_reached_the_server_says_nothing_either_way() {
        // No org config means riabuild could not ask, not that the answer was
        // no. Reporting "your lead has not set one" on a laptop that is simply
        // offline would send a developer to bother somebody over nothing.
        assert!(missing_authtoken_note(None).is_none());
    }

    #[tokio::test]
    async fn a_launcher_rewrite_does_not_re_download_ngrok() {
        // Every riabuild release moves riabuild's own path, and the launcher
        // names it in full — so the launcher drifts on every single upgrade.
        // Re-fetching 12 MB to rewrite six lines of shell would make each
        // release cost every laptop a download it does not need.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        assert_eq!(installed(&ctx).await.unwrap(), Installed::Usable);
    }

    #[tokio::test]
    async fn a_corrupted_binary_is_downloaded_again_rather_than_kept() {
        // The other half of the same decision. "Is it there?" would leave a
        // half-written binary in place for ever: `check()` would go on saying
        // it is not usable and the `apply()` after it would go on skipping the
        // download — a check its own repair can never satisfy.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 2.3.40")).await;
        assert_eq!(
            installed(&ctx).await.unwrap(),
            Installed::Unusable("ngrok version 2.3.40".into())
        );
    }

    #[tokio::test]
    async fn a_machine_with_no_ngrok_downloads_it() {
        let (ctx, _home) = ctx_with(reporting("ngrok version 3.39.11")).await;
        assert_eq!(installed(&ctx).await.unwrap(), Installed::Missing);
    }

    #[tokio::test]
    async fn a_current_ngrok_is_satisfied() {
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        install_shim(&ctx).await;
        assert_eq!(Ngrok.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_machine_without_ngrok_asks_for_it() {
        let (ctx, _home) = ctx_with(reporting("ngrok version 3.39.11")).await;
        let status = Ngrok.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("has not installed ngrok"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_corrupted_install_is_detected() {
        // The owned copy is a known version, so a low one means a truncated or
        // half-written download rather than an old release.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 2.3.40")).await;
        install_shim(&ctx).await;
        let status = Ngrok.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not usable"), "{status:?}");
    }

    #[tokio::test]
    async fn an_ngrok_with_no_launcher_is_not_reported_as_satisfied() {
        // The binary alone runs unauthenticated, and a developer would meet
        // that as ngrok's own "authentication failed" rather than as anything
        // riabuild said.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        let status = Ngrok.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("no launcher"), "{status:?}");
    }

    #[tokio::test]
    async fn a_launcher_written_by_another_riabuild_is_replaced() {
        // Self-update moves riabuild's own path, and the shim names it in full.
        // A shim left pointing at the previous binary is drift `check()` has to
        // see — the file is there, and it is wrong.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        let stale = shims::ngrok_shim_script(
            std::path::Path::new("/opt/homebrew/bin/riabuild"),
            std::path::Path::new(&ctx.ngrok()),
        );
        write_file(&ctx.paths.bin_dir().join("ngrok"), &stale).await;
        let status = Ngrok.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the one this riabuild writes"),
            "{status:?}"
        );
    }
}

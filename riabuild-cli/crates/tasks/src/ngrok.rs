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
//!
//! One row in `owned_tool`'s table, so the row is the task — with the two
//! things that are ngrok's own carried as data on it. Its shim is **not** an
//! `exec` line: `shims::ngrok_shim_script` fetches the token per invocation and
//! hands it over in the environment, and folding that into "an exec shim plus a
//! special case" is how a credential ends up somewhere it can be read twice.
//! And a team with no token yet is told so once, on the run that installs
//! ngrok, by the row's `note`.

use crate::Ctx;
use crate::owned_tool::{OwnedTool, Shim, plain_probe};
use crate::shims;
use riabuild_fetch::tools;

pub(crate) static NGROK: OwnedTool = OwnedTool {
    id: "ngrok",
    title: "ngrok",
    label: "ngrok",
    version: 1,
    min_version: "3.0.0",
    pinned_version: tools::NGROK_VERSION,
    release: tools::ngrok,
    binary: Ctx::ngrok,
    probe: plain_probe,
    shim: Some(Shim {
        name: "ngrok",
        render: shims::ngrok_shim_script,
        without_it: "so it would run without the team's authtoken",
    }),
    installing: "installing ngrok",
    note: authtoken_note,
};

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

/// The row's `note`, which is the same question asked of this run's `Ctx`.
///
/// Split from `missing_authtoken_note` so the decision can be tested against an
/// `OrgConfig` directly — including the "riabuild never reached the server"
/// case, which is not a `Ctx` a test can conveniently hold.
fn authtoken_note(ctx: &Ctx) -> Option<String> {
    missing_authtoken_note(ctx.org.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owned_tool::Installed;
    use crate::testing::{ctx_with, ctx_with_tools, write_file};
    use crate::{Status, Task};
    use riabuild_runner::FakeRunner;

    fn reporting(version: &str) -> FakeRunner {
        FakeRunner::new().with("ngrok --version", 0, version, "")
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
    async fn the_note_the_row_carries_is_the_one_this_run_would_print() {
        // The row's hook and the decision above it have to be the same
        // question. A `note` wired to the wrong org would print nothing on
        // exactly the team it exists for.
        let (mut ctx, _home) = ctx_with(reporting("ngrok version 3.39.11")).await;
        ctx.org.as_mut().unwrap().ngrok_authtoken_updated_at = 0;
        assert!(authtoken_note(&ctx).expect("a note").contains("team lead"));
        ctx.org.as_mut().unwrap().ngrok_authtoken_updated_at = 1_755_000_000;
        assert!(authtoken_note(&ctx).is_none());
    }

    #[tokio::test]
    async fn a_launcher_rewrite_does_not_re_download_ngrok() {
        // Every riabuild release moves riabuild's own path, and the launcher
        // names it in full — so the launcher drifts on every single upgrade.
        // Re-fetching 12 MB to rewrite six lines of shell would make each
        // release cost every laptop a download it does not need.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        assert_eq!(NGROK.installed(&ctx).await.unwrap(), Installed::Usable);
    }

    #[tokio::test]
    async fn a_corrupted_binary_is_downloaded_again_rather_than_kept() {
        // The other half of the same decision. "Is it there?" would leave a
        // half-written binary in place for ever: `check()` would go on saying
        // it is not usable and the `apply()` after it would go on skipping the
        // download — a check its own repair can never satisfy.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 2.3.40")).await;
        assert_eq!(
            NGROK.installed(&ctx).await.unwrap(),
            Installed::Unusable("ngrok version 2.3.40".into())
        );
    }

    #[tokio::test]
    async fn a_machine_with_no_ngrok_downloads_it() {
        let (ctx, _home) = ctx_with(reporting("ngrok version 3.39.11")).await;
        assert_eq!(NGROK.installed(&ctx).await.unwrap(), Installed::Missing);
    }

    #[tokio::test]
    async fn a_current_ngrok_is_satisfied() {
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        assert_eq!(NGROK.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_machine_without_ngrok_asks_for_it() {
        let (ctx, _home) = ctx_with(reporting("ngrok version 3.39.11")).await;
        let status = NGROK.check(&ctx).await.unwrap();
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
        let status = NGROK.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not usable"), "{status:?}");
    }

    #[tokio::test]
    async fn an_ngrok_with_no_launcher_is_not_reported_as_satisfied() {
        // The binary alone runs unauthenticated, and a developer would meet
        // that as ngrok's own "authentication failed" rather than as anything
        // riabuild said.
        let (ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        tokio::fs::remove_file(ctx.paths.bin_dir().join("ngrok"))
            .await
            .unwrap();
        let status = NGROK.check(&ctx).await.unwrap();
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
        let status = NGROK.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not the one this riabuild writes"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn the_launcher_never_writes_the_authtoken_down() {
        // The design rule, asserted on the file that actually lands: the token
        // reaches ngrok in one process's environment and is on this machine
        // nowhere else. A shim that baked it in would pass every other test
        // here.
        let (mut ctx, _home) = ctx_with_tools(reporting("ngrok version 3.39.11")).await;
        NGROK.apply(&mut ctx).await.unwrap();
        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join("ngrok"))
            .await
            .unwrap();
        // The token is fetched by the process that goes on to *become* ngrok,
        // so it is not in this file, not in an argument list, and not in a
        // shell variable on the way there.
        assert!(script.contains("internal ngrok"), "{script}");
        assert!(!script.contains("NGROK_AUTHTOKEN"), "{script}");
        assert!(!script.contains("$("), "{script}");
    }
}

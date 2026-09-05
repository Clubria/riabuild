//! Task 8 — cache the org's Claude Code settings.
//!
//! The file is handed to `claude --settings` by every account launcher —
//! `claude` and `claude-1` … `claude-N` — which layers it over that account's
//! own settings at launch. One cached file serves all of them: org policy is
//! org-wide by definition. Nothing is merged into anyone's `settings.json`.
//!
//! A recurring deep-merge cannot express removal, cannot tell org keys from
//! developer keys after the first run, and silently clobbers edits. Layering at
//! launch means org policy is always current, removals take effect, developer
//! edits survive, and there is no merge code to maintain.
//!
//! **Nothing the server sends is written unread.** `vetting` is the gate, and
//! `../../../../CLAUDE.md` is the rule it enforces: the org settings may *name*
//! a program and never *carry* one. This used to write `remote.settings`
//! verbatim, which made a `hooks` block in the dashboard arbitrary code on every
//! laptop.

mod vetting;

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_api::org;
use riabuild_paths::contract_tilde;

pub struct OrgSettings;

/// The status line command the `claude_statusline` task installs on *this*
/// machine, which is the one `vetting` writes into the file.
///
/// Derived from `Paths` rather than written out, because the two are not the
/// same string everywhere: `claude_statusline_file()` hangs off `tools_root()`,
/// so on a server it is the shared account's `~/.riabuild` and not this
/// developer's namespace. A constant here would be wrong on exactly the machines
/// nobody tests by hand.
///
/// `~`, not the absolute path, and that is not cosmetic: Claude Code runs this
/// through a shell, and the tilde is what makes one string right for a laptop
/// and for every developer on a server at once.
fn installed_status_line(ctx: &Ctx) -> String {
    contract_tilde(&ctx.paths.claude_statusline_file(), &ctx.paths.home())
}

#[async_trait]
impl Task for OrgSettings {
    fn id(&self) -> TaskId {
        "org_settings"
    }

    fn title(&self) -> &str {
        "Team Claude Code settings"
    }

    /// 2 for `vetting`. A key that names a program is drift `check()` now sees
    /// on its own, so the bump is not for that half — it is for the *stripped*
    /// half. A cached file written verbatim by an older riabuild can carry keys
    /// this would now leave out, and `updated_at` says nothing about them, so
    /// without a bump those machines keep a file nobody would write today.
    fn version(&self) -> u32 {
        2
    }

    fn depends_on(&self) -> &[TaskId] {
        &["login"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let file = ctx.paths.org_settings_file();
        if !tokio::fs::try_exists(&file).await.unwrap_or(false) {
            return Ok(Status::needs("the team settings have not been fetched yet"));
        }

        let Ok(text) = tokio::fs::read_to_string(&file).await else {
            return Ok(Status::needs("the cached team settings cannot be read"));
        };
        let Ok(cached) = serde_json::from_str::<serde_json::Value>(&text) else {
            // `claude --settings` would fail on this at launch, so it counts as
            // a broken machine even though the file is present.
            return Ok(Status::needs("the cached team settings are not valid JSON"));
        };

        // The file on disk gets the same reading the server's payload does, and
        // before the network is consulted. A machine provisioned by a riabuild
        // released before `vetting` existed has a verbatim copy of whatever the
        // dashboard held that day sitting in the file every launcher passes to
        // `claude --settings`, and `updated_at` would call it current forever.
        // Reporting it here is what makes an upgrade re-write it — or, when it
        // carries a program, fail loudly on the next run instead of the next
        // dashboard edit.
        let vetted = match vetting::vet(&cached, &installed_status_line(ctx)) {
            Ok(vetted) => vetted,
            Err(refusal) => return Ok(Status::needs(refusal.reason().to_string())),
        };

        // Nothing to compare against until this machine is signed in, and the
        // question has to be *asked* before it can be answered — an
        // unauthenticated request gets a 401, which `?` turns into a hard error
        // and takes the whole run down with it.
        //
        // That is the difference between `riabuild --check` telling a developer
        // with an expired session "you are not signed in" and it refusing to
        // report anything at all, which is the moment that command matters
        // most. `login` runs first and this re-checks once it has. Same guard
        // `project` and `env_local` already use.
        if ctx.member.is_none() {
            return Ok(Status::needs("waiting for sign-in"));
        }

        // Vetting is idempotent — it is run over its own output here — so a
        // cached file that differs from what riabuild would write now is a file
        // riabuild did not write. That is what repairs a `statusLine` naming a
        // status line that is no longer installed, which is every machine
        // provisioned before 2026-09-05: `updated_at` cannot see it, because
        // nothing on the server changed. It also makes a hand-edited settings
        // file drift rather than policy.
        //
        // **After** the sign-in guard, not with the local checks above it,
        // because `apply` needs the network: reporting this on a signed-out
        // machine would send the run into a fetch that 401s, which is the exact
        // regression `a_signed_out_machine_is_reported_not_thrown` exists for.
        if vetted.settings != cached {
            return Ok(Status::needs(
                "the cached team settings are not what riabuild would write now",
            ));
        }

        // The authoritative comparison: what the server says it published.
        let remote = org::fetch_claude_settings(&ctx.api).await?;
        match ctx.config.org_settings_updated_at {
            Some(cached) if cached == remote.updated_at => Ok(Status::Satisfied),
            _ => Ok(Status::needs("the team settings changed")),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let remote = org::fetch_claude_settings(&ctx.api).await?;

        // Before anything is written, and before `updated_at` is recorded: a
        // refusal here leaves the previous cached file exactly where it was, so
        // a dashboard edit that carries a program cannot even blank out the
        // policy a machine already had.
        let vetted = vetting::vet(&remote.settings, &installed_status_line(ctx))?;
        if !vetted.stripped.is_empty() {
            ctx.ui.note(&format!(
                "The team Claude Code settings name {} riabuild does not recognise, so {} left \
                 out: {}",
                if vetted.stripped.len() == 1 {
                    "a setting"
                } else {
                    "settings"
                },
                if vetted.stripped.len() == 1 {
                    "it was"
                } else {
                    "they were"
                },
                vetted.stripped.join(", ")
            ));
        }

        let file = ctx.paths.org_settings_file();
        if let Some(parent) = file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&file, serde_json::to_string_pretty(&vetted.settings)?).await?;

        let updated_at = remote.updated_at;
        ctx.update_config(|config| config.org_settings_updated_at = Some(updated_at))
            .await?;
        Ok(())
    }
}

/// Brings this machine's copy of the team settings up to date, outside the task
/// engine.
///
/// `pub(crate)` alongside `claude_trust::trust_one` and its two neighbours, and
/// for the same reason: `riabuild claude new` creates an account and hands it
/// straight to the developer, so the next `riabuild` run is too late. The
/// difference is that this one is not per-account. One file serves every
/// launcher, which is exactly why nothing outside the engine was ensuring it —
/// and why a machine that had never completed a provisioning run gave a brand
/// new account no org policy at all, silently, since the launcher drops
/// `--settings` rather than naming a file that is not there.
///
/// The whole task, not a file test: `check()` compares what is cached against
/// what the server says it published, so this repairs a stale copy as well as a
/// missing one.
pub(crate) async fn ensure_cached(ctx: &mut Ctx) -> Result<()> {
    if OrgSettings.check(ctx).await? == Status::Satisfied {
        return Ok(());
    }

    // `check()` answers the signed-out machine without touching the network,
    // and `apply()` would then spend a request learning what it already knows:
    // there is no session to fetch with. `riabuild claude` is documented to
    // work with no session and no network, so this returns rather than hanging
    // a browser sign-in behind an HTTP timeout.
    if ctx.member.is_none() {
        return Err(anyhow::anyhow!("this machine is not signed in to riabuild"));
    }

    OrgSettings.apply(ctx).await?;
    // The invariant, kept where the engine cannot keep it: apply is always
    // followed by a re-run of check.
    match OrgSettings.check(ctx).await? {
        Status::Satisfied => Ok(()),
        Status::Needs(still) => Err(anyhow::anyhow!("{}", still.describe())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;

    #[tokio::test]
    async fn a_missing_cache_needs_fetching() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not been fetched"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_signed_out_machine_is_reported_not_thrown() {
        // Regression: with a valid cache on disk and no session, `check()` used
        // to ask the server anyway, take a 401, and turn `riabuild --check`
        // into a hard failure — on exactly the machine whose problem is that
        // the session expired. There is no runner output here because a
        // signed-out check must not reach the network at all.
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_file(&ctx.paths.org_settings_file(), r#"{"env":{}}"#).await;
        assert!(ctx.member.is_none());

        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("waiting for sign-in"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn ensuring_on_a_signed_out_machine_reports_that_rather_than_the_network() {
        // `riabuild claude new` calls this on a machine that may have no
        // session, no network, and nothing provisioned. `check` answers that
        // case without a request, so `apply` would spend a round trip — and a
        // reqwest timeout — learning there is no token to send. The wording is
        // the observable difference: an error that reached the network names
        // the host or the status, and this one names the sign-in.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(ctx.member.is_none());

        let error = ensure_cached(&mut ctx)
            .await
            .expect_err("nothing can be fetched without a session");
        assert!(error.to_string().contains("not signed in"), "{error}");
    }

    /// A machine provisioned before `vetting` existed can be holding a verbatim
    /// copy of a `hooks` block in the file every launcher passes to
    /// `claude --settings`. `updated_at` would call it current, so the file
    /// itself has to be read — and read before the sign-in guard, or the answer
    /// on an expired session is "waiting for sign-in" while the hook still runs.
    #[tokio::test]
    async fn a_cached_file_carrying_a_hook_is_reported_rather_than_trusted() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_file(
            &ctx.paths.org_settings_file(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"id"}]}]}}"#,
        )
        .await;

        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("hooks"), "{status:?}");
    }

    /// A cached file naming a status line riabuild did not install is drift,
    /// and it is the shape every machine provisioned before 2026-09-05 is in:
    /// the settings name the JavaScript, that file is gone, and a status line
    /// whose command fails renders as nothing at all.
    ///
    /// `updated_at` cannot see it — nothing on the server changed — so the
    /// comparison against what riabuild would write now is the only thing that
    /// repairs it.
    #[tokio::test]
    async fn a_cached_file_naming_a_status_line_riabuild_did_not_install_is_reported() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        // Signed in, because this drift is reported after the sign-in guard —
        // a machine with no session has a more useful thing to be told.
        ctx.member = Some(riabuild_api::Member {
            github_login: "ada".into(),
            member_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@clubria.dev".into(),
            role: "developer".into(),
            status: "active".into(),
        });
        write_file(
            &ctx.paths.org_settings_file(),
            r#"{"statusLine":{"type":"command","command":"node ~/.riabuild/x.js"}}"#,
        )
        .await;

        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("what riabuild would write"),
            "{status:?}"
        );
    }

    /// The command riabuild writes into the settings is the file
    /// `claude_statusline` installs, expressed through the `~` Claude Code's
    /// shell expands. Both come from one `Paths` method; this is the side that
    /// has to spell it.
    #[tokio::test]
    async fn the_status_line_written_is_the_one_that_gets_installed() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(installed_status_line(&ctx), "~/.riabuild/claude-statusline");
    }

    #[tokio::test]
    async fn a_corrupt_cache_is_detected_without_asking_the_server() {
        // No network here on purpose: invalid JSON on disk is enough to know the
        // machine is wrong.
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_file(&ctx.paths.org_settings_file(), "{ not json").await;
        let status = OrgSettings.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not valid JSON"),
            "{status:?}"
        );
    }
}

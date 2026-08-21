//! `riabuild claude new` — adding an account and signing it in.
//!
//! The four things done to a freshly created account before it is ever
//! launched — the team's settings, the agents view, onboarding, and trust —
//! and the roll-back that leaves nothing behind when the sign-in did not
//! happen.

use super::{in_account, list, update_accounts};
use crate::Ctx;
use crate::accounts;
use crate::accounts::status::{self, Identity};
use crate::shims;
use crate::{claude_agents_view, claude_onboarding, claude_trust, org_settings};
use anyhow::Result;
use riabuild_ui::Failure;
use std::path::Path;

/// Adds an account and signs it in — and only keeps it if that worked.
///
/// No Claude Code session is opened: signing in is the whole job, and the
/// developer starts a session with `claude-<n>` when they want one.
pub async fn new(ctx: &mut Ctx) -> Result<i32> {
    let id = accounts::new_id();

    // Before anything reaches the disk, so `--check` still reports that a tenth
    // account *would be refused* rather than claiming one would be added. `add`
    // is the only place that owns the cap rule, so the refusal is obtained by
    // doing it — to a copy, which is what keeps a `--check` run from writing.
    if ctx.dry_run {
        let mut would_be = ctx.config.clone();
        let number = accounts::add(&mut would_be, id)?;
        ctx.ui
            .info(&format!("would add account {number} and sign it in"));
        return Ok(0);
    }

    // Registered before the directory exists, and deliberately in that order: a
    // directory created first and then refused at the cap is an unregistered
    // account nothing can number, which `claude_accounts` can only report and
    // never repair — every later `riabuild` run aborts there.
    let number = update_accounts(ctx, |config| accounts::add(config, id.clone())).await?;

    let dir = ctx.paths.claude_profile_dir(&id);
    tokio::fs::create_dir_all(&dir).await?;
    shims::write_all(ctx).await?;

    ctx.ui.info(&format!(
        "Signing in account {number} — finish it in your browser."
    ));
    let claude = ctx.claude();
    let started = ctx
        .runner
        .run_interactive(&claude, &["auth", "login"], &in_account(&dir))
        .await;

    // Matched rather than `?`d. `run_interactive` answers a binary it could not
    // start with `Err`, and `Ctx::claude()` is the bare name `claude` until a
    // Node is pinned — which is the ordinary state of the unprovisioned machine
    // this command is required to work on. Propagating that would report a bug
    // in riabuild *and* skip the rollback, leaving the account registered with
    // an empty directory and a launcher: exactly the account nobody chose to
    // create that the rollback exists to prevent.
    if let Err(error) = started {
        roll_back(ctx, &id, &dir).await?;
        return Err(Failure::new(
            "adding a Claude Code account",
            "Run `riabuild` first — it installs the Claude Code this needs.",
        )
        .command("claude auth login")
        .detail(format!("{error:#}"))
        .into());
    }

    // Asked rather than inferred from the exit code: the machine's own answer
    // is the one that decides whether an account exists.
    if !matches!(status::read(ctx, &id).await, Identity::LoggedIn(_)) {
        roll_back(ctx, &id, &dir).await?;
        return Err(Failure::new(
            "adding a Claude Code account",
            "Run `riabuild claude new` again and finish the sign-in in your browser.",
        )
        .detail("the sign-in did not complete, so no account was added")
        .into());
    }

    settle_org_settings(ctx, number).await;
    settle_onboarding(ctx, &id, number).await;
    prefer_agents_view(ctx, &id).await;
    trust(ctx, &id, number).await;
    list(ctx).await
}

/// Makes sure this machine has the team's Claude Code settings, before the
/// account that is about to be launched goes looking for them.
///
/// The one thing here that is not per-account. `org-settings.json` serves every
/// launcher, and until now only a provisioning run ever fetched it — so an
/// account created on a machine that had never completed one got no org policy
/// at all. Not as an error, either: the launcher drops `--settings` when the
/// file is absent, because `claude --settings` on a missing path refuses to
/// start, so the failure is a silent downgrade with a healthy-looking account
/// list on top of it.
///
/// Reported only when the launcher really would find nothing. A copy riabuild
/// could not re-confirm — no session, no network — is still a copy the launcher
/// will layer, and the next `riabuild` run is what brings a stale one up to
/// date; calling that "without the team's settings" would be the opposite of
/// true. A note rather than a refusal for the same reason `trust`'s is: the
/// account was created and signed in, and saying otherwise is the worse lie.
async fn settle_org_settings(ctx: &mut Ctx, number: usize) {
    let Err(error) = org_settings::ensure_cached(ctx).await else {
        return;
    };
    if tokio::fs::try_exists(ctx.paths.org_settings_file())
        .await
        .unwrap_or(false)
    {
        return;
    }
    ctx.ui.note(&format!(
        "Account {number} will start without the team's Claude Code settings ({error:#}) — run `riabuild` to fetch them"
    ));
}

/// Gives one freshly created account the team's agents-view default.
///
/// Here as well as in the task for the same reason onboarding is: the account is
/// about to be used, and the next `riabuild` run is too late to decide what its
/// first session opens on.
///
/// The one silent failure on this path, and deliberately so. Its two neighbours
/// each report, because each is about to put a *dialog* in front of the
/// developer and an unexplained dialog reads as riabuild not having worked. This
/// one only decides which view opens: a note about it would be riabuild
/// apologising for a preference, and the next run sets it anyway.
async fn prefer_agents_view(ctx: &mut Ctx, id: &str) {
    let _ = claude_agents_view::prefer_one(ctx, id).await;
}

/// Records that Claude Code's first-run setup is done for one freshly created
/// account.
///
/// The sign-in above is exactly what leaves it undone: `claude auth login`
/// writes the account's credentials and never touches
/// `hasCompletedOnboarding`, so without this the developer's very next
/// `claude-<n>` opens the theme picker and then asks them to log in — to the
/// account they just finished logging into. See `tasks::claude_onboarding`.
///
/// Separate from `trust` rather than folded into it because it needs no
/// checkout: an account created before the project lands still deserves a
/// Claude Code that opens. A failure is a note for the same reason trust's is —
/// the account exists and is signed in, and the next `riabuild` run repairs it.
async fn settle_onboarding(ctx: &mut Ctx, id: &str, number: usize) {
    if let Err(error) = claude_onboarding::complete_one(ctx, id).await {
        ctx.ui.note(&format!(
            "Account {number} will still ask you Claude Code's first-run questions ({error:#}) — run `riabuild` to finish it"
        ));
    }
}

/// Trusts the checkout for one freshly created account.
///
/// `claude_trust` only runs inside the task engine, so without this the
/// developer's very next `claude-<n>` in the checkout opens Claude Code's trust
/// modal and holds the org's settings back as untrusted — the one dialog this
/// product exists to keep them from meeting. The next `riabuild` run repairs it,
/// which is why a failure here is a note and not a refusal: the account was
/// created and signed in, and saying otherwise would be a worse lie than the
/// dialog. But the window is exactly the minute the developer is about to use
/// the account, so it is closed here.
///
/// Both outcomes are said out loud. Silence is the one wrong answer: an
/// unexplained trust dialog reads as riabuild not having worked.
async fn trust(ctx: &mut Ctx, id: &str, number: usize) {
    let Some(dir) = ctx.project_dir() else {
        ctx.ui.note(&format!(
            "No checkout has been chosen yet, so account {number} will ask you to trust one — run `riabuild` and it is done for you"
        ));
        return;
    };
    let keys = claude_trust::trust_keys(&dir).await;
    if let Err(error) = claude_trust::trust_one(ctx, id, &keys).await {
        ctx.ui.note(&format!(
            "Account {number} could not be given the checkout's trust ({error:#}) — run `riabuild` to finish it"
        ));
    }
}

/// Undoes everything `new` did, so a sign-in that did not happen leaves nothing.
async fn roll_back(ctx: &mut Ctx, id: &str, dir: &Path) -> Result<()> {
    // By id, and under the lock. The registration this is undoing was made
    // under the lock too, and in between a second terminal can have added an
    // account of its own — at which point the *number* this account was given
    // is somebody else's.
    update_accounts(ctx, |config| {
        accounts::remove_id(config, id);
        Ok(())
    })
    .await?;
    // Swallowed because the refusal the caller is about to return says more than
    // this would. The stake is worth writing down: a directory left behind here
    // is one `claude_accounts::apply` *adopts* on the next run, which would
    // resurrect the very account this just refused to create — so it is
    // attempted rather than left to riabuild, and only its failure is ignored.
    let _ = tokio::fs::remove_dir_all(dir).await;
    shims::write_all(ctx).await
}

//! `riabuild claude delete` — taking an account off the list.
//!
//! The confirmation the developer is asked, the sign-out that goes with it,
//! and the renumbering every later account inherits — because position *is*
//! the account number.

use super::{in_account, list, update_accounts};
use crate::Ctx;
use crate::accounts;
use crate::accounts::status::{self, Identity};
use crate::shims;
use anyhow::Result;
use riabuild_paths::contract_tilde;
use riabuild_ui::Failure;
use std::path::Path;

/// The question a delete is confirmed with.
///
/// The account is named *inside* the question and not only in the lines above
/// it, because `Ui::info` returns early under `--quiet` while `Ui::ask` does
/// not: `riabuild --quiet claude delete 2` would otherwise ask a bare "Confirm
/// [y/N]" with the email and the warning silently dropped — losing the one
/// guarantee this path exists to give, in the mode where a developer is least
/// likely to notice it is missing.
fn confirm_question(number: usize, named: &str) -> String {
    format!("  Delete account {number} — {named}? [y/N]")
}

pub async fn delete(ctx: &mut Ctx, number: usize, assume_yes: bool) -> Result<i32> {
    // The number before the count, so `riabuild claude delete 4` on a machine
    // with one account says there is no account 4 rather than refusing to delete
    // the only one. The right refusal for the wrong reason teaches a developer
    // something false about their own machine.
    let id = accounts::id_of(&ctx.config, number)?;

    if ctx.config.claude_accounts.len() <= 1 {
        return Err(Failure::new(
            "deleting your only Claude Code account",
            "Add another with `riabuild claude new` first.",
        )
        .detail("the next run would only create an empty one and ask you to sign in again")
        .into());
    }

    let named = match status::read(ctx, &id).await {
        Identity::LoggedIn(email) => email,
        _ => format!("account {number}"),
    };

    // After the refusals and before the prompt, exactly like `reset --check`:
    // there is nothing to confirm when nothing will be done, and a `--check` run
    // in CI must not fail for want of a terminal to ask on.
    if ctx.dry_run {
        ctx.ui
            .info(&format!("would delete account {number} — {named}"));
        return Ok(0);
    }

    if !assume_yes {
        // Checked before asking, because `ask` returns None both for "they
        // chose the default" and for "there was nobody to ask", and an
        // irreversible delete has to tell those apart.
        if !ctx.ui.interactive() {
            return Err(Failure::new(
                format!("asking whether to delete Claude Code account {number}"),
                "re-run as `riabuild claude delete <number> --yes` if you meant to remove it unattended",
            )
            .detail("riabuild has no terminal to ask on, and will not assume yes.")
            .into());
        }
        ctx.ui.info("");
        // States what will happen; the question below asks. Saying "Delete
        // account 2 — you@example.com?" here as well would put the same sentence
        // on screen twice, and a prompt that repeats itself reads like a bug.
        ctx.ui.info(&format!(
            "  riabuild will remove account {number} from this machine."
        ));
        ctx.ui
            .info("  Its Claude Code sessions, history and login go with it.");
        // `Ui::confirm` defaults to yes, which is right for "shall I install
        // this" and wrong here: an empty answer must decline.
        let answer = ctx.ui.ask(&confirm_question(number, &named));
        let confirmed = answer
            .is_some_and(|answer| matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"));
        if !confirmed {
            ctx.ui.info("Left alone.");
            return Ok(0);
        }
    }

    // Signing out first is load-bearing: on macOS the keychain item is named
    // for a hash of the config directory's path, so removing the directory
    // first orphans a credential nothing can ever reach again. A logout that
    // fails is not fatal — the directory still has to go.
    let dir = ctx.paths.claude_profile_dir(&id);
    if sign_out(ctx, &dir).await {
        ctx.ui.note(&format!("Signed out {named}"));
    }

    let shown = contract_tilde(&dir, &ctx.paths.home());
    tokio::fs::remove_dir_all(&dir).await.map_err(|error| {
        Failure::new(
            format!("removing {shown}"),
            "check what still has a file open there, then run it again",
        )
        .detail(error.to_string())
    })?;
    ctx.ui.note(&format!("Removed {shown}"));

    // By id rather than by `number`, because the number was resolved against
    // the snapshot this process started with and the lock may hand back a list
    // another terminal has since shifted. The id is the account whose directory
    // was just removed, so this deregisters that one or nothing.
    update_accounts(ctx, |config| {
        accounts::remove_id(config, &id);
        Ok(())
    })
    .await?;
    shims::write_all(ctx).await?;
    if number <= ctx.config.claude_accounts.len() {
        ctx.ui
            .note(&format!("Account {} is now account {number}", number + 1));
    }

    list(ctx).await
}

/// Signs one account out, and answers whether that actually happened.
///
/// The failure is swallowed on purpose — a logout that did not work must not
/// block the removal, because the directory still has to go — but the answer is
/// not, because riabuild then prints a line about it. On an unprovisioned
/// machine the spawn fails outright and "Signed out you@example.com" is a
/// sentence about the machine that is simply false. Half the value of a
/// provisioner is telling the truth about the machine, so when this says no,
/// nothing is printed: the removal note follows immediately and carries the
/// outcome on its own.
async fn sign_out(ctx: &Ctx, dir: &Path) -> bool {
    let claude = ctx.claude();
    matches!(
        ctx.runner
            .run(&claude, &["auth", "logout"], &in_account(dir))
            .await,
        Ok(output) if output.ok()
    )
}

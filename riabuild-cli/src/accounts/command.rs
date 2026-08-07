//! `riabuild claude` — the account list, and the four things done to it.

use crate::accounts;
use crate::accounts::render;
use crate::accounts::status::{self, Identity};
use crate::cli::ClaudeAction;
use crate::paths::contract_tilde;
use crate::runner::RunOptions;
use crate::shims;
use crate::tasks::Ctx;
use crate::ui::Failure;
use anyhow::Result;
use std::path::Path;

/// Points an invocation at one account.
///
/// The single mechanism the whole feature rests on: `CLAUDE_CONFIG_DIR` scopes
/// the sign-in as well as the settings, so an invocation that loses it silently
/// addresses the default account rather than the one the developer named. Worth
/// one function rather than a copy per call site.
fn in_account(dir: &Path) -> RunOptions {
    RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    }
}

pub async fn run(ctx: &mut Ctx, action: Option<ClaudeAction>) -> Result<i32> {
    match action.unwrap_or(ClaudeAction::List) {
        ClaudeAction::List => list(ctx).await,
        ClaudeAction::New => new(ctx).await,
        ClaudeAction::Delete { number, yes } => delete(ctx, number, yes).await,
        ClaudeAction::Primary { number } => primary(ctx, number).await,
    }
}

async fn list(ctx: &Ctx) -> Result<i32> {
    let found = status::read_all(ctx).await;
    ctx.ui.info("");
    ctx.ui.info(&render::accounts_box(&found, ctx.ui.colour()));
    Ok(0)
}

/// Adds an account and signs it in — and only keeps it if that worked.
///
/// No Claude Code session is opened: signing in is the whole job, and the
/// developer starts a session with `claude-<n>` when they want one.
async fn new(ctx: &mut Ctx) -> Result<i32> {
    let id = accounts::new_id();
    // Registered before the directory exists, and deliberately in that order: a
    // directory created first and then refused at the cap is an unregistered
    // account nothing can number, which `claude_accounts` can only report and
    // never repair — every later `riabuild` run aborts there.
    let number = accounts::add(&mut ctx.config, id.clone())?;

    // After the cap refusal above and before anything reaches the disk, so
    // `--check` still reports that a tenth account *would be refused* rather
    // than claiming one would be added. `add` is the only place that owns the
    // cap rule, so the refusal is obtained by doing it and undoing it — at this
    // point that undo is a `Vec::remove` and nothing else.
    if ctx.dry_run {
        accounts::remove(&mut ctx.config, number)?;
        ctx.ui
            .info(&format!("would add account {number} and sign it in"));
        return Ok(0);
    }

    let dir = ctx.paths.claude_profile_dir(&id);
    tokio::fs::create_dir_all(&dir).await?;
    ctx.config.save(ctx.paths.as_ref()).await?;
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
        roll_back(ctx, number, &dir).await?;
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
        roll_back(ctx, number, &dir).await?;
        return Err(Failure::new(
            "adding a Claude Code account",
            "Run `riabuild claude new` again and finish the sign-in in your browser.",
        )
        .detail("the sign-in did not complete, so no account was added")
        .into());
    }

    list(ctx).await
}

/// Undoes everything `new` did, so a sign-in that did not happen leaves nothing.
async fn roll_back(ctx: &mut Ctx, number: usize, dir: &Path) -> Result<()> {
    accounts::remove(&mut ctx.config, number)?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    // Swallowed because the refusal the caller is about to return says more than
    // this would. The stake is worth writing down: a directory left behind here
    // is one `claude_accounts::apply` *adopts* on the next run, which would
    // resurrect the very account this just refused to create — so it is
    // attempted rather than left to riabuild, and only its failure is ignored.
    let _ = tokio::fs::remove_dir_all(dir).await;
    shims::write_all(ctx).await
}

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

async fn delete(ctx: &mut Ctx, number: usize, assume_yes: bool) -> Result<i32> {
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

    accounts::remove(&mut ctx.config, number)?;
    ctx.config.save(ctx.paths.as_ref()).await?;
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

async fn primary(ctx: &mut Ctx, number: usize) -> Result<i32> {
    // Validated for its refusal, not its value: `--check` has to report "there
    // is no account 4" rather than claim a promotion that would not happen.
    accounts::id_of(&ctx.config, number)?;
    if ctx.dry_run {
        ctx.ui.info(&format!(
            "would make account {number} the one `claude` runs"
        ));
        return Ok(0);
    }

    accounts::promote(&mut ctx.config, number)?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    // Rewritten in place, so shells that are already open pick this up with no
    // further action — which is the reason the environment no longer exports
    // CLAUDE_CONFIG_DIR.
    shims::write_all(ctx).await?;
    list(ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
    use crate::ui::Ui;
    use std::sync::Arc;

    const STATUS: &str = "claude auth status --json";

    fn signed_in() -> FakeRunner {
        FakeRunner::new().with(
            STATUS,
            0,
            r#"{"loggedIn":true,"email":"clubria@proton.me"}"#,
            "",
        )
    }

    /// A ctx with `count` accounts on disk, all signed in.
    async fn with_accounts(count: usize) -> (Ctx, tempfile::TempDir, Vec<String>) {
        let (mut ctx, home) = ctx_with(signed_in()).await;
        let mut ids = Vec::new();
        for _ in 0..count {
            let id = accounts::new_id();
            tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
                .await
                .unwrap();
            ids.push(id);
        }
        ctx.config.claude_accounts = ids.clone();
        (ctx, home, ids)
    }

    /// How many account directories are actually on disk.
    ///
    /// The counterweight to every assertion about `claude_accounts`: a registry
    /// that was rolled back correctly while a directory was left behind is the
    /// state `claude_accounts::apply` would adopt back into an account.
    async fn dirs_on_disk(ctx: &Ctx) -> usize {
        let mut entries = tokio::fs::read_dir(ctx.paths.claude_dir()).await.unwrap();
        let mut count = 0;
        while let Ok(Some(_)) = entries.next_entry().await {
            count += 1;
        }
        count
    }

    /// A runner that cannot start anything.
    ///
    /// What `RealRunner` does on a machine with no Claude Code: a failed spawn is
    /// an `Err`, not an exit code. `FakeRunner` cannot express that — it answers
    /// an unstubbed command with exit 127 inside an `Ok` — so the unprovisioned
    /// machine, which `riabuild claude` is required to work on, needs its own
    /// double.
    struct NothingInstalled;

    #[async_trait::async_trait]
    impl crate::runner::CommandRunner for NothingInstalled {
        async fn run(
            &self,
            program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<crate::runner::CommandOutput> {
            Err(anyhow::anyhow!("could not start `{program}`"))
        }

        async fn run_interactive(
            &self,
            program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<i32> {
            Err(anyhow::anyhow!("could not start `{program}`"))
        }

        fn which(&self, _program: &str) -> Option<std::path::PathBuf> {
            None
        }
    }

    #[tokio::test]
    async fn the_question_the_developer_sees_names_the_account() {
        // Asserted on the question actually put, not on `confirm_question` in
        // isolation: `Ui::info` returns early under --quiet and `Ui::ask` does
        // not, so a call site that named the account only in the lines above the
        // prompt would leave `riabuild --quiet claude delete 2` asking a bare
        // "Confirm [y/N]" about an account it never identified.
        let (mut ctx, _home, _ids) = with_accounts(2).await;
        ctx.ui = Ui::scripted(["n"]);

        delete(&mut ctx, 2, false).await.unwrap();

        let asked = ctx.ui.asked();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].contains("clubria@proton.me"), "{asked:?}");
        assert!(asked[0].contains("account 2"), "{asked:?}");
        // Not [Y/n]: an empty answer to an irreversible delete must decline.
        assert!(asked[0].contains("[y/N]"), "{asked:?}");
    }

    #[tokio::test]
    async fn listing_asks_every_account_who_it_is_and_changes_nothing() {
        // This pins one thing and no more: every registered account is asked who
        // it is, so `read_all` covers the whole list rather than a prefix of it.
        //
        // What it deliberately does *not* claim to cover is the text that comes
        // out. `Ui` prints through `println!` and records nothing, so no test
        // here can read the rendered box; a filter applied to `read_all`'s result
        // after the fact would still pass this. `accounts_box` is covered by its
        // own tests in `render.rs`, and the one link between them — that `list`
        // hands over the whole list — is verified by reading the code.
        let (mut ctx, _home, ids) = with_accounts(3).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();

        assert_eq!(list(&ctx).await.unwrap(), 0);

        let asked = runner
            .calls()
            .into_iter()
            .filter(|call| call.contains("auth status"))
            .count();
        assert_eq!(asked, 3, "{:?}", runner.calls());
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn deleting_the_only_account_is_refused() {
        let (mut ctx, _home, ids) = with_accounts(1).await;
        let error = delete(&mut ctx, 1, true).await.unwrap_err().to_string();
        assert!(error.contains("only Claude Code account"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
        assert!(
            tokio::fs::try_exists(ctx.paths.claude_profile_dir(&ids[0]))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_number_nobody_has_is_refused_for_the_right_reason() {
        // With one account, `delete 4` used to report "deleting your only
        // Claude Code account" — the right refusal for the wrong reason, which
        // tells a developer something false about their own machine.
        let (mut ctx, _home, ids) = with_accounts(1).await;
        let error = delete(&mut ctx, 4, true).await.unwrap_err().to_string();
        assert!(error.contains("finding Claude Code account 4"), "{error}");
        assert!(!error.contains("only Claude Code account"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn deleting_signs_the_account_out_and_removes_its_directory() {
        // Both, and in that order in the code: the keychain item is named for a
        // hash of the directory's path, so removing the directory first orphans a
        // credential permanently. This test cannot see the ordering — a version
        // that removed the directory first would pass it identically — only that
        // neither step was skipped.
        let (mut ctx, _home, ids) = with_accounts(2).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();

        delete(&mut ctx, 2, true).await.unwrap();

        let logouts: Vec<String> = runner
            .calls()
            .into_iter()
            .filter(|call| call.contains("auth logout"))
            .collect();
        assert_eq!(logouts.len(), 1, "{:?}", runner.calls());
        assert!(
            !tokio::fs::try_exists(ctx.paths.claude_profile_dir(&ids[1]))
                .await
                .unwrap()
        );
        assert_eq!(ctx.config.claude_accounts, vec![ids[0].clone()]);
    }

    #[tokio::test]
    async fn a_sign_out_that_happened_is_reported() {
        // A non-quiet Ui, because `note` returns early under --quiet and
        // `Ui::noted` records only what was actually printed.
        let (mut ctx, _home, _ids) = with_accounts(2).await;
        ctx.ui = Ui::scripted([]);
        ctx.runner = Arc::new(signed_in().with("claude auth logout", 0, "", ""));

        delete(&mut ctx, 2, true).await.unwrap();

        let notes = ctx.ui.noted();
        assert!(
            notes.iter().any(|note| note.contains("Signed out")),
            "{notes:?}"
        );
    }

    #[tokio::test]
    async fn a_sign_out_that_did_not_happen_is_not_claimed() {
        // The unprovisioned machine: `claude` cannot be started at all, so
        // nothing was signed out. Saying so anyway is riabuild describing a
        // machine it did not touch — and this is the removal path, where a
        // developer has no way left to check. The removal itself still has to
        // happen, and still has to be reported.
        let (mut ctx, _home, ids) = with_accounts(2).await;
        ctx.ui = Ui::scripted([]);
        ctx.runner = Arc::new(NothingInstalled);

        delete(&mut ctx, 2, true).await.unwrap();

        let notes = ctx.ui.noted();
        assert!(
            !notes.iter().any(|note| note.contains("Signed out")),
            "riabuild claimed a sign-out that never ran: {notes:?}"
        );
        assert!(
            notes.iter().any(|note| note.contains("Removed")),
            "{notes:?}"
        );
        assert_eq!(ctx.config.claude_accounts, vec![ids[0].clone()]);
    }

    #[tokio::test]
    async fn deleting_from_the_middle_moves_every_later_account_up_a_number() {
        // The feature's headline promise: delete account 2 of three and account 3
        // becomes account 2. Deleting the *last* account is the one case where
        // nothing renumbers, so it cannot stand in for this.
        let (mut ctx, _home, ids) = with_accounts(3).await;

        delete(&mut ctx, 2, true).await.unwrap();

        assert_eq!(
            ctx.config.claude_accounts,
            vec![ids[0].clone(), ids[2].clone()]
        );
        let bin = ctx.paths.bin_dir();
        let second = tokio::fs::read_to_string(bin.join("claude-2"))
            .await
            .unwrap();
        assert!(
            second.contains(ids[2].as_str()),
            "claude-2 must now run what was account 3: {second}"
        );
        // The assertion that matters most: a launcher left pointing at a deleted
        // directory makes Claude Code create it afresh and ask for a login,
        // leaving an account no riabuild command can see.
        assert!(!tokio::fs::try_exists(bin.join("claude-3")).await.unwrap());
    }

    #[tokio::test]
    async fn deleting_with_nobody_to_ask_refuses_rather_than_assuming() {
        let (mut ctx, _home, ids) = with_accounts(2).await;
        // ctx_with builds a quiet, non-interactive Ui.
        let error = delete(&mut ctx, 2, false).await.unwrap_err().to_string();
        assert!(error.contains("asking whether to delete"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn declining_leaves_the_account_alone() {
        let (mut ctx, _home, ids) = with_accounts(2).await;
        ctx.ui = Ui::scripted(["n"]);
        delete(&mut ctx, 2, false).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn pressing_enter_declines_rather_than_deleting() {
        // A terminal that answers nothing is a developer taking the default,
        // which for an irreversible delete has to be no. This is the test that
        // fails if `ask` is ever swapped back for `confirm`, which reads an empty
        // answer as yes. Distinct from having no terminal at all, which is
        // refused outright above.
        let (mut ctx, _home, ids) = with_accounts(2).await;
        ctx.ui = Ui::scripted([]);

        assert_eq!(delete(&mut ctx, 2, false).await.unwrap(), 0);

        assert_eq!(ctx.config.claude_accounts, ids);
        assert!(
            tokio::fs::try_exists(ctx.paths.claude_profile_dir(&ids[1]))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_dry_run_deletes_nothing() {
        // `--check` is global and documented as changing nothing. It also must
        // not need a terminal: this ctx has none, and a `--check` that refused
        // for want of one would make the flag unusable in CI.
        let (mut ctx, _home, ids) = with_accounts(2).await;
        ctx.dry_run = true;

        assert_eq!(delete(&mut ctx, 2, false).await.unwrap(), 0);

        assert_eq!(ctx.config.claude_accounts, ids);
        assert_eq!(dirs_on_disk(&ctx).await, 2);
    }

    #[tokio::test]
    async fn a_dry_run_still_reports_a_delete_that_would_be_refused() {
        // The reason `--check` is tested after the refusals rather than before
        // them: "it would be fine" is the one answer that must not come back.
        let (mut ctx, _home, _ids) = with_accounts(1).await;
        ctx.dry_run = true;

        let error = delete(&mut ctx, 1, false).await.unwrap_err().to_string();
        assert!(error.contains("only Claude Code account"), "{error}");
    }

    #[tokio::test]
    async fn a_dry_run_adds_no_account() {
        let (mut ctx, _home, ids) = with_accounts(1).await;
        ctx.dry_run = true;

        assert_eq!(new(&mut ctx).await.unwrap(), 0);

        assert_eq!(ctx.config.claude_accounts, ids);
        assert_eq!(dirs_on_disk(&ctx).await, 1);
    }

    #[tokio::test]
    async fn a_dry_run_at_the_cap_still_refuses() {
        let (mut ctx, _home, ids) = with_accounts(accounts::MAX).await;
        ctx.dry_run = true;

        let error = new(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("riabuild claude delete"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn a_dry_run_changes_no_primary() {
        let (mut ctx, _home, ids) = with_accounts(3).await;
        // Written first, so the assertion below is that the launcher still names
        // the old primary rather than that no launcher exists yet — a `claude`
        // rewritten under a `--check` run would change what an already-open
        // shell runs, which is the whole effect this command has.
        shims::write_all(&ctx).await.unwrap();
        ctx.dry_run = true;

        assert_eq!(primary(&mut ctx, 3).await.unwrap(), 0);

        assert_eq!(ctx.config.claude_accounts, ids);
        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join("claude"))
            .await
            .unwrap();
        assert!(script.contains(ids[0].as_str()), "{script}");
    }

    #[tokio::test]
    async fn a_dry_run_still_refuses_a_primary_nobody_has() {
        let (mut ctx, _home, _ids) = with_accounts(2).await;
        ctx.dry_run = true;

        let error = primary(&mut ctx, 7).await.unwrap_err().to_string();
        assert!(error.contains("finding Claude Code account 7"), "{error}");
    }

    #[tokio::test]
    async fn making_an_account_primary_reorders_and_rewrites_the_launchers() {
        let (mut ctx, _home, ids) = with_accounts(3).await;
        primary(&mut ctx, 3).await.unwrap();

        assert_eq!(
            ctx.config.claude_accounts,
            vec![ids[2].clone(), ids[0].clone(), ids[1].clone()]
        );
        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join("claude"))
            .await
            .unwrap();
        assert!(script.contains(ids[2].as_str()), "{script}");
    }

    #[tokio::test]
    async fn a_refusal_at_the_cap_leaves_no_directory_behind() {
        // The order inside `new` is load-bearing: a directory created before
        // the cap is checked is an account nothing can number, and the
        // `claude_accounts` task can only report that state — never repair it —
        // so every later `riabuild` run aborts there. Without this, moving
        // `create_dir_all` one line up stays green everywhere.
        let (mut ctx, _home, ids) = with_accounts(accounts::MAX).await;

        let error = new(&mut ctx).await.unwrap_err().to_string();
        // The action, not the attempt: an abandoned sign-in says "adding a
        // Claude Code account" too, and this must be the cap refusal.
        assert!(error.contains("riabuild claude delete"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
        assert_eq!(
            dirs_on_disk(&ctx).await,
            accounts::MAX,
            "a tenth directory was created anyway"
        );
    }

    #[tokio::test]
    async fn a_sign_in_that_did_not_take_adds_no_account() {
        // The browser was closed. Anything left behind would show as an
        // account permanently "(logged out)" that nobody chose to create.
        let (mut ctx, _home, ids) = with_accounts(1).await;
        ctx.runner = Arc::new(
            FakeRunner::new()
                .with(STATUS, 1, r#"{"loggedIn":false}"#, "")
                .with("claude auth login", 1, "", ""),
        );

        let error = new(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("adding a Claude Code account"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
        assert_eq!(
            dirs_on_disk(&ctx).await,
            1,
            "the abandoned directory was left behind"
        );
    }

    #[tokio::test]
    async fn a_claude_that_cannot_be_started_adds_no_account() {
        // The unprovisioned machine, which this command is required to work on:
        // no Node is pinned, so `Ctx::claude()` is the bare name and the spawn
        // fails outright. Propagating that `Err` would print "it is a bug in
        // riabuild" *and* skip the rollback, leaving a registered account with an
        // empty directory and a launcher — the account nobody chose to create.
        let (mut ctx, _home, ids) = with_accounts(1).await;
        assert_eq!(ctx.claude(), "claude", "this test needs no Node pinned");
        ctx.runner = Arc::new(NothingInstalled);

        let error = new(&mut ctx).await.unwrap_err();

        let failure = error
            .downcast_ref::<Failure>()
            .expect("a machine with no Claude Code is not a riabuild bug");
        assert!(failure.action.contains("Run `riabuild`"), "{failure:?}");
        assert_eq!(ctx.config.claude_accounts, ids);
        assert_eq!(dirs_on_disk(&ctx).await, 1);
        assert!(
            !tokio::fs::try_exists(ctx.paths.bin_dir().join("claude-2"))
                .await
                .unwrap(),
            "the rolled-back account must not keep its launcher"
        );
    }
}

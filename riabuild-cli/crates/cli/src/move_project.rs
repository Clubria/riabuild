//! `riabuild move-project` — a checkout, somewhere else.
//!
//! Moving a checkout by hand leaves riabuild pointing at a directory that is no
//! longer there, and the next run quietly clones a second copy to the old
//! place. This is the supported way to change the answer given at first setup.
//!
//! Two questions now, because a machine can have a checkout of more than one
//! repository: which one, and then where it should go. The first is only put
//! when there is something to choose — one checkout is not a choice — so on the
//! machine every developer has, this command is exactly what it was.

use crate::fs_move::move_tree;
use anyhow::Result;
use riabuild_paths::{contract_tilde, expand_tilde};
use riabuild_tasks::Ctx;
use riabuild_tasks::repo;
use riabuild_tasks::shell;
use riabuild_ui::Failure;
use std::path::{Path, PathBuf};

/// Moves the recorded checkout to `requested`, or to wherever the developer
/// says when there is no argument.
pub async fn run(ctx: &mut Ctx, requested: Option<&str>) -> Result<i32> {
    let home = ctx.paths.home();
    // Asked first, because everything below is about one checkout: which
    // directory is being moved, which path is refused for being inside it, and
    // which repository's entry is rewritten at the end. `None` is a machine with
    // nothing recorded but the single pre-picker path, which the rest of this
    // still handles exactly as it did.
    let moving = repo::pick::choose_cloned(ctx).await?;
    let from = recorded_checkout(ctx, &home).await?;

    let answer = match requested {
        Some(path) => path.to_string(),
        None => {
            ctx.ui.info(&match &moving {
                Some(repo) => format!("{repo} is at {}.", contract_tilde(&from, &home)),
                None => format!("The repository is at {}.", contract_tilde(&from, &home)),
            });
            ctx.ui.ask("Move it to:").ok_or_else(|| {
                Failure::new(
                    "moving your Clubria checkout",
                    "Run `riabuild move-project <path>` with the new location.",
                )
                .detail("there is no terminal here to ask where it should go")
            })?
        }
    };
    let to = expand_tilde(&answer, &home);

    if to == from {
        ctx.ui.info(&format!(
            "The repository is already at {}.",
            contract_tilde(&from, &home)
        ));
        return Ok(0);
    }

    if let Some(refusal) = refusal(&from, &to).await {
        return Err(Failure::new(
            format!("moving your checkout to {}", to.display()),
            "Give a path that does not exist yet, outside the checkout you are moving.",
        )
        .detail(refusal)
        .into());
    }

    // Checked after the refusals above, so `--check` still reports the reasons
    // a move would not work rather than only that it was not attempted.
    if ctx.dry_run {
        ctx.ui.info(&format!(
            "would move {} → {}",
            contract_tilde(&from, &home),
            contract_tilde(&to, &home)
        ));
        return Ok(0);
    }

    // The point of the recursive create: a developer naming a directory that
    // does not exist yet means "make it", not "that was invalid".
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            Failure::new(
                format!("creating {}", parent.display()),
                "Choose somewhere you can write to, then run `riabuild move-project` again.",
            )
            .detail(error.to_string())
        })?;
    }

    ctx.ui.note(&format!(
        "Moving {} → {}…",
        contract_tilde(&from, &home),
        contract_tilde(&to, &home)
    ));
    move_tree(&from, &to).await.map_err(|error| {
        Failure::new(
            format!("moving {} to {}", from.display(), to.display()),
            "Your checkout has been left where it was. Check there is room for it at the new path.",
        )
        .detail(format!("{error:#}"))
    })?;

    let destination = to.to_string_lossy().into_owned();
    match &moving {
        // The moved repository's own entry, and nothing else: the other
        // checkouts on this machine have not gone anywhere. Which repository is
        // *active* is untouched too — this is a question about a directory.
        Some(repo) => {
            let slug = repo.slug().to_string();
            ctx.update_config(|config| config.set_checkout(&slug, destination))
                .await?;
        }
        None => {
            ctx.update_config(|config| config.project_path = Some(destination))
                .await?;
        }
    }

    ctx.ui.info(&format!(
        "moved {} → {}",
        contract_tilde(&from, &home),
        contract_tilde(&to, &home)
    ));
    if shell::already_inside() {
        // This shell was started in a directory that no longer exists, and
        // every command run in it from here would fail confusingly.
        ctx.ui
            .warn("this shell is still in the old directory — type `exit`, then run `riabuild`");
    }
    Ok(0)
}

/// The checkout riabuild is currently looking after.
///
/// Both failures point at `riabuild` itself: creating a checkout is its job,
/// and doing that job is what makes this command meaningful.
async fn recorded_checkout(ctx: &Ctx, home: &Path) -> Result<PathBuf> {
    let Some(from) = ctx.project_dir() else {
        return Err(Failure::new(
            "moving your Clubria checkout",
            "Run `riabuild` first — there is no checkout to move yet.",
        )
        .into());
    };

    if !tokio::fs::try_exists(&from).await.unwrap_or(false) {
        return Err(Failure::new(
            format!("moving {}", contract_tilde(&from, home)),
            "Run `riabuild` to check this machine out again.",
        )
        .detail("riabuild has that path recorded, but nothing is there")
        .into());
    }
    Ok(from)
}

/// Why the checkout cannot move to `to`, if it cannot.
async fn refusal(from: &Path, to: &Path) -> Option<String> {
    if !to.is_absolute() {
        return Some(format!(
            "{} is relative — give a path starting with / or ~/",
            to.display()
        ));
    }

    if to.starts_with(from) {
        return Some(format!(
            "{} is inside the checkout being moved",
            to.display()
        ));
    }

    let Ok(mut reader) = tokio::fs::read_dir(to).await else {
        // Nothing there, or not a directory at all — `rename` will say so.
        return None;
    };
    match reader.next_entry().await {
        // An empty directory is somewhere a developer prepared for this.
        Ok(Some(_)) => Some(format!("{} already has files in it", to.display())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;
    use riabuild_tasks::testing::{ctx_with, write_file};
    use riabuild_ui::Ui;
    use std::path::Path;

    /// A checkout riabuild already knows about.
    async fn recorded(ctx: &mut Ctx, dir: &Path) {
        write_file(&dir.join(".git/HEAD"), "ref: refs/heads/main\n").await;
        write_file(&dir.join("README.md"), "hello\n").await;
        ctx.config.project_path = Some(dir.to_string_lossy().into());
    }

    #[tokio::test]
    async fn moves_the_checkout_and_records_where_it_went() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        recorded(&mut ctx, &from).await;

        run(&mut ctx, Some(to.to_string_lossy().as_ref()))
            .await
            .unwrap();

        assert!(!from.exists(), "the checkout must not be left behind");
        assert!(to.join(".git/HEAD").exists());
        assert_eq!(
            ctx.config.project_path.as_deref(),
            Some(to.to_string_lossy().as_ref()),
            "the new location must be recorded, or the next run re-clones"
        );
    }

    #[tokio::test]
    async fn a_dry_run_moves_nothing() {
        // `--check` is global and documented as changing nothing, and a
        // subcommand that quietly ignored it would make the flag untrustworthy
        // everywhere else.
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        recorded(&mut ctx, &from).await;
        ctx.dry_run = true;

        run(&mut ctx, Some(to.to_string_lossy().as_ref()))
            .await
            .unwrap();

        assert!(from.join(".git/HEAD").exists());
        assert!(!to.exists());
        assert_eq!(
            ctx.config.project_path.as_deref(),
            Some(from.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn creates_a_destination_whose_parents_do_not_exist() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        let to = home.path().join("a/b/c/hub");
        recorded(&mut ctx, &from).await;

        run(&mut ctx, Some(to.to_string_lossy().as_ref()))
            .await
            .unwrap();

        assert!(to.join("README.md").exists());
    }

    /// A checkout of one particular repository, recorded the way the picker and
    /// the `project` task record one.
    ///
    /// Written through `update_config` rather than onto `ctx.config`, because the
    /// first real write reloads the file and would discard a seed that only ever
    /// existed in memory — the trap `riabuild-cli/CLAUDE.md` names under "State
    /// is read under a lock".
    async fn recorded_for(ctx: &mut Ctx, slug: &str, dir: &Path) {
        write_file(&dir.join(".git/HEAD"), "ref: refs/heads/main\n").await;
        write_file(&dir.join("README.md"), "hello\n").await;
        let path = dir.to_string_lossy().into_owned();
        let slug = slug.to_string();
        ctx.update_config(|config| config.set_checkout(&slug, path))
            .await
            .expect("record the checkout");
    }

    /// Which repository this machine is working on, recorded durably.
    async fn working_on(ctx: &mut Ctx, slug: &str) {
        let slug = slug.to_string();
        ctx.update_config(|config| config.active_repo = Some(slug))
            .await
            .expect("record the active repository");
    }

    #[tokio::test]
    async fn with_two_checkouts_it_asks_which_one_and_moves_that_one() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let hub = home.path().join("code/ai-builders-hub");
        let payments = home.path().join("code/payments");
        let to = home.path().join("work/payments");
        recorded_for(&mut ctx, "Clubria/ai-builders-hub", &hub).await;
        recorded_for(&mut ctx, "Clubria/payments", &payments).await;
        working_on(&mut ctx, "Clubria/ai-builders-hub").await;
        // "2" is `payments`: the box puts what Enter would take first, and the
        // rest in name order.
        ctx.ui = Ui::scripted(["2", to.to_string_lossy().as_ref()]);

        run(&mut ctx, None).await.unwrap();

        assert!(to.join(".git/HEAD").exists(), "payments should have moved");
        assert!(
            hub.join(".git/HEAD").exists(),
            "the other tree must not move"
        );
        assert_eq!(
            ctx.config.checkout_of("Clubria/payments"),
            Some(to.to_string_lossy().as_ref())
        );
        assert_eq!(
            ctx.config.checkout_of("Clubria/ai-builders-hub"),
            Some(hub.to_string_lossy().as_ref()),
            "the repository that did not move keeps its path"
        );
        assert_eq!(
            ctx.config.active_repo.as_deref(),
            Some("Clubria/ai-builders-hub"),
            "moving a directory must not change which repository is active"
        );
    }

    #[tokio::test]
    async fn a_typed_repository_name_picks_the_checkout_to_move() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let hub = home.path().join("code/ai-builders-hub");
        let payments = home.path().join("code/payments");
        let to = home.path().join("work/payments");
        recorded_for(&mut ctx, "Clubria/ai-builders-hub", &hub).await;
        recorded_for(&mut ctx, "Clubria/payments", &payments).await;
        ctx.ui = Ui::scripted(["payments", to.to_string_lossy().as_ref()]);

        run(&mut ctx, None).await.unwrap();

        assert!(to.join(".git/HEAD").exists());
        assert!(hub.join(".git/HEAD").exists());
    }

    #[tokio::test]
    async fn a_repository_this_machine_has_not_cloned_is_refused_at_the_question() {
        // There is no directory to move, and saying so here is better than a
        // `rename` that fails on a path nobody recorded.
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let hub = home.path().join("code/ai-builders-hub");
        let payments = home.path().join("code/payments");
        let to = home.path().join("work/payments");
        recorded_for(&mut ctx, "Clubria/ai-builders-hub", &hub).await;
        recorded_for(&mut ctx, "Clubria/payments", &payments).await;
        working_on(&mut ctx, "Clubria/payments").await;
        ctx.ui = Ui::scripted(["design-system", "", to.to_string_lossy().as_ref()]);

        run(&mut ctx, None).await.unwrap();

        // The second answer was Enter, which takes `payments`.
        assert!(to.join(".git/HEAD").exists());
        assert!(hub.join(".git/HEAD").exists());
    }

    #[tokio::test]
    async fn with_one_checkout_there_is_no_repository_question() {
        // The machine every developer has. One checkout is not a choice, and a
        // picker with one row is a worse way to say "this one".
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        recorded_for(&mut ctx, "Clubria/ai-builders-hub", &from).await;
        ctx.ui = Ui::scripted([to.to_string_lossy().as_ref()]);

        run(&mut ctx, None).await.unwrap();

        assert!(to.join(".git/HEAD").exists());
        let asked = ctx.ui.asked();
        assert_eq!(asked.len(), 1, "only the destination was asked: {asked:?}");
        assert!(asked[0].contains("Move it to"), "{asked:?}");
    }

    #[tokio::test]
    async fn a_machine_that_has_not_migrated_yet_moves_its_one_checkout() {
        // `config.json` from before the picker existed: one path, no map. The
        // command has to keep working there, because a developer reaches for it
        // on exactly the machine they have been using for months.
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        recorded(&mut ctx, &from).await;

        run(&mut ctx, Some(to.to_string_lossy().as_ref()))
            .await
            .unwrap();

        assert!(to.join(".git/HEAD").exists());
        assert_eq!(
            ctx.config.project_path.as_deref(),
            Some(to.to_string_lossy().as_ref()),
            "the field the next run still reads has to be the one updated"
        );
    }

    #[tokio::test]
    async fn asks_when_no_path_is_given() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        recorded(&mut ctx, &from).await;
        ctx.ui = Ui::scripted([to.to_string_lossy().as_ref()]);

        run(&mut ctx, None).await.unwrap();

        assert!(to.join(".git/HEAD").exists());
    }

    #[tokio::test]
    async fn no_terminal_and_no_argument_says_which_argument_to_pass() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        recorded(&mut ctx, &home.path().join("code/hub")).await;

        let error = run(&mut ctx, None).await.unwrap_err();

        assert!(
            format!("{error}").contains("move-project"),
            "a non-interactive failure must name the argument form: {error}"
        );
    }

    #[tokio::test]
    async fn refuses_a_destination_inside_the_checkout() {
        // The copy fallback would otherwise walk into its own output.
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        recorded(&mut ctx, &from).await;

        let to = from.join("inner");
        assert!(
            run(&mut ctx, Some(to.to_string_lossy().as_ref()))
                .await
                .is_err()
        );
        assert!(from.join(".git/HEAD").exists(), "the checkout must survive");
    }

    #[tokio::test]
    async fn refuses_a_destination_that_already_has_files_in_it() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        let to = home.path().join("work/hub");
        recorded(&mut ctx, &from).await;
        write_file(&to.join("someone-elses-work.txt"), "mine\n").await;

        assert!(
            run(&mut ctx, Some(to.to_string_lossy().as_ref()))
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read_to_string(to.join("someone-elses-work.txt"))
                .await
                .unwrap(),
            "mine\n",
            "a refused move must not touch what is already there"
        );
    }

    #[tokio::test]
    async fn refuses_a_relative_destination() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        recorded(&mut ctx, &home.path().join("code/hub")).await;

        assert!(run(&mut ctx, Some("work/hub")).await.is_err());
    }

    #[tokio::test]
    async fn moving_somewhere_it_already_is_changes_nothing() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let from = home.path().join("code/hub");
        recorded(&mut ctx, &from).await;

        run(&mut ctx, Some(from.to_string_lossy().as_ref()))
            .await
            .unwrap();

        assert!(from.join(".git/HEAD").exists());
    }

    #[tokio::test]
    async fn a_checkout_riabuild_does_not_have_yet_is_an_error() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let to = home.path().join("work/hub");

        let error = run(&mut ctx, Some(to.to_string_lossy().as_ref()))
            .await
            .unwrap_err();

        assert!(
            format!("{error}").contains("riabuild"),
            "the way out is to run riabuild first: {error}"
        );
    }

    #[tokio::test]
    async fn a_recorded_checkout_that_is_gone_is_an_error() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        ctx.config.project_path = Some(home.path().join("code/hub").to_string_lossy().into());

        assert!(
            run(
                &mut ctx,
                Some(home.path().join("work/hub").to_string_lossy().as_ref())
            )
            .await
            .is_err()
        );
    }
}

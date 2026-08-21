//! `riabuild reset` — remove `~/.riabuild` and start over.
//!
//! Deliberately outside the task engine. Reset exists for the machine no task
//! can talk its way out of: a half-downloaded toolchain, a state file that
//! disagrees with reality, a tree left by a version of riabuild that no longer
//! exists. Running the checks first would mean repairing the thing about to be
//! deleted, and would fail on exactly the machines that need this most.
//!
//! What it removes is reconstructible by design — `check()` is authoritative,
//! so `state.json` is a cache, and every toolchain is a download away. What it
//! does *not* touch is the developer's checkout, which lives outside the tree.
//! Their riabuild sign-in usually survives too, because the token is in the
//! keychain — but on a machine with no keyring it is `session.token` inside the
//! tree, and a reset there signs them out. That is the right outcome (a reset
//! is "start over", and `login` will simply ask again), and it is not worth a
//! special case to preserve. It **is** worth saying out loud, and `warnings`
//! now does: the line used to promise unconditionally that "your riabuild
//! sign-in is not touched", so on a managed server or a headless Linux box the
//! developer confirmed a recursive delete against a claim that was false — on
//! exactly the machines where signing in again is most awkward. Their Claude
//! Code sign-ins do go: each account's login is scoped to the config directory
//! being removed, so `warnings` counts them and says so.

use anyhow::Result;
use riabuild_paths::{Paths, contract_tilde};
use riabuild_ui::{Failure, Ui};
use std::path::{Path, PathBuf};

/// How the caller wants the removal done.
pub struct Request {
    /// `--yes`: do not ask.
    pub assume_yes: bool,
    /// `--check`: report and change nothing.
    pub dry_run: bool,
    /// Whether we are running inside the Clubria environment shell.
    pub inside_shell: bool,
}

/// What a reset is about to remove.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub root: PathBuf,
    /// Top-level names, sorted, so the developer sees what they are agreeing to.
    pub entries: Vec<String>,
}

/// `None` only when the directory is not there — a reset on a machine that was
/// never provisioned is not an error, it is a no-op worth saying out loud.
///
/// An *empty* directory is still a `Plan` with no entries. `main` creates
/// `~/.riabuild` before any task runs, so a first run that failed early leaves
/// one behind, and a reset that reported it missing would be contradicted by
/// `ls`.
pub async fn plan(paths: &dyn Paths) -> Option<Plan> {
    let root = paths.root();
    // `tokio::fs::read_dir` is a cursor rather than an iterator, so collecting
    // the names is a loop with an await in it, not a combinator chain.
    let mut cursor = tokio::fs::read_dir(&root).await.ok()?;
    let mut entries = Vec::new();
    while let Ok(Some(entry)) = cursor.next_entry().await {
        entries.push(entry.file_name().to_string_lossy().into_owned());
    }
    entries.sort();
    Some(Plan { root, entries })
}

pub async fn run(paths: &dyn Paths, ui: &Ui, request: Request) -> Result<i32> {
    if request.inside_shell {
        // This shell's PATH points at ~/.riabuild/bin and its rcfile is in
        // ~/.riabuild/shell. Removing those under it leaves a developer in a
        // shell where half the commands have vanished for no visible reason.
        return Err(Failure::new(
            "resetting riabuild from inside the Clubria environment shell",
            "type `exit` to leave the environment, then run `riabuild reset` again",
        )
        .into());
    }

    let home = paths.home();
    let Some(plan) = plan(paths).await else {
        ui.info(&format!(
            "Nothing to remove — {} is not there.",
            contract_tilde(&paths.root(), &home)
        ));
        return Ok(0);
    };

    // Checked before the developer is asked anything: a question riabuild would
    // refuse to act on is worse than no question.
    if !is_safe_to_remove(&plan.root, &home) {
        return Err(Failure::new(
            "removing riabuild's directory",
            "send this to your team lead — it is a bug in riabuild",
        )
        .detail(format!(
            "refusing to recursively remove {}, which is not below your home directory",
            plan.root.display()
        ))
        .into());
    }

    let shown = contract_tilde(&plan.root, &home);
    ui.info(&format!("riabuild reset will remove {shown}"));
    for line in warnings(&plan, &paths.claude_dir(), &paths.session_token_file()).await {
        ui.note(&line);
    }

    if request.dry_run {
        ui.info("");
        ui.info("Nothing was removed. Run `riabuild reset` to do it.");
        return Ok(0);
    }

    if !request.assume_yes {
        // Checked before asking, because `ask` returns None both for "they
        // chose the default" and for "there was nobody to ask", and a recursive
        // delete has to tell those apart.
        if !ui.interactive() {
            return Err(Failure::new(
                "asking whether to remove riabuild's directory",
                "re-run as `riabuild reset --yes` if you meant to remove it unattended",
            )
            .detail("riabuild has no terminal to ask on, and will not assume yes.")
            .into());
        }
        let answer = ui.ask(&format!("Remove {shown} and everything in it? [y/N]"));
        let confirmed = answer
            .is_some_and(|answer| matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"));
        if !confirmed {
            ui.info("Left alone.");
            return Ok(0);
        }
    }

    // Taken here rather than at the top, and this is the one destructive
    // command whose lock must *follow* its question: `acquire` waits for a
    // holder, and waiting before the prompt would leave a developer looking at
    // a silent terminal. Everything above this line only reads.
    //
    // Held across the removal, because the run this is racing is one unpacking
    // a toolchain into the tree about to disappear — the case the lock protocol
    // was written for and the one command that never took it. The lock file
    // lives inside `plan.root` and goes with it; the guard closes a descriptor
    // to an unlinked file, which is exactly what dropping it should do.
    let _provisioning = crate::lock::provisioning_lock(paths, ui, false).await?;

    tokio::fs::remove_dir_all(&plan.root)
        .await
        .map_err(|error| {
            Failure::new(
                format!("removing {shown}"),
                "check what still has a file open there, then run `riabuild reset` again",
            )
            .detail(error.to_string())
        })?;

    ui.info(&format!(
        "Removed {shown}. Run `riabuild` to set this machine up again."
    ));
    Ok(0)
}

/// What the developer is told before being asked.
///
/// Takes the tree it describes as arguments, so every claim can be tested. The
/// Claude Code history is the one item in the tree that is not reconstructible,
/// so it is named — but only when there are accounts actually holding some.
/// Warning about history a developer does not have teaches them to skim the
/// warning that matters. `claude_dir` is passed in rather than spelled out here,
/// because the layout belongs to `paths.rs`.
///
/// The accounts are *counted* rather than described as one profile: a developer
/// with four of them is agreeing to lose four sign-ins and four session
/// histories, and the singular understated that badly enough to read as a
/// different, smaller operation.
///
/// The sign-in claim is settled by looking for `session_token`, not by asking
/// which keychain this machine would use. Those answer the same question where
/// it matters — a machine with a keyring has no such file, and the two machines
/// that keep one (a managed server, and a laptop with no Secret Service
/// answering) keep it at exactly this path, inside the tree — and the file is
/// the honest test of the two: a machine that has never signed in loses nothing
/// however it would have stored a token. The line was unconditional, and on
/// those machines it told the developer their sign-in was safe while the delete
/// underneath it removed the session.
async fn warnings(plan: &Plan, claude_dir: &Path, session_token: &Path) -> Vec<String> {
    if plan.entries.is_empty() {
        return vec!["it is empty".to_string()];
    }
    let mut lines = vec![plan.entries.join(", ")];
    if plan
        .entries
        .iter()
        .any(|entry| plan.root.join(entry) == claude_dir)
    {
        // Counted on disk and not from `config.json`: a reset is for the machine
        // whose state file already disagrees with reality, so the directories
        // are the only account list worth believing here.
        let found = account_count(claude_dir).await;
        if found > 0 {
            lines.push(format!(
                "this includes the session history and sign-in of {}",
                riabuild_ui::plural(found, "Claude Code account")
            ));
        }
    }
    if tokio::fs::try_exists(session_token).await.unwrap_or(false) {
        lines.push("your checkout is not touched".to_string());
        lines.push(format!(
            "this machine has no keyring, so its riabuild sign-in is the file {} inside that \
             tree — removing it signs this machine out, and `riabuild` will ask you to sign in \
             again",
            session_token.display()
        ));
    } else {
        lines.push("your checkout and your riabuild sign-in are not touched".to_string());
    }
    lines
}

/// How many account directories `~/.riabuild/claude/` holds.
///
/// Deferred to `accounts::ids_on_disk` rather than walked here, so there is one
/// answer to "which directories under there are accounts". A second walk with
/// its own filter would eventually disagree with the first, and the disagreement
/// a developer would meet is a reset warning naming a number no `riabuild claude
/// list` agrees with. Only names `accounts::looks_like_id` accepts count: a
/// stray file, or a directory made by hand, is not an account.
async fn account_count(claude_dir: &Path) -> u64 {
    riabuild_tasks::accounts::ids_on_disk(claude_dir)
        .await
        .len() as u64
}

/// A last guard on a recursive delete driven by a trait a test can override.
/// `root` must live under the home directory and must not *be* it.
fn is_safe_to_remove(root: &Path, home: &Path) -> bool {
    root != home && root.starts_with(home) && root.file_name().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_tasks::testing::write_file;
    use tempfile::TempDir;

    /// A populated `~/.riabuild`, as a machine part-way through setup has it.
    async fn provisioned() -> (TempDir, RealPaths) {
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_file(&paths.state_file(), "{}").await;
        write_file(&paths.config_file(), "{}").await;
        write_file(
            &paths.node_dir("22.23.1").join("bin").join("node"),
            "#!/bin/sh",
        )
        .await;
        write_file(&paths.claude_dir().join("abc").join("history.jsonl"), "{}").await;
        write_file(&paths.log_file(), "ran\n").await;
        (home, paths)
    }

    fn quiet_ui() -> Ui {
        Ui::new(true)
    }

    #[tokio::test]
    async fn a_machine_that_was_never_provisioned_has_nothing_to_remove() {
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());

        assert_eq!(plan(&paths).await, None);
    }

    #[tokio::test]
    async fn an_empty_directory_is_still_removed() {
        // `main` creates ~/.riabuild before any task runs, so a first run that
        // failed early leaves an empty one behind. Telling that developer it is
        // "not there" while `ls` shows it there is a small lie, and a
        // provisioner that shades the truth about the machine is worth less
        // than one that says nothing.
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("create ~/.riabuild");

        let plan = plan(&paths).await.expect("an empty tree is still a tree");
        assert!(plan.entries.is_empty());

        run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: true,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("reset succeeds");

        assert!(!paths.root().exists(), "the empty directory is removed too");
    }

    #[tokio::test]
    async fn the_plan_names_what_the_developer_is_agreeing_to_lose() {
        let (_home, paths) = provisioned().await;

        let plan = plan(&paths)
            .await
            .expect("a provisioned tree has something to remove");

        assert_eq!(plan.root, paths.root());
        assert_eq!(
            plan.entries,
            vec!["claude", "config.json", "logs", "node", "state.json"]
        );
    }

    #[tokio::test]
    async fn confirmed_reset_removes_the_whole_tree() {
        let (_home, paths) = provisioned().await;

        let code = run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: true,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("reset succeeds");

        assert_eq!(code, 0);
        assert!(!paths.root().exists(), "~/.riabuild should be gone");
    }

    #[tokio::test]
    async fn reset_leaves_the_developers_own_home_alone() {
        let (home, paths) = provisioned().await;
        let sibling = home.path().join("code");
        write_file(&sibling.join("hub").join("README.md"), "# hub").await;

        run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: true,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("reset succeeds");

        assert!(
            sibling.join("hub").join("README.md").exists(),
            "the checkout lives outside ~/.riabuild and must survive"
        );
    }

    #[tokio::test]
    async fn check_mode_reports_without_removing_anything() {
        let (_home, paths) = provisioned().await;

        let code = run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: true,
                dry_run: true,
                inside_shell: false,
            },
        )
        .await
        .expect("a dry run succeeds");

        assert_eq!(code, 0);
        assert!(paths.root().exists(), "--check must change nothing");
    }

    #[tokio::test]
    async fn resetting_a_clean_machine_succeeds_quietly() {
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());

        let code = run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: true,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("nothing to remove is not a failure");

        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn reset_refuses_from_inside_the_environment_shell() {
        // The running shell has ~/.riabuild/bin on its PATH and its rcfile in
        // ~/.riabuild/shell. Pulling those out from under it leaves a developer
        // in a shell that half-works and cannot be explained.
        let (_home, paths) = provisioned().await;

        let error = run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: true,
                dry_run: false,
                inside_shell: true,
            },
        )
        .await
        .expect_err("reset from inside the environment shell is refused");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("refusal is an actionable failure");
        assert!(failure.action.contains("exit"));
        assert!(paths.root().exists(), "nothing is removed when refused");
    }

    #[tokio::test]
    async fn answering_yes_removes_the_tree() {
        let (_home, paths) = provisioned().await;

        run(
            &paths,
            &Ui::scripted(["y"]),
            Request {
                assume_yes: false,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("reset succeeds");

        assert!(!paths.root().exists());
    }

    #[tokio::test]
    async fn answering_no_leaves_the_tree_alone() {
        let (_home, paths) = provisioned().await;

        let code = run(
            &paths,
            &Ui::scripted(["n"]),
            Request {
                assume_yes: false,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("declining is not a failure");

        assert_eq!(code, 0);
        assert!(paths.root().exists(), "a declined reset removes nothing");
    }

    #[tokio::test]
    async fn pressing_enter_declines_rather_than_removing() {
        // A terminal that answers nothing is a developer choosing the default,
        // which for a recursive delete must be no. Distinct from having no
        // terminal at all, which is refused outright.
        let (_home, paths) = provisioned().await;

        let code = run(
            &paths,
            &Ui::scripted([]),
            Request {
                assume_yes: false,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("an empty answer is not a failure");

        assert_eq!(code, 0);
        assert!(paths.root().exists());
    }

    #[tokio::test]
    async fn without_a_terminal_reset_refuses_rather_than_assuming_yes() {
        // `cargo test` has no terminal on stdin, which is exactly the state a
        // script or a CI job is in.
        let (_home, paths) = provisioned().await;

        let error = run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: false,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect_err("an unattended reset is refused");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("refusal is an actionable failure");
        assert!(failure.action.contains("--yes"));
        assert!(paths.root().exists(), "nothing is removed when refused");
    }

    #[tokio::test]
    async fn a_machine_with_a_keyring_is_told_its_sign_in_is_safe() {
        // The laptop case: the token is in `security`/`secret-tool`, nothing
        // in the tree holds it, and the reset really does leave it alone.
        let (_home, paths) = provisioned().await;
        assert!(!paths.session_token_file().exists());

        let plan = plan(&paths).await.expect("a provisioned tree");
        let said = warnings(&plan, &paths.claude_dir(), &paths.session_token_file())
            .await
            .join("\n");

        assert!(said.contains("riabuild sign-in are not touched"), "{said}");
        assert!(!said.contains("signs this machine out"), "{said}");
    }

    #[tokio::test]
    async fn a_machine_with_no_keyring_is_told_the_reset_signs_it_out() {
        // A managed server, or a headless Linux box with no Secret Service
        // answering: the session token is `session.token` inside the tree
        // being removed. The warning used to promise the opposite, and it did
        // so on exactly the machines where signing in again is most awkward.
        let (_home, paths) = provisioned().await;
        write_file(&paths.session_token_file(), "rb_live_token").await;

        let plan = plan(&paths).await.expect("a provisioned tree");
        let said = warnings(&plan, &paths.claude_dir(), &paths.session_token_file())
            .await
            .join("\n");

        assert!(said.contains("signs this machine out"), "{said}");
        assert!(
            !said.contains("riabuild sign-in are not touched"),
            "the false claim must be gone, not merely joined by a true one: {said}"
        );
        assert!(said.contains("your checkout is not touched"), "{said}");
    }

    #[tokio::test]
    async fn the_reset_that_signs_a_machine_out_really_does_remove_the_token() {
        // The other half: the warning and the behaviour have to agree, so the
        // file the message names must actually be gone afterwards.
        let (_home, paths) = provisioned().await;
        write_file(&paths.session_token_file(), "rb_live_token").await;

        run(
            &paths,
            &quiet_ui(),
            Request {
                assume_yes: true,
                dry_run: false,
                inside_shell: false,
            },
        )
        .await
        .expect("reset succeeds");

        assert!(!paths.session_token_file().exists());
    }

    #[tokio::test]
    async fn the_plan_counts_the_accounts_a_reset_would_sign_out() {
        // Three accounts is three sign-ins and three session histories, and a
        // warning that says "your Clubria profile" understates that by a factor
        // of three. `provisioned` also leaves a non-UUID directory behind, which
        // is never an account and must not be counted as one.
        let (home, paths) = provisioned().await;
        for _ in 0..3 {
            let id = riabuild_tasks::accounts::new_id();
            tokio::fs::create_dir_all(paths.claude_profile_dir(&id))
                .await
                .expect("an account directory");
        }

        let plan = plan(&paths).await.expect("a provisioned tree");
        let said = warnings(&plan, &paths.claude_dir(), &paths.session_token_file())
            .await
            .join("\n");

        assert!(said.contains("3 Claude Code accounts"), "{said}");
        drop(home);
    }

    #[tokio::test]
    async fn one_account_is_counted_in_the_singular() {
        let (_home, paths) = provisioned().await;
        tokio::fs::create_dir_all(paths.claude_profile_dir(&riabuild_tasks::accounts::new_id()))
            .await
            .expect("an account directory");

        let plan = plan(&paths).await.expect("a provisioned tree");
        let said = warnings(&plan, &paths.claude_dir(), &paths.session_token_file())
            .await
            .join("\n");

        assert!(said.contains("1 Claude Code account"), "{said}");
        assert!(!said.contains("accounts"), "{said}");
    }

    #[tokio::test]
    async fn the_claude_history_is_only_named_when_there_is_an_account_to_lose() {
        let (_home, paths) = provisioned().await;
        let claude = paths.claude_dir();
        tokio::fs::create_dir_all(paths.claude_profile_dir(&riabuild_tasks::accounts::new_id()))
            .await
            .expect("an account directory");

        let with_accounts = Plan {
            root: paths.root(),
            entries: vec!["claude".into(), "state.json".into()],
        };
        let without = Plan {
            root: paths.root(),
            entries: vec!["state.json".into()],
        };

        let named = warnings(&with_accounts, &claude, &paths.session_token_file())
            .await
            .join("\n");
        assert!(named.contains("Claude Code account"), "{named}");

        // A tree with no `claude` entry has no history to warn about, however
        // many directories the account root turns out to hold.
        let silent = warnings(&without, &claude, &paths.session_token_file())
            .await
            .join("\n");
        assert!(!silent.contains("Claude Code account"), "{silent}");
    }

    #[tokio::test]
    async fn an_empty_tree_says_so_and_claims_nothing_else() {
        let paths = RealPaths::rooted_at("/Users/ada");
        let plan = Plan {
            root: paths.root(),
            entries: Vec::new(),
        };

        assert_eq!(
            warnings(&plan, &paths.claude_dir(), &paths.session_token_file()).await,
            vec!["it is empty"]
        );
    }

    #[test]
    fn a_root_that_is_not_below_home_is_never_removed() {
        let home = Path::new("/Users/ada");
        assert!(is_safe_to_remove(Path::new("/Users/ada/.riabuild"), home));
        assert!(!is_safe_to_remove(home, home));
        assert!(!is_safe_to_remove(Path::new("/"), home));
        assert!(!is_safe_to_remove(Path::new("/etc"), home));
    }
}

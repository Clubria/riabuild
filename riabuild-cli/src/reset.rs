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
//! does *not* touch is the developer's checkout (it lives outside the tree) and
//! their sign-in (the token is in the keychain, never here).

use crate::paths::{Paths, contract_tilde};
use crate::ui::{Failure, Ui};
use anyhow::Result;
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
    for line in warnings(&plan, &paths.claude_dir()) {
        ui.note(&line);
    }

    if request.dry_run {
        ui.info("");
        ui.info("Nothing was removed. Run `riabuild reset` to do it.");
        return Ok(0);
    }

    if !request.assume_yes {
        match ui.confirm(&format!("Remove {shown} and everything in it?")) {
            Some(true) => {}
            Some(false) => {
                ui.info("Left alone.");
                return Ok(0);
            }
            None => {
                return Err(Failure::new(
                    "asking whether to remove riabuild's directory",
                    "re-run as `riabuild reset --yes` if you meant to remove it unattended",
                )
                .detail("riabuild has no terminal to ask on, and will not assume yes.")
                .into());
            }
        }
    }

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
/// Pure, so the claims can be tested. The Claude Code history is the one item
/// in the tree that is not reconstructible, so it is named — but only when the
/// profile is actually there. Warning about history a developer does not have
/// teaches them to skim the warning that matters. `claude_dir` is passed in
/// rather than spelled out here, because the layout belongs to `paths.rs`.
fn warnings(plan: &Plan, claude_dir: &Path) -> Vec<String> {
    if plan.entries.is_empty() {
        return vec!["it is empty".to_string()];
    }
    let mut lines = vec![plan.entries.join(", ")];
    if plan
        .entries
        .iter()
        .any(|entry| plan.root.join(entry) == claude_dir)
    {
        lines
            .push("this includes the Claude Code history kept in your Clubria profile".to_string());
    }
    lines.push("your checkout and your riabuild sign-in are not touched".to_string());
    lines
}

/// A last guard on a recursive delete driven by a trait a test can override.
/// `root` must live under the home directory and must not *be* it.
fn is_safe_to_remove(root: &Path, home: &Path) -> bool {
    root != home && root.starts_with(home) && root.file_name().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RealPaths;
    use crate::testing::write_file;
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

    #[test]
    fn the_claude_history_is_only_named_when_there_is_a_profile_to_lose() {
        let paths = RealPaths::rooted_at("/Users/ada");
        let claude = paths.claude_dir();

        let with_profile = Plan {
            root: paths.root(),
            entries: vec!["claude".into(), "state.json".into()],
        };
        let without = Plan {
            root: paths.root(),
            entries: vec!["state.json".into()],
        };

        let names_history = |plan: &Plan| {
            warnings(plan, &claude)
                .iter()
                .any(|line| line.contains("Claude Code history"))
        };
        assert!(names_history(&with_profile));
        assert!(
            !names_history(&without),
            "a tree with no profile in it has no history to warn about"
        );
    }

    #[test]
    fn an_empty_tree_says_so_and_claims_nothing_else() {
        let paths = RealPaths::rooted_at("/Users/ada");
        let plan = Plan {
            root: paths.root(),
            entries: Vec::new(),
        };

        assert_eq!(warnings(&plan, &paths.claude_dir()), vec!["it is empty"]);
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

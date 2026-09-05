//! What the default flow says about the run — to the developer, and to the log.
//!
//! Three of these print before a task has done anything, because "riabuild is
//! using the wrong account" and "riabuild is provisioning the wrong
//! repository" are otherwise invisible until something fails for a confusing
//! reason. The fourth writes the line a team lead asks for afterwards.

use riabuild_paths::config;
use riabuild_tasks::{Ctx, engine};
use riabuild_ui as ui;

/// What a `--check` found, in one sentence.
///
/// `failed` and `skipped` are here because a dry run has both: a `check()` that
/// errors is a question riabuild could not put to the machine at all, and
/// everything downstream of one is never asked. Counting only `satisfied` and
/// `applied` made a run that could not look at half the machine report a
/// shorter to-do list than a run that could.
pub(super) fn dry_run_summary(outcome: &engine::Outcome) -> String {
    // "9 item(s) already correct, 0 would be set up." made a fine machine read
    // like a to-do list. The all-clear deserves to say so plainly.
    if outcome.applied.is_empty() && outcome.failed.is_empty() && outcome.skipped.is_empty() {
        return "Everything on this machine is already set up.".to_string();
    }
    let mut parts = vec![
        format!(
            "{} already correct",
            ui::plural(outcome.satisfied.len() as u64, "item")
        ),
        format!("{} still to set up", outcome.applied.len()),
    ];
    if !outcome.failed.is_empty() {
        parts.push(format!("{} riabuild could not check", outcome.failed.len()));
    }
    if !outcome.skipped.is_empty() {
        parts.push(format!("{} it never got to", outcome.skipped.len()));
    }
    format!("{}.", parts.join(", "))
}

/// Who riabuild thinks this machine belongs to, and where the token lives.
///
/// Printed on every run because "riabuild is using the wrong account" is
/// otherwise invisible until something fails for a confusing reason.
pub(super) fn describe_session(ctx: &Ctx) {
    let Some(member) = &ctx.member else {
        ctx.ui
            .note("not signed in yet — riabuild will give you a code to approve");
        return;
    };
    ctx.ui.note(&format!(
        "signed in as {} <{}> · {} · token in {}",
        member.display_name(),
        member.email,
        member.role,
        ctx.keychain.describe(),
    ));
}

/// Which repository this run is about, and where its checkout is.
///
/// Printed for the same reason `describe_session` is: with more than one
/// repository in play, "riabuild is working on the wrong one" is otherwise
/// invisible until a task says something that reads as a fault.
pub(super) fn describe_repo(ctx: &Ctx) {
    let Ok(repo) = ctx.repo() else {
        return;
    };
    let home = ctx.paths.home();
    let checkout = match ctx.project_dir() {
        Some(dir) => riabuild_paths::contract_tilde(&dir, &home),
        None => "not cloned yet".to_string(),
    };
    // The whole of what a developer is told about a pin, and it is told on the
    // run they answer `Always use …?` and on every run after — because a run
    // that honours one draws no box, and this line stands where the box was.
    // Said here rather than in the picker so there is one line about which
    // repository this run is about, not two: `repo::pick::choose` deliberately
    // says nothing.
    let always = match ctx.config.always_repo.as_deref() == Some(repo.slug()) {
        true => " · always — `riabuild --repo` asks again",
        false => "",
    };
    ctx.ui
        .note(&format!("working on {repo} · {checkout}{always}"));
}

/// One line per run in `~/.riabuild/logs/riabuild.log`.
///
/// Deliberately never fatal: failing to write a log must not fail a setup that
/// otherwise worked. It exists so "send me your riabuild log" is a useful thing
/// for a team lead to ask.
///
/// A run that failed is logged too — `after_the_tasks` reaches here whatever
/// the tasks did — so the line names what failed and what was never attempted
/// behind it. A log that only ever records the good runs is one the lead is
/// asking for on exactly the machine it has nothing to say about.
pub(super) async fn log_run(ctx: &Ctx, outcome: &engine::Outcome) {
    use tokio::io::AsyncWriteExt;
    let path = ctx.paths.log_file();
    let Some(parent) = path.parent() else { return };
    if tokio::fs::create_dir_all(parent).await.is_err() {
        return;
    }
    let line = format!(
        "{} riabuild {} satisfied={} applied=[{}] failed=[{}] skipped=[{}]\n",
        config::now_secs(),
        ctx.cli_version,
        outcome.satisfied.len(),
        outcome.applied.join(","),
        outcome.failed.join(","),
        outcome.skipped.join(","),
    );
    if let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        // write_all reporting success only means the bytes reached
        // tokio::fs::File's internal buffer, not that the background
        // write() syscall ran — flush is what waits for that.
        let _ = async {
            file.write_all(line.as_bytes()).await?;
            file.flush().await
        }
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--check` counts what it could not check, not just what it could.
    ///
    /// A `check()` that errors is a question riabuild could not put to the
    /// machine at all, and everything downstream of one is never asked. Left
    /// out of the count, a run that looked at half the machine reported a
    /// *shorter* to-do list than one that looked at all of it.
    #[test]
    fn a_dry_run_summary_counts_what_it_could_not_check() {
        assert_eq!(
            dry_run_summary(&engine::Outcome {
                satisfied: vec!["login", "github_cli"],
                applied: vec!["toolchain"],
                failed: vec!["claude_accounts"],
                skipped: vec!["claude_trust", "claude_plugins"],
            }),
            "2 items already correct, 1 still to set up, 1 riabuild could not check, \
             2 it never got to."
        );
    }

    #[test]
    fn a_dry_run_that_found_nothing_wrong_still_says_so_plainly() {
        assert_eq!(
            dry_run_summary(&engine::Outcome {
                satisfied: vec!["login"],
                ..Default::default()
            }),
            "Everything on this machine is already set up."
        );
    }

    /// A machine with nothing to do but something riabuild could not look at is
    /// not "already set up".
    #[test]
    fn a_dry_run_that_could_not_check_is_never_reported_as_all_clear() {
        let summary = dry_run_summary(&engine::Outcome {
            satisfied: vec!["login"],
            failed: vec!["toolchain"],
            ..Default::default()
        });
        assert!(summary.contains("could not check"), "{summary}");
    }

    /// The line that stands where the box used to be.
    ///
    /// A run that honours a pin asks nothing and draws nothing, so this is the
    /// only place the developer is told they have stopped being asked — and
    /// the only place the way back is named.
    #[tokio::test]
    async fn a_pinned_machine_is_told_how_to_be_asked_again() {
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = riabuild_ui::Ui::new(false);
        ctx.repo = Some(riabuild_api::Repo::parse("Clubria/payments").expect("parses"));

        describe_repo(&ctx);
        assert!(
            ctx.ui.noted().iter().all(|note| !note.contains("always")),
            "a machine that has not pinned is told nothing about pins: {:?}",
            ctx.ui.noted()
        );

        ctx.update_config(|config| config.always_repo = Some("Clubria/payments".into()))
            .await
            .expect("write");
        describe_repo(&ctx);

        let said = ctx.ui.noted().last().cloned().unwrap_or_default();
        assert!(said.contains("working on Clubria/payments"), "{said}");
        assert!(said.contains("always"), "{said}");
        assert!(said.contains("riabuild --repo"), "{said}");
    }

    /// …and never about the repository this run is *not* about.
    #[tokio::test]
    async fn a_pin_naming_another_repository_says_nothing_here() {
        // `riabuild --repo payments` on a machine pinned elsewhere is a run
        // about `payments`, and `adopt_named` repoints the pin — but a `--check`
        // writes nothing, so the two really can disagree while this line is
        // printed. Claiming "always" about a repository the pin does not name
        // would be the one sentence on screen that is false.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = riabuild_ui::Ui::new(false);
        ctx.repo = Some(riabuild_api::Repo::parse("Clubria/payments").expect("parses"));
        ctx.update_config(|config| config.always_repo = Some("Clubria/ai-builders-hub".into()))
            .await
            .expect("write");

        describe_repo(&ctx);

        let said = ctx.ui.noted().last().cloned().unwrap_or_default();
        assert!(said.contains("working on Clubria/payments"), "{said}");
        assert!(!said.contains("always"), "{said}");
    }
}

//! Task 6 — report on the checkout. **Never changes it.**
//!
//! `git pull` on every launch fails loudly on dirty trees, detached HEAD and
//! mid-conflict states. Startup is the worst possible moment for that, and a
//! provisioner that mangles someone's work in progress is worse than one that
//! says nothing. riabuild reports drift and lets the developer decide.
//!
//! Because it only reports, `check()` is where the work happens and `apply()`
//! has nothing to do.

use super::{Ctx, Status, Task, TaskId};
use crate::runner::RunOptions;
use anyhow::Result;
use std::path::Path;

pub struct RepoStatus;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: u32,
    pub detached: bool,
}

impl Report {
    pub fn is_clean_and_current(&self) -> bool {
        self.ahead == 0 && self.behind == 0 && self.dirty == 0 && !self.detached
    }

    /// One line a developer can act on without reading git documentation.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.detached {
            parts.push("not on a branch".to_string());
        }
        if self.behind > 0 {
            parts.push(format!("{} commit(s) behind origin", self.behind));
        }
        if self.ahead > 0 {
            parts.push(format!("{} commit(s) not pushed", self.ahead));
        }
        if self.dirty > 0 {
            parts.push(format!("{} file(s) with local changes", self.dirty));
        }
        if parts.is_empty() {
            return format!("{} is clean and up to date", self.branch);
        }
        format!("{}: {}", self.branch, parts.join(", "))
    }
}

/// Parses the first line of `git status --porcelain=v2 --branch`.
pub fn parse_status(output: &str) -> Report {
    let mut report = Report {
        branch: "HEAD".to_string(),
        ..Default::default()
    };

    for line in output.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            report.branch = head.trim().to_string();
            report.detached = head.trim() == "(detached)";
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut parts = ab.split_whitespace();
            report.ahead = parts
                .next()
                .and_then(|value| value.trim_start_matches('+').parse().ok())
                .unwrap_or(0);
            report.behind = parts
                .next()
                .and_then(|value| value.trim_start_matches('-').parse().ok())
                .unwrap_or(0);
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            report.dirty += 1;
        }
    }
    report
}

fn gather(ctx: &Ctx, dir: &Path) -> Result<Report> {
    let output = ctx.runner.run(
        "git",
        &[
            "-C",
            &dir.to_string_lossy(),
            "status",
            "--porcelain=v2",
            "--branch",
        ],
        &RunOptions::default(),
    )?;
    Ok(parse_status(&output.stdout))
}

impl Task for RepoStatus {
    fn id(&self) -> TaskId {
        "repo_status"
    }

    fn title(&self) -> &str {
        "Checkout status"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &["project"]
    }

    fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::Satisfied);
        };
        if !dir.join(".git").exists() {
            return Ok(Status::Satisfied);
        }

        let report = gather(ctx, &dir)?;
        if !report.is_clean_and_current() {
            ctx.ui.note(&report.describe());
        }
        // Always satisfied: there is no state to repair, only news to deliver.
        Ok(Status::Satisfied)
    }

    fn apply(&self, _ctx: &mut Ctx) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};
    use std::sync::Arc;

    #[test]
    fn reads_a_clean_checkout() {
        let report = parse_status(
            "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
        );
        assert_eq!(report.branch, "main");
        assert!(report.is_clean_and_current());
        assert_eq!(report.describe(), "main is clean and up to date");
    }

    #[test]
    fn counts_drift_in_both_directions() {
        let report = parse_status(
            "# branch.head feature\n# branch.ab +2 -5\n1 .M N... 100644 100644 100644 aaa bbb src/a.rs\n1 .M N... 100644 100644 100644 ccc ddd src/b.rs\n",
        );
        assert_eq!(report.ahead, 2);
        assert_eq!(report.behind, 5);
        assert_eq!(report.dirty, 2);
        let described = report.describe();
        assert!(described.contains("5 commit(s) behind"), "{described}");
        assert!(
            described.contains("2 file(s) with local changes"),
            "{described}"
        );
    }

    #[test]
    fn notices_a_detached_head() {
        let report = parse_status("# branch.head (detached)\n");
        assert!(report.detached);
        assert!(report.describe().contains("not on a branch"));
    }

    #[test]
    fn a_dirty_checkout_is_still_satisfied_because_this_task_only_reports() {
        let (mut ctx, home) = ctx_with(FakeRunner::new());
        let dir = home.path().join("code/hub");
        write_file(&dir.join(".git/HEAD"), "ref: refs/heads/main\n");
        ctx.config.project_path = Some(dir.to_string_lossy().into());
        ctx.runner = Arc::new(FakeRunner::new().with(
            "git -C",
            0,
            "# branch.head main\n# branch.ab +0 -3\n1 .M N... 1 2 3 a b src/x.rs\n",
            "",
        ));

        // Never Needs: applying would mean pulling, and pulling at startup is
        // exactly what this task exists to avoid.
        assert_eq!(RepoStatus.check(&ctx).unwrap(), Status::Satisfied);
    }

    #[test]
    fn apply_touches_nothing() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new());
        RepoStatus.apply(&mut ctx).unwrap();
        let calls = ctx
            .runner
            .run("noop", &[], &RunOptions::default())
            .map(|_| ())
            .is_ok();
        assert!(calls);
    }
}

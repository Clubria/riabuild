//! What a run remembers about the tasks it could not finish.

use super::Outcome;
use crate::{Ctx, Task, TaskId};
use anyhow::Result;
use riabuild_ui::{Detail, Failure};
use std::collections::HashSet;

/// The failures a run carried on past.
///
/// One task failing is not a reason to leave the rest of the machine alone: a
/// developer who walked away from the Claude sign-in should still come back to
/// a Codex, a checkout and a toolchain. What it *is* a reason for is not
/// running anything downstream of it — a task whose prerequisite failed would
/// be working against a machine that is not in the state it was promised.
///
/// Which is what `stopped` holds: failed tasks and, transitively, the tasks
/// skipped behind them. This is the only thing that decides what does not run,
/// and it is read from `depends_on()` — so "what must not wait behind what" is
/// a declared edge rather than a position in `registry()`, which is what it
/// used to be.
#[derive(Default)]
pub(super) struct Carried {
    stopped: HashSet<TaskId>,
    reported: Vec<(String, anyhow::Error)>,
}

impl Carried {
    /// The dependency of `task` that did not finish, if one did not.
    pub(super) fn blocker(&self, task: &dyn Task) -> Option<TaskId> {
        task.depends_on()
            .iter()
            .copied()
            .find(|dependency| self.stopped.contains(dependency))
    }

    /// A task that failed: said out loud where it happened, and remembered for
    /// the end of the run and for everything downstream of it.
    pub(super) fn failed(
        &mut self,
        task: &dyn Task,
        error: anyhow::Error,
        ctx: &Ctx,
        outcome: &mut Outcome,
    ) {
        // `unresolved` rather than `warn`: it covers the `◐` line this task is
        // sitting on, so a failed task resolves to `▲` instead of staying busy
        // for the rest of the run.
        ctx.ui.unresolved(
            task.title(),
            "could not be set up",
            &[Detail::Prose(&format!("{error}"))],
        );
        self.stopped.insert(task.id());
        self.reported.push((task.title().to_string(), error));
        outcome.failed.push(task.id());
    }

    /// A task riabuild did not attempt, because `blocker` did not finish.
    pub(super) fn skipped(
        &mut self,
        task: &dyn Task,
        blocker: TaskId,
        ctx: &Ctx,
        outcome: &mut Outcome,
    ) {
        ctx.ui.unresolved(
            task.title(),
            &format!("not attempted — {blocker} did not finish"),
            &[],
        );
        self.stopped.insert(task.id());
        outcome.skipped.push(task.id());
    }

    /// What the run reports at the end, or `Ok(())` when nothing failed.
    ///
    /// One failure is passed through exactly as it arrived. That is not a
    /// special case for tidiness: a task's own `Failure` carries the remedy
    /// that applies — accept the invite, refresh the token — and a wrapper
    /// around it would replace the one sentence the developer can act on with
    /// a generic one. More than one has no single remedy, so it names each.
    pub(super) fn into_result(mut self) -> Result<()> {
        if self.reported.len() == 1 {
            let (_, error) = self.reported.remove(0);
            return Err(error);
        }
        if self.reported.is_empty() {
            return Ok(());
        }
        let detail = self
            .reported
            .iter()
            .map(|(title, error)| format!("{title}: {error}"))
            .collect::<Vec<_>>()
            .join("\n");
        Err(Failure::new(
            format!(
                "{} could not be set up",
                riabuild_ui::plural(self.reported.len() as u64, "task")
            ),
            "Deal with each of these and run `riabuild` again. Everything else on this \
             machine is already set up.",
        )
        .detail(detail)
        .into())
    }
}

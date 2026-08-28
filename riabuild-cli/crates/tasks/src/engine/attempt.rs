//! One task's turn: check → apply → re-check, against whichever `Ctx` it was
//! handed.
//!
//! Lifted out of the run loop unchanged in substance, because it now has two
//! callers rather than one: a task that needs the developer gets the run's own
//! `Ctx` and prints as it goes, and every other task gets a fork whose `Ui`
//! records instead. Nothing in here knows which it has, which is the property
//! that keeps the two paths from drifting — the concurrent one is the
//! sequential one with a different `Ctx` behind it.
//!
//! It reports rather than decides. What a finished attempt *means* to the run —
//! what is recorded, what is skipped behind it, what the exit status is — is
//! `engine::settle` and `Carried`, on the run's own `Ctx`, in declaration
//! order.

use super::status_for;
use crate::{Ctx, Status, Task, TaskId};
use riabuild_ui::Failure;
use std::collections::HashSet;

/// What one task's turn came to.
pub(super) enum Attempt {
    /// The machine was already in shape.
    Satisfied,
    /// Work was needed, and it was done — or, under `--check`, would have been.
    Ran,
    /// This task could not be finished. The run carries on past it and skips
    /// whatever declared a dependency on it.
    Failed(anyhow::Error),
    /// The run itself cannot continue: `state.json` could not be written, so
    /// riabuild can no longer remember what it has done. Propagated out of
    /// `run_into` with `?`, which is what a failed state write did before there
    /// were two kinds of failure to tell apart.
    Fatal(anyhow::Error),
}

pub(super) async fn run(task: &dyn Task, ctx: &mut Ctx, applied: &HashSet<TaskId>) -> Attempt {
    let status = match status_for(task, ctx, applied).await {
        Ok(status) => status,
        // A `check()` that errors is not the same as one that says work is
        // needed — it is a question riabuild could not put to the machine at
        // all — but it is the same *kind* of failure to a run: nothing
        // downstream of it can be trusted, and everything beside it still can.
        Err(error) => return Attempt::Failed(error),
    };

    let reason = match status {
        Status::Satisfied => {
            ctx.ui.satisfied(task.title());
            // Record a task that was already in shape the first time riabuild
            // saw this machine. Nothing was applied, but a recordless task is
            // invisible to the `version()` escape hatch — the forced rerun for
            // drift `check()` cannot observe — so leaving it unrecorded would
            // quietly exempt a server from every future version bump.
            //
            // Never under `--check`, which reports and changes nothing — and
            // `state.json` is part of "nothing". The macOS end-to-end suite
            // caught this: a dry run that recorded `repo_status` left the
            // machine different from how it found it, which is the one thing a
            // dry run may not do.
            if !ctx.dry_run && !ctx.state.tasks.contains_key(task.id()) {
                let (id, version) = (task.id(), task.version());
                if let Err(error) = ctx
                    .update_state(|state| state.mark_satisfied(id, version, "already_satisfied"))
                    .await
                {
                    return Attempt::Fatal(error);
                }
            }
            return Attempt::Satisfied;
        }
        Status::Needs(reason) => reason,
    };

    if ctx.dry_run {
        ctx.ui.warn(&format!(
            "{} would run — {}",
            task.title(),
            reason.describe()
        ));
        // Reported as `Ran` *for this run's purposes* as well as printed. A
        // real run would apply this task, and applying it is what makes every
        // dependent re-run — so a dry run that left the set empty answered a
        // different question than the one asked: on a machine missing Node it
        // reported the tasks behind `toolchain` satisfied, when a real run
        // would have re-run every one of them.
        return Attempt::Ran;
    }

    ctx.ui.working(task.title(), &reason.describe());
    if let Err(error) = task.apply(ctx).await {
        return Attempt::Failed(error);
    }

    // The whole point: never record a success we have not verified.
    match task.check(ctx).await {
        Ok(Status::Satisfied) => {}
        Ok(Status::Needs(still)) => {
            let error = Failure::new(
                format!("{} (it did not take effect)", task.title()),
                "Run `riabuild` again; if it keeps failing, send this message to your team lead.",
            )
            .detail(format!(
                "after setting it up, riabuild re-checked and found: {}",
                still.describe()
            ));
            return Attempt::Failed(error.into());
        }
        Err(error) => return Attempt::Failed(error),
    }

    ctx.ui.applied(task.title());
    let (id, version, tag) = (task.id(), task.version(), reason.tag());
    if let Err(error) = ctx
        .update_state(|state| state.mark_satisfied(id, version, &tag))
        .await
    {
        return Attempt::Fatal(error);
    }
    Attempt::Ran
}

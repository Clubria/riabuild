//! Whether one task needs to run, without running anything.

use crate::{Ctx, Reason, Status, Task, TaskId};
use anyhow::Result;
use std::collections::HashSet;

/// Decides whether a task needs to run, without running anything.
pub async fn status_for(task: &dyn Task, ctx: &Ctx, applied: &HashSet<TaskId>) -> Result<Status> {
    let record = ctx.state.tasks.get(task.id());

    let Some(record) = record else {
        // No record means this riabuild has never run here. It does *not* mean
        // the machine still needs the work: a state file is riabuild's memory,
        // not the machine's state, and something other than a previous run can
        // have put the machine in the desired shape already. `riabuild remote`
        // is exactly that — it writes a server's session token into the
        // server's namespace before the server's own riabuild has ever
        // started, so `login` arrives at its first run already signed in.
        //
        // Applying anyway used to cost a second browser round trip on every
        // new server: the laptop minted a session, and the server then asked
        // the developer to approve a *second* device code for the token it was
        // already holding. `check()` is authoritative — the invariant in
        // riabuild-cli/CLAUDE.md — and skipping it here was the one place that
        // was not true.
        //
        // "first run" stays the reason when work *is* needed: it is the honest
        // and more useful explanation of why, and keeps `last_reason` stable
        // in state.json for the run after it.
        return Ok(match task.check(ctx).await? {
            Status::Satisfied => Status::Satisfied,
            Status::Needs(_) => Status::Needs(Reason::NeverRun),
        });
    };

    if record.version != task.version() {
        return Ok(Status::Needs(Reason::VersionChanged {
            from: record.version,
            to: task.version(),
        }));
    }

    for dependency in task.depends_on() {
        if applied.contains(dependency) {
            return Ok(Status::Needs(Reason::UpstreamChanged(dependency)));
        }
    }

    task.check(ctx).await
}

//! The task runner: topological order, then check → apply → re-check.

use super::{Ctx, Reason, Status, Task, TaskId};
use anyhow::{Result, anyhow};
use riabuild_ui::Failure;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Default)]
pub struct Outcome {
    pub satisfied: Vec<TaskId>,
    pub applied: Vec<TaskId>,
}

/// Orders tasks into dependency *waves*: every task in a wave has all of its
/// dependencies satisfied by earlier waves.
///
/// The loop below already computed these waves and then flattened them away.
/// Keeping them costs nothing and means the graph still knows its own shape, so
/// running a wave concurrently later becomes a change to the runner rather than
/// a rewrite of the ordering. Execution today is still strictly sequential.
///
/// Returns an error for a cycle or for an edge naming a task that is not
/// registered — both are programming mistakes that must fail loudly in tests
/// rather than quietly reorder someone's setup.
pub fn topological_order(tasks: &[Box<dyn Task>]) -> Result<Vec<Vec<usize>>> {
    let index: HashMap<TaskId, usize> = tasks
        .iter()
        .enumerate()
        .map(|(position, task)| (task.id(), position))
        .collect();

    if index.len() != tasks.len() {
        return Err(anyhow!("two tasks share an id"));
    }

    let mut remaining: BTreeSet<usize> = (0..tasks.len()).collect();
    let mut done: HashSet<TaskId> = HashSet::new();
    let mut waves: Vec<Vec<usize>> = Vec::new();

    for task in tasks {
        for dependency in task.depends_on() {
            if !index.contains_key(dependency) {
                return Err(anyhow!(
                    "task `{}` depends on `{dependency}`, which is not registered",
                    task.id()
                ));
            }
        }
    }

    while !remaining.is_empty() {
        // BTreeSet keeps this deterministic: the same graph always produces the
        // same order, so the output a developer sees does not shuffle.
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|position| {
                tasks[*position]
                    .depends_on()
                    .iter()
                    .all(|dependency| done.contains(dependency))
            })
            .collect();

        if ready.is_empty() {
            let stuck: Vec<&str> = remaining.iter().map(|p| tasks[*p].id()).collect();
            return Err(anyhow!(
                "these tasks depend on each other in a cycle: {}",
                stuck.join(", ")
            ));
        }

        for position in &ready {
            remaining.remove(position);
            done.insert(tasks[*position].id());
        }
        waves.push(ready);
    }

    Ok(waves)
}

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

pub async fn run_all(tasks: &[Box<dyn Task>], ctx: &mut Ctx) -> Result<Outcome> {
    let order = topological_order(tasks)?;
    let mut applied: HashSet<TaskId> = HashSet::new();
    let mut outcome = Outcome::default();

    for position in order.into_iter().flatten() {
        let task = tasks[position].as_ref();
        let status = status_for(task, ctx, &applied).await?;

        let reason = match status {
            Status::Satisfied => {
                ctx.ui.satisfied(task.title());
                // Record a task that was already in shape the first time
                // riabuild saw this machine. Nothing was applied, but a
                // recordless task is invisible to the `version()` escape
                // hatch — the forced rerun for drift `check()` cannot observe
                // — so leaving it unrecorded would quietly exempt a server
                // from every future version bump.
                //
                // Never under `--check`, which reports and changes nothing —
                // and `state.json` is part of "nothing". The macOS end-to-end
                // suite caught this: a dry run that recorded `repo_status`
                // left the machine different from how it found it, which is
                // the one thing a dry run may not do.
                if !ctx.dry_run && !ctx.state.tasks.contains_key(task.id()) {
                    let (id, version) = (task.id(), task.version());
                    ctx.update_state(|state| {
                        state.mark_satisfied(id, version, "already_satisfied")
                    })
                    .await?;
                }
                outcome.satisfied.push(task.id());
                continue;
            }
            Status::Needs(reason) => reason,
        };

        if ctx.dry_run {
            ctx.ui.warn(&format!(
                "{} would run — {}",
                task.title(),
                reason.describe()
            ));
            outcome.applied.push(task.id());
            continue;
        }

        ctx.ui.working(task.title(), &reason.describe());
        task.apply(ctx).await?;

        // The whole point: never record a success we have not verified.
        match task.check(ctx).await? {
            Status::Satisfied => {}
            Status::Needs(still) => {
                return Err(Failure::new(
                    format!("{} (it did not take effect)", task.title()),
                    "Run `riabuild` again; if it keeps failing, send this message to your team lead.",
                )
                .detail(format!(
                    "after setting it up, riabuild re-checked and found: {}",
                    still.describe()
                ))
                .into());
            }
        }

        ctx.ui.applied(task.title());
        let tag = reason.tag();
        ctx.update_state(|state| state.mark_satisfied(task.id(), task.version(), &tag))
            .await?;
        applied.insert(task.id());
        outcome.applied.push(task.id());
    }

    // Not a redundant no-op write: `State::load` drops records for tasks
    // riabuild no longer has, and this is the write that makes that dropping
    // stick. `a_dropped_record_does_not_come_back_on_the_next_save` in
    // `config.rs` is the test that fails if it goes.
    ctx.update_state(|_| {}).await?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;
    use crate::testing::test_ctx;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct Fake {
        id: TaskId,
        deps: Vec<TaskId>,
        version: u32,
        /// Statuses returned by successive `check()` calls.
        checks: Mutex<Vec<Status>>,
        applies: Arc<Mutex<u32>>,
    }

    impl Fake {
        fn new(id: TaskId, deps: Vec<TaskId>, checks: Vec<Status>) -> Self {
            Self {
                id,
                deps,
                version: 1,
                checks: Mutex::new(checks),
                applies: Arc::new(Mutex::new(0)),
            }
        }
    }

    #[async_trait]
    impl Task for Fake {
        fn id(&self) -> TaskId {
            self.id
        }
        fn title(&self) -> &str {
            self.id
        }
        fn version(&self) -> u32 {
            self.version
        }
        fn depends_on(&self) -> &[TaskId] {
            &self.deps
        }
        async fn check(&self, _ctx: &Ctx) -> Result<Status> {
            let mut checks = self.checks.lock().unwrap();
            if checks.is_empty() {
                return Ok(Status::Satisfied);
            }
            Ok(checks.remove(0))
        }
        async fn apply(&self, _ctx: &mut Ctx) -> Result<()> {
            *self.applies.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[test]
    fn independent_tasks_land_in_the_same_wave() {
        // `a` and `b` depend on nothing, `c` depends on both: two waves.
        //
        // Execution is still strictly sequential — this only stops the graph
        // from discarding structure it already computes, so that running a wave
        // concurrently later is a change to the runner rather than a rewrite of
        // the ordering.
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::new("a", vec![], vec![])),
            Box::new(Fake::new("b", vec![], vec![])),
            Box::new(Fake::new("c", vec!["a", "b"], vec![])),
        ];

        let waves = topological_order(&tasks).unwrap();

        assert_eq!(
            waves.len(),
            2,
            "a and b are independent and belong together"
        );
        assert_eq!(waves[0], vec![0, 1]);
        assert_eq!(waves[1], vec![2]);
    }

    #[test]
    fn the_real_graph_is_acyclic_and_fully_declared() {
        // The test the design asks for: every declared edge names a registered
        // task, and the graph can actually be ordered.
        let tasks = registry();
        let waves = topological_order(&tasks).expect("registry must be a DAG");
        assert_eq!(waves.iter().flatten().count(), tasks.len());

        // Checked wave by wave rather than over the flattened order: a
        // dependency must be satisfied by an *earlier* wave, not merely earlier
        // in the list. Flattening first would let two tasks in the same wave
        // look correctly ordered when they are meant to be independent.
        let mut seen: HashSet<TaskId> = HashSet::new();
        for wave in waves {
            for position in &wave {
                for dependency in tasks[*position].depends_on() {
                    assert!(
                        seen.contains(dependency),
                        "{} ran before its dependency {dependency}",
                        tasks[*position].id()
                    );
                }
            }
            for position in &wave {
                seen.insert(tasks[*position].id());
            }
        }
    }

    #[test]
    fn a_cycle_is_rejected() {
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::new("a", vec!["b"], vec![])),
            Box::new(Fake::new("b", vec!["a"], vec![])),
        ];
        let error = topological_order(&tasks).unwrap_err().to_string();
        assert!(error.contains("cycle"), "{error}");
    }

    #[test]
    fn an_undeclared_dependency_is_rejected() {
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(Fake::new("a", vec!["ghost"], vec![]))];
        let error = topological_order(&tasks).unwrap_err().to_string();
        assert!(error.contains("not registered"), "{error}");
    }

    #[tokio::test]
    async fn a_first_run_applies_everything_and_records_it() {
        let (mut ctx, _home) = test_ctx().await;
        // Two statuses: with no record in state.json the engine still asks
        // `check()` — the machine may already be in shape — and only the first
        // answer here says otherwise. The second is the verifying re-check
        // after `apply()`.
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(Fake::new(
            "a",
            vec![],
            vec![Status::needs("nothing here yet"), Status::Satisfied],
        ))];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();
        assert_eq!(outcome.applied, vec!["a"]);
        assert_eq!(ctx.state.tasks["a"].version, 1);
        // Still "first run", not the check's own words: with no record that is
        // both true and the more useful thing to have said.
        assert_eq!(ctx.state.tasks["a"].last_reason, "never_run");
    }

    #[tokio::test]
    async fn a_first_run_on_a_machine_already_in_shape_applies_nothing() {
        // The regression test for a `riabuild remote` that asked for two
        // sign-ins on a new server. The laptop writes the server's session
        // token into its namespace before the server's riabuild ever runs, so
        // `login` reaches its first run already satisfied — and applying on
        // the strength of an empty state.json alone made the developer approve
        // a second device code for a token the server was already holding.
        let (mut ctx, _home) = test_ctx().await;
        let task = Fake::new("a", vec![], vec![Status::Satisfied]);
        let applies = task.applies.clone();
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(task)];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();
        assert_eq!(outcome.satisfied, vec!["a"]);
        assert_eq!(*applies.lock().unwrap(), 0);
        // Recorded even though nothing ran, so a later `version()` bump can
        // still force this task through on the machine that skipped it.
        assert_eq!(ctx.state.tasks["a"].version, 1);
        assert_eq!(ctx.state.tasks["a"].last_reason, "already_satisfied");
    }

    #[tokio::test]
    async fn a_dry_run_records_nothing_it_found_already_satisfied() {
        // `--check` reports; it does not change the machine, and state.json is
        // part of the machine. The recording above is the first thing in this
        // engine that writes without applying, so it is the first that could
        // get this wrong — and it did, until the macOS end-to-end suite noticed
        // a dry run had grown a `repo_status` record out of nowhere.
        let (mut ctx, _home) = test_ctx().await;
        ctx.dry_run = true;
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(Fake::new("a", vec![], vec![]))];

        run_all(&tasks, &mut ctx).await.unwrap();
        assert!(ctx.state.tasks.is_empty(), "{:?}", ctx.state.tasks);
    }

    #[tokio::test]
    async fn a_satisfied_machine_is_left_alone() {
        let (mut ctx, _home) = test_ctx().await;
        ctx.state.mark_satisfied("a", 1, "never_run");
        let task = Fake::new("a", vec![], vec![Status::Satisfied]);
        let applies = task.applies.clone();
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(task)];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();
        assert_eq!(outcome.satisfied, vec!["a"]);
        assert_eq!(*applies.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_version_bump_forces_a_rerun_even_when_the_check_passes() {
        let (mut ctx, _home) = test_ctx().await;
        ctx.state.mark_satisfied("a", 1, "never_run");
        let mut task = Fake::new("a", vec![], vec![Status::Satisfied]);
        task.version = 2;
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(task)];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();
        assert_eq!(outcome.applied, vec!["a"]);
        assert_eq!(ctx.state.tasks["a"].version, 2);
        assert_eq!(ctx.state.tasks["a"].last_reason, "version_changed");
    }

    #[tokio::test]
    async fn a_dependency_that_ran_forces_its_dependents_to_rerun() {
        let (mut ctx, _home) = test_ctx().await;
        // Seeded through the real API rather than into `ctx.state` alone: the
        // engine reloads under the lock after every task, so a record that was
        // never on disk is correctly gone by the time `b` is considered.
        ctx.update_state(|state| {
            state.mark_satisfied("a", 1, "never_run");
            state.mark_satisfied("b", 1, "never_run");
        })
        .await
        .unwrap();
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::new(
                "a",
                vec![],
                vec![Status::needs("stale"), Status::Satisfied],
            )),
            Box::new(Fake::new("b", vec!["a"], vec![Status::Satisfied])),
        ];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();
        assert_eq!(outcome.applied, vec!["a", "b"]);
        assert_eq!(ctx.state.tasks["b"].last_reason, "upstream:a");
    }

    #[tokio::test]
    async fn an_apply_that_did_not_take_is_a_hard_error() {
        let (mut ctx, _home) = test_ctx().await;
        // check fails, apply "succeeds", re-check still fails.
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(Fake::new(
            "a",
            vec![],
            vec![Status::needs("missing"), Status::needs("still missing")],
        ))];

        let error = run_all(&tasks, &mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("did not take effect"), "{error}");
        // And nothing was recorded: a lie about the machine is worse than a gap.
        assert!(!ctx.state.tasks.contains_key("a"));
    }

    #[tokio::test]
    async fn a_dry_run_changes_nothing() {
        let (mut ctx, _home) = test_ctx().await;
        ctx.dry_run = true;
        let task = Fake::new("a", vec![], vec![Status::needs("missing")]);
        let applies = task.applies.clone();
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(task)];

        run_all(&tasks, &mut ctx).await.unwrap();
        assert_eq!(*applies.lock().unwrap(), 0);
        assert!(!ctx.state.tasks.contains_key("a"));
    }
}

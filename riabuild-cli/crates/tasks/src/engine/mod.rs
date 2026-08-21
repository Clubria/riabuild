//! The task runner: topological order, then check → apply → re-check.
//!
//! The order is `order`, the check → apply → re-check decision for one task
//! is `status`, and what a run remembers about the tasks it could not finish
//! is `carried`. What is here is the loop that drives the three.

mod carried;
mod order;
mod status;

pub use order::topological_order;
pub use status::status_for;

use carried::Carried;

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use riabuild_ui::Failure;
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct Outcome {
    pub satisfied: Vec<TaskId>,
    pub applied: Vec<TaskId>,
    /// Tasks that could not be finished. The run carried on past each one.
    pub failed: Vec<TaskId>,
    /// Tasks riabuild never attempted, because something they depend on
    /// failed.
    pub skipped: Vec<TaskId>,
}

/// The whole run, and its verdict, kept apart.
///
/// A run that carries on past a failed task has something to say about every
/// task beside it, and `Result` cannot carry both — an `Err` throws the
/// `Outcome` away. That is not a detail of the type: `provision` writes the
/// Claude launchers, prints the "Worth knowing" notes and logs the run *from*
/// the outcome, and with `run_all(…)?` at the top it did none of those on
/// exactly the run that needed them most. The machine a developer walked away
/// from with one failed sign-in was left without the launchers for the eight
/// tasks that worked.
///
/// The verdict is unchanged and still has to be propagated: `provision` returns
/// it after the landing above, so the exit status is what it always was.
pub async fn run_all_with_outcome(tasks: &[Box<dyn Task>], ctx: &mut Ctx) -> (Outcome, Result<()>) {
    let mut outcome = Outcome::default();
    let verdict = run_into(tasks, ctx, &mut outcome).await;
    (outcome, verdict)
}

/// The `?`-shaped spelling, for callers with nothing to do with a partial run.
pub async fn run_all(tasks: &[Box<dyn Task>], ctx: &mut Ctx) -> Result<Outcome> {
    let (outcome, verdict) = run_all_with_outcome(tasks, ctx).await;
    verdict.map(|()| outcome)
}

async fn run_into(tasks: &[Box<dyn Task>], ctx: &mut Ctx, outcome: &mut Outcome) -> Result<()> {
    let order = topological_order(tasks)?;
    let mut applied: HashSet<TaskId> = HashSet::new();
    let mut carried = Carried::default();

    for position in order.into_iter().flatten() {
        let task = tasks[position].as_ref();

        // Before anything is asked of the machine. A `check()` behind a failed
        // prerequisite would report drift its own `apply()` could not repair,
        // and an `apply()` would work against a state nothing established.
        if let Some(blocker) = carried.blocker(task) {
            carried.skipped(task, blocker, ctx, outcome);
            continue;
        }

        let status = match status_for(task, ctx, &applied).await {
            Ok(status) => status,
            // A `check()` that errors is not the same as one that says work is
            // needed — it is a question riabuild could not put to the machine
            // at all — but it is the same *kind* of failure to a run: nothing
            // downstream of it can be trusted, and everything beside it still
            // can.
            Err(error) => {
                carried.failed(task, error, ctx, outcome);
                continue;
            }
        };

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
            // Recorded as applied *for this run's purposes* as well as
            // reported. A real run would apply this task, and applying it is
            // what makes every dependent re-run — so a dry run that left the
            // set empty answered a different question than the one asked: on a
            // machine missing Node it reported the tasks behind `toolchain`
            // satisfied, when a real run would have re-run every one of them.
            applied.insert(task.id());
            outcome.applied.push(task.id());
            continue;
        }

        ctx.ui.working(task.title(), &reason.describe());
        if let Err(error) = task.apply(ctx).await {
            carried.failed(task, error, ctx, outcome);
            continue;
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
                carried.failed(task, error.into(), ctx, outcome);
                continue;
            }
            Err(error) => {
                carried.failed(task, error, ctx, outcome);
                continue;
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
    //
    // Before the failures are reported, and unconditionally: every task that
    // *did* finish has earned its record whatever happened beside it.
    ctx.update_state(|_| {}).await?;

    // The run still fails, and that is the whole of what changed here: it
    // fails at the end, having done everything it could, rather than at the
    // first task that could not be finished. `provision` propagates this, so
    // the exit status is what it always was.
    carried.into_result()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;
    use crate::testing::test_ctx;
    use anyhow::anyhow;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct Fake {
        id: TaskId,
        deps: Vec<TaskId>,
        version: u32,
        /// Statuses returned by successive `check()` calls.
        checks: Mutex<Vec<Status>>,
        applies: Arc<Mutex<u32>>,
        /// What `apply()` fails with, when it is meant to.
        fails: Option<&'static str>,
    }

    impl Fake {
        fn new(id: TaskId, deps: Vec<TaskId>, checks: Vec<Status>) -> Self {
            Self {
                id,
                deps,
                version: 1,
                checks: Mutex::new(checks),
                applies: Arc::new(Mutex::new(0)),
                fails: None,
            }
        }

        /// A task that needs to run and cannot: the browser sign-in nobody
        /// answered, the download that timed out.
        fn failing(id: TaskId, deps: Vec<TaskId>, why: &'static str) -> Self {
            let mut task = Self::new(id, deps, vec![Status::needs("not set up")]);
            task.fails = Some(why);
            task
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
            match self.fails {
                Some(why) => Err(anyhow!("{why}")),
                None => Ok(()),
            }
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
    async fn a_task_that_fails_does_not_take_the_rest_of_the_run_with_it() {
        // What `registry()`'s ordering comment used to be instead of. A
        // developer who walks away from the Claude sign-in leaves that task
        // failing, and everything that does not depend on it — the Codex
        // install, the toolchain, the checkout — has no business waiting behind
        // it, let alone being cancelled by it.
        let (mut ctx, _home) = test_ctx().await;
        let later = Fake::new("b", vec![], vec![Status::needs("nothing here yet")]);
        let applies = later.applies.clone();
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::failing("a", vec![], "nobody answered the browser")),
            Box::new(later),
        ];

        let error = run_all(&tasks, &mut ctx)
            .await
            .expect_err("a run with a failed task still fails");

        assert_eq!(*applies.lock().unwrap(), 1, "the later task never ran");
        assert!(ctx.state.tasks.contains_key("b"), "{:?}", ctx.state.tasks);
        assert!(!ctx.state.tasks.contains_key("a"), "{:?}", ctx.state.tasks);
        // One failure is passed through as it arrived, so the remedy the task
        // wrote is the one the developer reads.
        assert!(format!("{error}").contains("nobody answered"), "{error}");
    }

    #[tokio::test]
    async fn a_task_whose_prerequisite_failed_does_not_run() {
        // The other half, and the reason this is not simply "keep going". A
        // dependent behind a failed task would be working against a machine
        // nothing put in the state it was promised — and `depends_on()` is
        // where that is written down, which is the whole point of moving the
        // question out of `registry()`'s ordering.
        let (mut ctx, _home) = test_ctx().await;
        let dependent = Fake::new("b", vec!["a"], vec![Status::needs("waiting on a")]);
        let transitive = Fake::new("c", vec!["b"], vec![Status::needs("waiting on b")]);
        let (b_applies, c_applies) = (dependent.applies.clone(), transitive.applies.clone());
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::failing("a", vec![], "the download timed out")),
            Box::new(dependent),
            Box::new(transitive),
        ];

        let outcome = run_all(&tasks, &mut ctx).await;

        assert!(outcome.is_err());
        assert_eq!(*b_applies.lock().unwrap(), 0, "b ran behind a failed a");
        assert_eq!(*c_applies.lock().unwrap(), 0, "the skip is not transitive");
        assert!(ctx.state.tasks.is_empty(), "{:?}", ctx.state.tasks);
    }

    #[tokio::test]
    async fn the_run_ends_by_naming_everything_that_failed() {
        // Two failures have no single remedy, so the summary is a list. It has
        // to name each of them: a developer reading only the last thing on
        // their screen would otherwise go and fix one of two problems.
        let (mut ctx, _home) = test_ctx().await;
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::failing("a", vec![], "the download timed out")),
            Box::new(Fake::new("b", vec![], vec![Status::needs("x")])),
            Box::new(Fake::failing("c", vec![], "nobody answered the browser")),
        ];

        let error = run_all(&tasks, &mut ctx).await.unwrap_err();
        let failure = error
            .downcast_ref::<Failure>()
            .unwrap_or_else(|| panic!("not a Failure: {error}"));

        assert!(
            failure.detail.contains("a: the download timed out"),
            "{failure:?}"
        );
        assert!(failure.detail.contains("c: nobody answered"), "{failure:?}");
        assert!(!failure.detail.contains("b:"), "b finished: {failure:?}");
        assert!(ctx.state.tasks.contains_key("b"));
    }

    #[tokio::test]
    async fn the_failures_are_reported_as_they_happen_too() {
        // The returned error is rendered once, at the end, by whoever called
        // the engine. A developer watching a long run needs the task that
        // failed to resolve where it is on the ladder rather than sitting at
        // `◐` for the rest of it.
        let (mut ctx, _home) = test_ctx().await;
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::failing("a", vec![], "the download timed out")),
            Box::new(Fake::new("b", vec!["a"], vec![Status::needs("waiting")])),
        ];

        run_all(&tasks, &mut ctx).await.unwrap_err();

        let warned = ctx.ui.warned().join("\n");
        assert!(warned.contains("could not be set up"), "{warned}");
        assert!(
            warned.contains("not attempted — a did not finish"),
            "{warned}"
        );
    }

    #[tokio::test]
    async fn a_dry_run_reports_what_a_real_run_would_re_run() {
        // `--check` answers "what would a real run do", and a real run applies
        // a task and then re-runs everything downstream of it. Leaving the
        // applied set empty under `--check` answered a different question: on a
        // machine missing Node it reported the tasks behind `toolchain`
        // satisfied, when a real run would have re-run every one of them.
        let (mut ctx, _home) = test_ctx().await;
        ctx.dry_run = true;
        ctx.update_state(|state| {
            state.mark_satisfied("a", 1, "never_run");
            state.mark_satisfied("b", 1, "never_run");
        })
        .await
        .unwrap();
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::new("a", vec![], vec![Status::needs("no node here")])),
            // Satisfied on its own terms, and still going to re-run: its
            // dependency is about to change underneath it.
            Box::new(Fake::new("b", vec!["a"], vec![Status::Satisfied])),
        ];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();

        assert_eq!(outcome.applied, vec!["a", "b"]);
        assert!(outcome.satisfied.is_empty(), "{:?}", outcome.satisfied);
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

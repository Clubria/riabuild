//! The task runner: topological order, then check → apply → re-check.

use super::{Ctx, Reason, Status, Task, TaskId};
use crate::ui::Failure;
use anyhow::{Result, anyhow};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Default)]
pub struct Outcome {
    pub satisfied: Vec<TaskId>,
    pub applied: Vec<TaskId>,
}

/// Orders tasks so every dependency comes before its dependents.
///
/// Returns an error for a cycle or for an edge naming a task that is not
/// registered — both are programming mistakes that must fail loudly in tests
/// rather than quietly reorder someone's setup.
pub fn topological_order(tasks: &[Box<dyn Task>]) -> Result<Vec<usize>> {
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
    let mut order = Vec::with_capacity(tasks.len());

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

        for position in ready {
            remaining.remove(&position);
            done.insert(tasks[position].id());
            order.push(position);
        }
    }

    Ok(order)
}

/// Decides whether a task needs to run, without running anything.
pub fn status_for(task: &dyn Task, ctx: &Ctx, applied: &HashSet<TaskId>) -> Result<Status> {
    let record = ctx.state.tasks.get(task.id());

    let Some(record) = record else {
        return Ok(Status::Needs(Reason::NeverRun));
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

    task.check(ctx)
}

pub fn run_all(tasks: &[Box<dyn Task>], ctx: &mut Ctx) -> Result<Outcome> {
    let order = topological_order(tasks)?;
    let mut applied: HashSet<TaskId> = HashSet::new();
    let mut outcome = Outcome::default();

    for position in order {
        let task = tasks[position].as_ref();
        let status = status_for(task, ctx, &applied)?;

        let reason = match status {
            Status::Satisfied => {
                ctx.ui.satisfied(task.title());
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
        task.apply(ctx)?;

        // The whole point: never record a success we have not verified.
        match task.check(ctx)? {
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
        ctx.state
            .mark_satisfied(task.id(), task.version(), &reason.tag());
        ctx.state.save(ctx.paths.as_ref())?;
        applied.insert(task.id());
        outcome.applied.push(task.id());
    }

    ctx.state.save(ctx.paths.as_ref())?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::registry;
    use crate::testing::test_ctx;
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
        fn check(&self, _ctx: &Ctx) -> Result<Status> {
            let mut checks = self.checks.lock().unwrap();
            if checks.is_empty() {
                return Ok(Status::Satisfied);
            }
            Ok(checks.remove(0))
        }
        fn apply(&self, _ctx: &mut Ctx) -> Result<()> {
            *self.applies.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[test]
    fn the_real_graph_is_acyclic_and_fully_declared() {
        // The test the design asks for: every declared edge names a registered
        // task, and the graph can actually be ordered.
        let tasks = registry();
        let order = topological_order(&tasks).expect("registry must be a DAG");
        assert_eq!(order.len(), tasks.len());

        let mut seen: HashSet<TaskId> = HashSet::new();
        for position in order {
            for dependency in tasks[position].depends_on() {
                assert!(
                    seen.contains(dependency),
                    "{} ran before its dependency {dependency}",
                    tasks[position].id()
                );
            }
            seen.insert(tasks[position].id());
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

    #[test]
    fn a_first_run_applies_everything_and_records_it() {
        let (mut ctx, _home) = test_ctx();
        // Only one status is queued: with no record in state.json the engine
        // reports NeverRun without asking `check()` at all, so the single call
        // that happens is the verifying re-check after `apply()`.
        let tasks: Vec<Box<dyn Task>> =
            vec![Box::new(Fake::new("a", vec![], vec![Status::Satisfied]))];

        let outcome = run_all(&tasks, &mut ctx).unwrap();
        assert_eq!(outcome.applied, vec!["a"]);
        assert_eq!(ctx.state.tasks["a"].version, 1);
        assert_eq!(ctx.state.tasks["a"].last_reason, "never_run");
    }

    #[test]
    fn a_satisfied_machine_is_left_alone() {
        let (mut ctx, _home) = test_ctx();
        ctx.state.mark_satisfied("a", 1, "never_run");
        let task = Fake::new("a", vec![], vec![Status::Satisfied]);
        let applies = task.applies.clone();
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(task)];

        let outcome = run_all(&tasks, &mut ctx).unwrap();
        assert_eq!(outcome.satisfied, vec!["a"]);
        assert_eq!(*applies.lock().unwrap(), 0);
    }

    #[test]
    fn a_version_bump_forces_a_rerun_even_when_the_check_passes() {
        let (mut ctx, _home) = test_ctx();
        ctx.state.mark_satisfied("a", 1, "never_run");
        let mut task = Fake::new("a", vec![], vec![Status::Satisfied]);
        task.version = 2;
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(task)];

        let outcome = run_all(&tasks, &mut ctx).unwrap();
        assert_eq!(outcome.applied, vec!["a"]);
        assert_eq!(ctx.state.tasks["a"].version, 2);
        assert_eq!(ctx.state.tasks["a"].last_reason, "version_changed");
    }

    #[test]
    fn a_dependency_that_ran_forces_its_dependents_to_rerun() {
        let (mut ctx, _home) = test_ctx();
        ctx.state.mark_satisfied("a", 1, "never_run");
        ctx.state.mark_satisfied("b", 1, "never_run");
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::new(
                "a",
                vec![],
                vec![Status::needs("stale"), Status::Satisfied],
            )),
            Box::new(Fake::new("b", vec!["a"], vec![Status::Satisfied])),
        ];

        let outcome = run_all(&tasks, &mut ctx).unwrap();
        assert_eq!(outcome.applied, vec!["a", "b"]);
        assert_eq!(ctx.state.tasks["b"].last_reason, "upstream:a");
    }

    #[test]
    fn an_apply_that_did_not_take_is_a_hard_error() {
        let (mut ctx, _home) = test_ctx();
        // check fails, apply "succeeds", re-check still fails.
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(Fake::new(
            "a",
            vec![],
            vec![Status::needs("missing"), Status::needs("still missing")],
        ))];

        let error = run_all(&tasks, &mut ctx).unwrap_err().to_string();
        assert!(error.contains("did not take effect"), "{error}");
        // And nothing was recorded: a lie about the machine is worse than a gap.
        assert!(!ctx.state.tasks.contains_key("a"));
    }

    #[test]
    fn a_dry_run_changes_nothing() {
        let (mut ctx, _home) = test_ctx();
        ctx.dry_run = true;
        let task = Fake::new("a", vec![], vec![Status::needs("missing")]);
        let applies = task.applies.clone();
        let tasks: Vec<Box<dyn Task>> = vec![Box::new(task)];

        run_all(&tasks, &mut ctx).unwrap();
        assert_eq!(*applies.lock().unwrap(), 0);
        assert!(!ctx.state.tasks.contains_key("a"));
    }
}

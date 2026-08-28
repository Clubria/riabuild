//! Cutting one dependency wave into the steps a developer will watch it take.
//!
//! `topological_order` says which tasks *may* run together. Three things then
//! say which of them actually will, and all three are decided here, before
//! anything runs:
//!
//! - a task that needs the developer runs **alone**, against the run's own
//!   `Ui`, so its device code or its question reaches a terminal nobody else is
//!   drawing on;
//! - a task whose prerequisite failed runs **not at all**, and still has to be
//!   reported where it stands in the list;
//! - two tasks that write the same thing run **one after the other**, because
//!   `depends_on()` declares ordering and `writes()` is where exclusion is
//!   declared instead.
//!
//! Everything left over runs at the same time. The cut never reorders the wave:
//! every step holds a run of positions that were already adjacent, so the
//! output stays in declaration order whatever the concurrency does underneath.

use crate::{Task, TaskId};
use std::collections::HashSet;

/// One step of a wave, in the order the developer will see it happen.
pub(super) enum Step {
    /// One task, against the run's own `Ctx`: it is about to talk to the
    /// developer, or it is not going to run at all.
    Alone(usize),
    /// Tasks with nothing in common, run at the same time and reported in this
    /// order however they finish.
    Together(Vec<usize>),
}

/// Splits `wave` into steps.
///
/// `alone` holds the positions that must not be forked — the interactive tasks
/// and the ones behind a failed prerequisite. `jobs` caps how many tasks a
/// single `Together` may hold; `1` reproduces the sequential engine exactly,
/// which is what `--jobs 1` is for.
///
/// The cap is a cap on a *group*, not a pool: a group runs to completion before
/// the next one starts, so one slow task in a group of four delays the fifth.
/// That is the honest shape of a bound that costs no scheduler, and at riabuild's
/// six-task maximum the difference is not measurable.
pub(super) fn steps(
    tasks: &[Box<dyn Task>],
    wave: &[usize],
    alone: &HashSet<usize>,
    jobs: usize,
) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut group: Vec<usize> = Vec::new();

    for &position in wave {
        if alone.contains(&position) {
            if !group.is_empty() {
                steps.push(Step::Together(std::mem::take(&mut group)));
            }
            steps.push(Step::Alone(position));
            continue;
        }

        let contended = group
            .iter()
            .any(|&other| shares_a_resource(tasks[other].as_ref(), tasks[position].as_ref()));
        if !group.is_empty() && (contended || group.len() >= jobs.max(1)) {
            steps.push(Step::Together(std::mem::take(&mut group)));
        }
        group.push(position);
    }

    if !group.is_empty() {
        steps.push(Step::Together(group));
    }
    steps
}

/// Whether these two tasks both write something one of them names.
///
/// Quadratic over a group that never exceeds a handful, and over resource lists
/// with one entry in them. A `HashSet` here would cost more than it saved and
/// would hide what this is: two short lists, compared.
fn shares_a_resource(one: &dyn Task, other: &dyn Task) -> bool {
    one.writes()
        .iter()
        .any(|resource| other.writes().contains(resource))
}

/// The tasks of `wave` that must not be forked, by position.
///
/// Blockers are settled before the wave starts and cannot change inside it: a
/// dependency is always in a strictly earlier wave, so nothing running now can
/// fail one.
pub(super) fn run_alone(
    tasks: &[Box<dyn Task>],
    wave: &[usize],
    blocked: impl Fn(&dyn Task) -> Option<TaskId>,
) -> HashSet<usize> {
    wave.iter()
        .copied()
        .filter(|&position| {
            let task = tasks[position].as_ref();
            task.interactive() || blocked(task).is_some()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Ctx, Resource, Status};
    use anyhow::Result;
    use async_trait::async_trait;

    struct Fake {
        id: TaskId,
        writes: Vec<Resource>,
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
            1
        }
        fn depends_on(&self) -> &[TaskId] {
            &[]
        }
        fn writes(&self) -> &[Resource] {
            &self.writes
        }
        async fn check(&self, _ctx: &Ctx) -> Result<Status> {
            Ok(Status::Satisfied)
        }
        async fn apply(&self, _ctx: &mut Ctx) -> Result<()> {
            Ok(())
        }
    }

    fn graph(rows: &[(TaskId, &[Resource])]) -> Vec<Box<dyn Task>> {
        rows.iter()
            .map(|(id, writes)| {
                Box::new(Fake {
                    id,
                    writes: writes.to_vec(),
                }) as Box<dyn Task>
            })
            .collect()
    }

    /// Renders a cut as `"a b | c"` — groups separated by `|`, a task alone in
    /// its own group. Comparing strings rather than a nested structure so a
    /// failure prints the shape rather than a wall of enum.
    fn shape(steps: &[Step], tasks: &[Box<dyn Task>]) -> String {
        steps
            .iter()
            .map(|step| match step {
                Step::Alone(position) => tasks[*position].id().to_string(),
                Step::Together(group) => group
                    .iter()
                    .map(|&position| tasks[position].id())
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn tasks_with_nothing_in_common_land_in_one_group() {
        let tasks = graph(&[("a", &[]), ("b", &[]), ("c", &[])]);
        let steps = steps(&tasks, &[0, 1, 2], &HashSet::new(), usize::MAX);
        assert_eq!(shape(&steps, &tasks), "a b c");
    }

    /// The wave-5 shape: everything writes one file, so nothing overlaps.
    #[test]
    fn tasks_that_write_the_same_thing_are_cut_apart() {
        let tasks = graph(&[
            ("trust", &["claude_config"]),
            ("onboarding", &["claude_config"]),
            ("plugins", &["claude_config"]),
        ]);
        let steps = steps(&tasks, &[0, 1, 2], &HashSet::new(), usize::MAX);
        assert_eq!(shape(&steps, &tasks), "trust | onboarding | plugins");
    }

    /// Disjoint resources are not a conflict. Only a name in common is.
    #[test]
    fn different_resources_still_run_together() {
        let tasks = graph(&[("a", &["one"]), ("b", &["two"]), ("c", &["one"])]);
        let steps = steps(&tasks, &[0, 1, 2], &HashSet::new(), usize::MAX);
        assert_eq!(shape(&steps, &tasks), "a b | c");
    }

    /// An interactive task splits its wave where it stands, and the tasks
    /// either side of it still group among themselves. The wave-2 shape.
    #[test]
    fn a_task_that_runs_alone_splits_the_wave_where_it_stands() {
        let tasks = graph(&[("a", &[]), ("asks", &[]), ("b", &[]), ("c", &[])]);
        let alone = HashSet::from([1]);
        let steps = steps(&tasks, &[0, 1, 2, 3], &alone, usize::MAX);
        assert_eq!(shape(&steps, &tasks), "a | asks | b c");
    }

    /// The wave-1 shape: two tasks that need the developer, then the downloads.
    #[test]
    fn the_first_wave_of_the_real_registry_keeps_its_downloads_together() {
        let registry = crate::registry();
        let waves = super::super::topological_order(&registry).expect("a DAG");
        let first = &waves[0];
        let alone = run_alone(&registry, first, |_| None);
        let steps = steps(&registry, first, &alone, usize::MAX);

        assert_eq!(
            shape(&steps, &registry),
            "login | github_cli | infisical_cli ngrok grok_cli claude_statusline",
            "the two sign-ins run alone; the tool downloads run at once"
        );
    }

    #[test]
    fn jobs_of_one_puts_every_task_in_its_own_group() {
        let tasks = graph(&[("a", &[]), ("b", &[]), ("c", &[])]);
        let steps = steps(&tasks, &[0, 1, 2], &HashSet::new(), 1);
        assert_eq!(shape(&steps, &tasks), "a | b | c");
    }

    #[test]
    fn a_cap_bounds_a_group_without_reordering_it() {
        let tasks = graph(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[]), ("e", &[])]);
        let steps = steps(&tasks, &[0, 1, 2, 3, 4], &HashSet::new(), 2);
        assert_eq!(shape(&steps, &tasks), "a b | c d | e");
    }

    /// `--jobs 0` is refused at the command line, so this is only about the
    /// engine not dividing a wave into empty groups for ever if one arrives.
    #[test]
    fn a_cap_of_zero_is_treated_as_one_rather_than_as_none() {
        let tasks = graph(&[("a", &[]), ("b", &[])]);
        let steps = steps(&tasks, &[0, 1], &HashSet::new(), 0);
        assert_eq!(shape(&steps, &tasks), "a | b");
    }

    #[test]
    fn an_empty_wave_produces_no_steps() {
        let tasks = graph(&[]);
        assert!(steps(&tasks, &[], &HashSet::new(), usize::MAX).is_empty());
    }
}

//! Dependency waves: the order the engine runs tasks in.

use crate::{Task, TaskId};
use anyhow::{Result, anyhow};
use std::collections::{BTreeSet, HashMap, HashSet};

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

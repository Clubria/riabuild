//! The task runner: dependency waves, then check → apply → re-check.
//!
//! The order is `order`, the cut of one wave into the steps a developer watches
//! it take is `wave`, one task's turn is `attempt`, the concurrency underneath
//! a step is `concurrent`, and what a run remembers about the tasks it could
//! not finish is `carried`. What is here is the loop that drives the five.
//!
//! ## Why a wave runs at once
//!
//! `topological_order` has always returned *waves* — sets of tasks whose
//! dependencies are all satisfied by earlier waves — and this loop used to
//! flatten them away and run one task at a time. A cold run therefore spent
//! most of its wall clock waiting on downloads it could have been doing at the
//! same time: `gh`, `infisical`, `ngrok` and Grok Build have no edges between
//! them and no edges into them.
//!
//! Two things had to be true before a wave could run concurrently, and neither
//! is a property of the graph:
//!
//! - **A terminal has one cursor.** `Ui::working` leaves a status line on the
//!   row for `Ui::applied` to cover with `\r`, so a second task printing
//!   between them covers its own line and the first never resolves. So every
//!   task in a concurrent group prints into a `Ui::buffered`, and the group is
//!   replayed in declaration order once it finishes. What a developer reads is
//!   what the sequential engine produced — which is the reason a concurrent
//!   step reports in the order it was *given* rather than the order it
//!   finished.
//! - **`depends_on()` declares ordering, not exclusion.** Sequential execution
//!   gave every task the machine to itself, so two tasks writing one file with
//!   no edge between them was invisible and free. `Task::writes` is where that
//!   is declared now, and `wave::steps` is what keeps two tasks naming the same
//!   resource out of one group.
//!
//! A task that needs the developer — a device code, a browser, a question —
//! declares `Task::interactive()` and is run alone against the run's own `Ui`,
//! in its declared position. A prompt recorded instead of printed is a prompt
//! nobody can answer.

mod attempt;
mod carried;
mod concurrent;
mod order;
mod status;
mod wave;

pub use order::topological_order;
pub use status::status_for;

use attempt::Attempt;
use carried::Carried;
use wave::Step;

use super::{Ctx, Task, TaskId};
use anyhow::Result;
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

/// How much of a wave riabuild will do at once.
///
/// Its own type rather than a field on `Ctx` because it is the engine's
/// business and no task's: `Ctx` is "everything a task is allowed to touch",
/// and a task that branched on how many of its siblings were running would be
/// a task with an undeclared dependency on one of them.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The most tasks riabuild runs at the same time. `1` is the sequential
    /// engine, exactly.
    pub jobs: usize,
}

impl Default for Limits {
    /// Everything a wave allows. The graph is the bound that matters — the
    /// widest wave riabuild has is six — and a smaller number here would be a
    /// second, invisible one.
    fn default() -> Self {
        Self { jobs: usize::MAX }
    }
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
pub async fn run_all_with_outcome(
    tasks: &[Box<dyn Task>],
    ctx: &mut Ctx,
    limits: Limits,
) -> (Outcome, Result<()>) {
    let mut outcome = Outcome::default();
    let verdict = run_into(tasks, ctx, &mut outcome, limits).await;
    (outcome, verdict)
}

/// The `?`-shaped spelling, for callers with nothing to do with a partial run.
pub async fn run_all(tasks: &[Box<dyn Task>], ctx: &mut Ctx) -> Result<Outcome> {
    let (outcome, verdict) = run_all_with_outcome(tasks, ctx, Limits::default()).await;
    verdict.map(|()| outcome)
}

async fn run_into(
    tasks: &[Box<dyn Task>],
    ctx: &mut Ctx,
    outcome: &mut Outcome,
    limits: Limits,
) -> Result<()> {
    let waves = topological_order(tasks)?;
    let mut applied: HashSet<TaskId> = HashSet::new();
    let mut carried = Carried::default();

    for positions in waves {
        // Settled before anything in the wave runs, and safe to settle there:
        // a dependency is always in a strictly earlier wave, so nothing about
        // to run can block anything else about to run.
        let alone = wave::run_alone(tasks, &positions, |task| carried.blocker(task));

        // Held back until the wave is over rather than inserted as each task
        // finishes. Nothing in a wave depends on anything else in it, so no
        // `status_for` in this wave can want these — and a set that grew
        // mid-wave would make a concurrent run's answers depend on which
        // download finished first.
        let mut ran: Vec<TaskId> = Vec::new();

        for step in wave::steps(tasks, &positions, &alone, limits.jobs) {
            match step {
                Step::Alone(position) => {
                    let task = tasks[position].as_ref();
                    // Before anything is asked of the machine. A `check()`
                    // behind a failed prerequisite would report drift its own
                    // `apply()` could not repair, and an `apply()` would work
                    // against a state nothing established.
                    if let Some(blocker) = carried.blocker(task) {
                        carried.skipped(task, blocker, ctx, outcome);
                        continue;
                    }
                    let done = attempt::run(task, ctx, &applied).await;
                    settle(task, done, ctx, &mut carried, outcome, &mut ran)?;
                }
                Step::Together(group) => {
                    // One `Ctx` each, because `apply()` takes `&mut Ctx` and
                    // two of those cannot exist at once. See `Ctx::fork`.
                    let mut forks: Vec<Ctx> = group.iter().map(|_| ctx.fork()).collect();
                    let running: Vec<_> = group
                        .iter()
                        .zip(forks.iter_mut())
                        .map(|(&position, fork)| {
                            attempt::run(tasks[position].as_ref(), fork, &applied)
                        })
                        .collect();
                    let done = concurrent::join_in_order(running).await;

                    // In declaration order, on the run's own `Ctx`: absorbing a
                    // fork is what puts its output on the screen, so this loop
                    // is the whole of what makes a concurrent wave read like a
                    // sequential one.
                    //
                    // A fatal error is kept rather than returned from inside
                    // here. Every task in this group has already *run* — the
                    // work is done and on the machine — so leaving on the first
                    // one would throw away the report of tasks that finished,
                    // which is the thing `run_all_with_outcome` exists to
                    // stop happening one level up.
                    let mut fatal: Option<anyhow::Error> = None;
                    for ((position, fork), done) in group.into_iter().zip(forks).zip(done) {
                        ctx.absorb(fork);
                        if let Err(error) = settle(
                            tasks[position].as_ref(),
                            done,
                            ctx,
                            &mut carried,
                            outcome,
                            &mut ran,
                        ) {
                            fatal.get_or_insert(error);
                        }
                    }

                    // The forks wrote `config.json` and `state.json` under the
                    // file lock, each re-reading inside it, so the disk holds
                    // every edit and this run's snapshots hold only some of
                    // them. The file is the authority; take it back — and take
                    // it back *per step*, so the step after this one sees what
                    // this one wrote exactly as it did when the engine ran one
                    // task at a time. Within a group is the one place a task
                    // cannot see a sibling's write, and that is sound for the
                    // reason the group exists: nothing in it depends on
                    // anything else in it.
                    ctx.reload().await;

                    if let Some(error) = fatal {
                        return Err(error);
                    }
                }
            }
        }

        applied.extend(ran);
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

/// What one finished attempt means to the run.
///
/// Every line of it touches state shared by the whole wave — `Carried`, the
/// `Outcome`, the applied set, and the run's own `Ui` — which is exactly why it
/// is here and not in `attempt`: this runs one task at a time, in declaration
/// order, however many of them ran at once.
fn settle(
    task: &dyn Task,
    done: Attempt,
    ctx: &Ctx,
    carried: &mut Carried,
    outcome: &mut Outcome,
    ran: &mut Vec<TaskId>,
) -> Result<()> {
    match done {
        Attempt::Satisfied => outcome.satisfied.push(task.id()),
        Attempt::Ran => {
            ran.push(task.id());
            outcome.applied.push(task.id());
        }
        Attempt::Failed(error) => carried.failed(task, error, ctx, outcome),
        // riabuild can no longer remember what it has done, so there is nothing
        // honest left to do with the rest of the run.
        Attempt::Fatal(error) => return Err(error),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // No longer reachable through `super::*`: the run loop hands `Status`
    // straight to `attempt`, and only the fakes below still name it.
    use crate::registry;
    use crate::testing::test_ctx;
    use crate::{Resource, Status};
    use anyhow::anyhow;
    use async_trait::async_trait;
    use riabuild_ui::Failure;
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
        interactive: bool,
        writes: Vec<Resource>,
        /// A rendezvous `apply()` waits at. Every task sharing one has to be
        /// inside `apply()` at the same time for any of them to leave, so a
        /// test that finishes proves they overlapped and one that hangs proves
        /// they did not.
        gate: Option<Arc<Gate>>,
        /// Where `apply()` writes that it started and that it finished, for the
        /// tests that care whether two tasks were inside at once.
        journal: Option<Arc<Mutex<Vec<String>>>>,
        /// How long `apply()` takes, so a test can make the task declared first
        /// the one that finishes last.
        delay_ms: u64,
        /// Whether `apply()` says something the developer would read. `warn`
        /// rather than `note` because `Ui::note` is silent under `--quiet` and
        /// `test_ctx` is quiet, and what these tests are about is the order
        /// lines reach the terminal.
        announces: bool,
    }

    /// A meeting point for the tasks of one wave.
    ///
    /// `yield_now` rather than a channel or a `tokio::sync::Barrier`: this has
    /// to work on a current-thread runtime with no `sync` feature turned on,
    /// which is the runtime riabuild actually has.
    struct Gate {
        arrived: Mutex<u32>,
        needed: u32,
    }

    impl Gate {
        fn holding(needed: u32) -> Arc<Self> {
            Arc::new(Self {
                arrived: Mutex::new(0),
                needed,
            })
        }

        async fn meet(&self) {
            *self.arrived.lock().unwrap() += 1;
            while *self.arrived.lock().unwrap() < self.needed {
                tokio::task::yield_now().await;
            }
        }
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
                interactive: false,
                writes: Vec::new(),
                gate: None,
                journal: None,
                delay_ms: 0,
                announces: false,
            }
        }

        /// A task that needs to run, and that says so on the way through.
        fn runs(id: TaskId) -> Self {
            let mut task = Self::new(id, vec![], vec![Status::needs("not set up")]);
            task.announces = true;
            task
        }

        fn at(mut self, gate: &Arc<Gate>) -> Self {
            self.gate = Some(Arc::clone(gate));
            self
        }

        fn logging(mut self, journal: &Arc<Mutex<Vec<String>>>) -> Self {
            self.journal = Some(Arc::clone(journal));
            self
        }

        fn writing(mut self, resource: Resource) -> Self {
            self.writes = vec![resource];
            self
        }

        fn taking(mut self, delay_ms: u64) -> Self {
            self.delay_ms = delay_ms;
            self
        }

        fn needing_the_developer(mut self) -> Self {
            self.interactive = true;
            self
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
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn writes(&self) -> &[Resource] {
            &self.writes
        }
        async fn check(&self, _ctx: &Ctx) -> Result<Status> {
            let mut checks = self.checks.lock().unwrap();
            if checks.is_empty() {
                return Ok(Status::Satisfied);
            }
            Ok(checks.remove(0))
        }
        async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
            *self.applies.lock().unwrap() += 1;
            if let Some(journal) = &self.journal {
                journal.lock().unwrap().push(format!("enter {}", self.id));
            }
            // Said from inside `apply()`, which is where a real task prints. On
            // a forked `Ctx` this reaches a buffer, and the order it comes back
            // out in is what these tests are about.
            if self.announces {
                ctx.ui.warn(&format!("{} ran", self.id));
            }
            if let Some(gate) = &self.gate {
                gate.meet().await;
            }
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            if let Some(journal) = &self.journal {
                journal.lock().unwrap().push(format!("leave {}", self.id));
            }
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

    /// How long a test waits before calling a rendezvous a deadlock.
    ///
    /// Generous on purpose: these tests prove *whether* two tasks overlapped,
    /// never how quickly, so the only thing a tight bound could add is a
    /// failure on a loaded CI runner.
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

    /// The whole point of the change. Three independent tasks that each wait
    /// for the other two to have started: sequentially the first one blocks for
    /// ever, concurrently all three go through.
    #[tokio::test]
    async fn the_independent_tasks_of_one_wave_run_at_the_same_time() {
        let (mut ctx, _home) = test_ctx().await;
        let gate = Gate::holding(3);
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::runs("a").at(&gate)),
            Box::new(Fake::runs("b").at(&gate)),
            Box::new(Fake::runs("c").at(&gate)),
        ];

        let outcome = tokio::time::timeout(PATIENCE, run_all(&tasks, &mut ctx))
            .await
            .expect("a wave of independent tasks must not run one at a time")
            .unwrap();

        assert_eq!(outcome.applied, vec!["a", "b", "c"]);
    }

    /// And what the developer reads is still one task at a time, in the order
    /// the registry declares them — not the order the downloads happened to
    /// finish in.
    ///
    /// `a` is the slowest and is declared first, so a run that reported as it
    /// finished would say `c`, `b`, `a`.
    #[tokio::test]
    async fn a_wave_reports_in_declaration_order_however_it_finishes() {
        let (mut ctx, _home) = test_ctx().await;
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::runs("a").taking(60)),
            Box::new(Fake::runs("b").taking(30)),
            Box::new(Fake::runs("c").taking(1)),
        ];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();

        assert_eq!(outcome.applied, vec!["a", "b", "c"]);
        // The lines each task printed from inside `apply()`, which took a
        // buffered `Ui` and came back out through `Ctx::absorb`.
        assert_eq!(ctx.ui.warned(), vec!["a ran", "b ran", "c ran"]);
    }

    /// `Task::writes` is what keeps two tasks out of one file, and this is the
    /// shape of the bug it exists for: `claude_trust`, `claude_onboarding` and
    /// `claude_agents_view` are independent, land in one wave, and each
    /// read-modify-writes the same `.claude.json`.
    ///
    /// The journal is checked for *nesting* rather than for a fixed sequence: a
    /// second task entering before the first has left is the lost update,
    /// whichever order they went in.
    #[tokio::test]
    async fn two_tasks_that_write_the_same_thing_never_overlap() {
        let (mut ctx, _home) = test_ctx().await;
        let journal = Arc::new(Mutex::new(Vec::new()));
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(
                Fake::runs("trust")
                    .writing("claude_config")
                    .logging(&journal)
                    .taking(20),
            ),
            Box::new(
                Fake::runs("onboarding")
                    .writing("claude_config")
                    .logging(&journal)
                    .taking(1),
            ),
            // Declares nothing, so it is free to run beside them.
            Box::new(Fake::runs("elsewhere").logging(&journal)),
        ];

        run_all(&tasks, &mut ctx).await.unwrap();

        let journal = journal.lock().unwrap().clone();
        assert!(
            !overlaps(&journal, "trust", "onboarding"),
            "two writers of one file were inside at once: {journal:?}"
        );
    }

    /// Whether `one` and `other` were ever inside `apply()` at the same time.
    fn overlaps(journal: &[String], one: &str, other: &str) -> bool {
        let mut inside: Vec<&str> = Vec::new();
        for line in journal {
            let Some((event, who)) = line.split_once(' ') else {
                continue;
            };
            if who != one && who != other {
                continue;
            }
            match event {
                "enter" => {
                    if !inside.is_empty() {
                        return true;
                    }
                    inside.push(who);
                }
                _ => inside.retain(|held| *held != who),
            }
        }
        false
    }

    /// `--jobs 1` is the escape hatch, and it has to be the engine riabuild had
    /// before this: one task at a time, whatever the graph allows.
    #[tokio::test]
    async fn jobs_of_one_runs_one_task_at_a_time() {
        let (mut ctx, _home) = test_ctx().await;
        let journal = Arc::new(Mutex::new(Vec::new()));
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::runs("a").logging(&journal).taking(20)),
            Box::new(Fake::runs("b").logging(&journal).taking(1)),
        ];

        let (outcome, verdict) = run_all_with_outcome(&tasks, &mut ctx, Limits { jobs: 1 }).await;
        verdict.unwrap();

        assert_eq!(outcome.applied, vec!["a", "b"]);
        assert_eq!(
            journal.lock().unwrap().clone(),
            vec!["enter a", "leave a", "enter b", "leave b"]
        );
    }

    /// A task that needs the developer gets the run's own `Ui`, which is the
    /// only one that can put a question to anybody. Everything beside it gets a
    /// fork, which reports `interactive() == false` because a prompt it
    /// recorded would never be seen.
    ///
    /// This is the invariant behind `Task::interactive`, and the reason a task
    /// that reaches for `ui.ask()` has to declare it.
    #[tokio::test]
    async fn only_an_interactive_task_is_handed_a_ui_that_can_ask() {
        struct Asks {
            id: TaskId,
            interactive: bool,
            saw: Arc<Mutex<Option<bool>>>,
        }

        #[async_trait]
        impl Task for Asks {
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
            fn interactive(&self) -> bool {
                self.interactive
            }
            async fn check(&self, _ctx: &Ctx) -> Result<Status> {
                Ok(match *self.saw.lock().unwrap() {
                    Some(_) => Status::Satisfied,
                    None => Status::needs("not asked yet"),
                })
            }
            async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
                *self.saw.lock().unwrap() = Some(ctx.ui.interactive());
                Ok(())
            }
        }

        let (mut ctx, _home) = test_ctx().await;
        ctx.ui = ctx.ui.assume_prompts_work(true);
        let (needs_it, does_not) = (Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None)));
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Asks {
                id: "asks",
                interactive: true,
                saw: Arc::clone(&needs_it),
            }),
            Box::new(Asks {
                id: "quiet",
                interactive: false,
                saw: Arc::clone(&does_not),
            }),
        ];

        run_all(&tasks, &mut ctx).await.unwrap();

        assert_eq!(
            *needs_it.lock().unwrap(),
            Some(true),
            "a prompt must reach the terminal"
        );
        assert_eq!(
            *does_not.lock().unwrap(),
            Some(false),
            "a buffered Ui must not claim it can ask"
        );
    }

    /// An interactive task is run where it was declared, not hoisted to the
    /// front of its wave — so the ladder reads the same as it always did.
    #[tokio::test]
    async fn an_interactive_task_keeps_its_place_in_the_wave() {
        let (mut ctx, _home) = test_ctx().await;
        let journal = Arc::new(Mutex::new(Vec::new()));
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::runs("first").logging(&journal)),
            Box::new(Fake::runs("asks").needing_the_developer().logging(&journal)),
            Box::new(Fake::runs("last").logging(&journal)),
        ];

        let outcome = run_all(&tasks, &mut ctx).await.unwrap();

        assert_eq!(outcome.applied, vec!["first", "asks", "last"]);
        assert_eq!(ctx.ui.warned(), vec!["first ran", "asks ran", "last ran"]);
        // And it really did run alone: nothing else was inside while it was.
        let journal = journal.lock().unwrap().clone();
        assert!(!overlaps(&journal, "first", "asks"), "{journal:?}");
        assert!(!overlaps(&journal, "asks", "last"), "{journal:?}");
    }

    /// A task that fails in a concurrent group still stops only its own
    /// dependents, and the tasks beside it still finish — the property the
    /// sequential engine had, now that "beside it" means "at the same time as
    /// it".
    #[tokio::test]
    async fn a_failure_in_a_concurrent_group_does_not_take_its_neighbours_down() {
        let (mut ctx, _home) = test_ctx().await;
        let tasks: Vec<Box<dyn Task>> = vec![
            Box::new(Fake::failing("a", vec![], "the download timed out")),
            Box::new(Fake::runs("b")),
            Box::new(Fake::new(
                "c",
                vec!["a"],
                vec![Status::needs("waiting on a")],
            )),
        ];

        let error = run_all(&tasks, &mut ctx).await.unwrap_err();

        assert!(
            format!("{error}").contains("the download timed out"),
            "{error}"
        );
        assert!(ctx.state.tasks.contains_key("b"), "{:?}", ctx.state.tasks);
        assert!(!ctx.state.tasks.contains_key("c"), "{:?}", ctx.state.tasks);
    }

    /// The checklist the registry has to keep, in the one place a reader of
    /// `Task::interactive` will look for it.
    ///
    /// A pin rather than a proof — nothing can derive "this task will open a
    /// browser" from the code — so it is written as the four tasks that do,
    /// named. A fifth arriving without a line here is the failure mode, and the
    /// skill file says so where a task is written.
    #[test]
    fn the_tasks_that_need_the_developer_say_so() {
        let registry = registry();
        let asking: Vec<TaskId> = registry
            .iter()
            .filter(|task| task.interactive())
            .map(|task| task.id())
            .collect();

        assert_eq!(
            asking,
            vec!["login", "github_cli", "project", "claude_accounts"],
            "every task that prints a device code, hands over a pty or asks a \
             question belongs here — see Task::interactive"
        );
    }

    /// And the other half: everything that writes a per-account `.claude.json`
    /// names it, so no two of them are ever in one group.
    #[test]
    fn every_writer_of_the_claude_config_declares_it() {
        let registry = registry();
        let writers: Vec<TaskId> = registry
            .iter()
            .filter(|task| task.writes().contains(&"claude_config"))
            .map(|task| task.id())
            .collect();

        assert_eq!(
            writers,
            vec![
                "claude_trust",
                "claude_onboarding",
                "claude_agents_view",
                "claude_plugins"
            ],
        );
    }

    /// The four of them are independent, so they land in one wave — which is
    /// what makes the declaration above load-bearing rather than decorative.
    #[test]
    fn those_writers_really_do_share_a_wave() {
        let registry = registry();
        let waves = topological_order(&registry).expect("registry must be a DAG");
        let holding = waves
            .iter()
            .find(|wave| {
                wave.iter()
                    .any(|&position| registry[position].id() == "claude_trust")
            })
            .expect("claude_trust is in some wave");

        for id in ["claude_onboarding", "claude_agents_view", "claude_plugins"] {
            assert!(
                holding
                    .iter()
                    .any(|&position| registry[position].id() == id),
                "{id} shares claude_trust's wave"
            );
        }
    }
}

//! The `Task` trait, and the vocabulary a run reports one with.
//!
//! `Reason` is why a task is about to run, `Status` is what `check()`
//! answered, and `Task` is the trait every file under this crate
//! implements exactly once.

use crate::Ctx;
use anyhow::Result;
use async_trait::async_trait;

pub type TaskId = &'static str;

/// Something on the machine that only one task may be inside at a time.
///
/// A name, agreed between the tasks that share it and meaningful to nobody
/// else — `claude_config` is the whole of it today. See [`Task::writes`].
pub type Resource = &'static str;

/// Why a task is about to run. Surfaced to the developer, so it reads as an
/// explanation rather than a status code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    NeverRun,
    VersionChanged { from: u32, to: u32 },
    UpstreamChanged(TaskId),
    CheckFailed(String),
}

impl Reason {
    pub fn describe(&self) -> String {
        match self {
            Reason::NeverRun => "first run".to_string(),
            Reason::VersionChanged { from, to } => {
                format!("riabuild changed what this should look like (v{from} → v{to})")
            }
            Reason::UpstreamChanged(id) => format!("{id} changed"),
            Reason::CheckFailed(detail) => detail.clone(),
        }
    }

    /// Stored in `state.json` for the next run to report against.
    pub fn tag(&self) -> String {
        match self {
            Reason::NeverRun => "never_run".into(),
            Reason::VersionChanged { .. } => "version_changed".into(),
            Reason::UpstreamChanged(id) => format!("upstream:{id}"),
            Reason::CheckFailed(_) => "check_failed".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Satisfied,
    Needs(Reason),
}

impl Status {
    /// Convenience for the common `check()` shape.
    pub fn needs(detail: impl Into<String>) -> Status {
        Status::Needs(Reason::CheckFailed(detail.into()))
    }
}

#[async_trait]
pub trait Task: Send + Sync {
    fn id(&self) -> TaskId;
    fn title(&self) -> &str;
    /// Forced-rerun escape hatch for drift `check()` genuinely cannot observe.
    /// `check()` is authoritative; bumping this to paper over a weak check is a
    /// bug in the check.
    fn version(&self) -> u32;
    fn depends_on(&self) -> &[TaskId];

    /// Whether this task needs the developer and the terminal to itself.
    ///
    /// True for anything that prints a device code, hands over a pty, or asks a
    /// question: the browser sign-ins and the one prompt for where a checkout
    /// should live. The engine runs these alone, against the run's own `Ui`, in
    /// their declared position — everything else in the wave runs concurrently
    /// around them with its output buffered, and a prompt buffered is a prompt
    /// nobody can see.
    ///
    /// Declared statically and therefore *conservatively*: `github_cli` and
    /// `claude_accounts` only reach a browser on a machine that is not signed
    /// in yet, and both say `true` on every run regardless. The cost is a
    /// little concurrency on the runs that would not have needed the terminal;
    /// the alternative is deciding it from inside `apply()`, by which time the
    /// task is already holding a `Ui` that cannot ask.
    ///
    /// A task that reads `ctx.ui.interactive()` or calls `ctx.ui.ask()`
    /// anywhere under `apply()` belongs here. `an_interactive_task_is_the_only_
    /// kind_that_may_ask` in `engine` is the test that says so.
    fn interactive(&self) -> bool {
        false
    }

    /// What this task writes that another task might also write.
    ///
    /// `depends_on()` declares *ordering*, and until the engine ran a wave
    /// concurrently that was the same thing as exclusion — the sequential loop
    /// gave every task the machine to itself, so two tasks touching one file
    /// with no edge between them was invisible and free. It is neither now.
    ///
    /// The case this exists for: `claude_trust`, `claude_onboarding` and
    /// `claude_agents_view` each read-modify-write the same per-account
    /// `.claude.json`, and `claude_plugins` runs a `claude` that writes it too.
    /// All four are independent, so all four land in one wave; run at the same
    /// time they would interleave three read-modify-writes and lose two of
    /// them, in the file Claude Code refuses to start against if it is wrong.
    /// Adding edges between them would have fixed it by writing an ordering
    /// nobody means into the graph, and would have said nothing about the next
    /// pair.
    ///
    /// Two tasks naming a resource in common never run at the same time. Tasks
    /// naming none, or disjoint sets, do.
    fn writes(&self) -> &[Resource] {
        &[]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status>;
    async fn apply(&self, ctx: &mut Ctx) -> Result<()>;
}

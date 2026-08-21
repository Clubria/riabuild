//! The `Task` trait, and the vocabulary a run reports one with.
//!
//! `Reason` is why a task is about to run, `Status` is what `check()`
//! answered, and `Task` is the trait every file under this crate
//! implements exactly once.

use crate::Ctx;
use anyhow::Result;
use async_trait::async_trait;

pub type TaskId = &'static str;

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
    async fn check(&self, ctx: &Ctx) -> Result<Status>;
    async fn apply(&self, ctx: &mut Ctx) -> Result<()>;
}

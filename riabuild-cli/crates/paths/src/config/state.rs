//! `~/.riabuild/state.json` — what each task last achieved.
//!
//! A cache of decisions, never a source of truth about the machine: `check()`
//! is that. So a corrupt or missing file degrades to "run everything" rather
//! than to an error, and unlike [`UserConfig`](crate::config::UserConfig)
//! nothing here is set aside for the developer to recover — losing it costs one
//! redundant `check()`.

use super::{now_secs, write_json};
use crate::Paths;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRecord {
    pub version: u32,
    pub last_ok_at: u64,
    pub last_reason: String,
}

/// Task ids riabuild used to record and no longer has.
///
/// A rename leaves the old record orphaned under the old key. Nothing reads it,
/// but `state.json` is where a developer looks to find out what riabuild thinks
/// it has done, and a key naming a task that no longer exists answers that
/// question wrongly. Dropping it on load means the next save omits it.
const RETIRED_TASKS: &[&str] = &["claude_profiles"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskRecord>,
}

impl State {
    pub async fn load(paths: &dyn Paths) -> Self {
        // Deliberately infallible: a state file we cannot read means we do not
        // know what has been done, and the correct response to that is to check
        // everything again.
        let mut state: State = tokio::fs::read_to_string(paths.state_file())
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        state.forget_retired();
        state
    }

    /// Acquires the lock, reads what is on disk *now*, applies `mutate`, and
    /// writes the result atomically.
    ///
    /// The read is inside the lock on purpose. Loading at process start and
    /// writing back much later is what let two riabuilds clobber each other:
    /// the later writer won with a snapshot from whenever it began. With the
    /// read here there is no stale snapshot, and so nothing to merge.
    ///
    /// There is deliberately no `save`. A method that writes without taking the
    /// lock is one a later change reaches for, and the lost update it brings
    /// back looks exactly like the bug this replaced.
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self> {
        // Contention here is milliseconds, so a wait is not worth a line on the
        // developer's terminal.
        let _lock = crate::filelock::FileLock::acquire(&paths.state_lock_file(), || {}).await?;
        let mut state = Self::load(paths).await;
        mutate(&mut state);
        write_json(&paths.state_file(), &state).await?;
        Ok(state)
    }

    pub fn mark_satisfied(&mut self, id: &str, version: u32, reason: &str) {
        self.tasks.insert(
            id.to_string(),
            TaskRecord {
                version,
                last_ok_at: now_secs(),
                last_reason: reason.to_string(),
            },
        );
    }

    pub fn forget(&mut self, id: &str) {
        self.tasks.remove(id);
    }

    /// Forgets records belonging to tasks riabuild no longer has.
    fn forget_retired(&mut self) {
        for id in RETIRED_TASKS {
            self.forget(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RealPaths;
    use tempfile::TempDir;

    #[tokio::test]
    async fn concurrent_state_updates_do_not_lose_each_other() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("mkdir");

        let mut writers = Vec::new();
        for n in 0..8 {
            let paths = RealPaths::rooted_at(home.path());
            writers.push(tokio::spawn(async move {
                State::update(&paths, |state| {
                    state.mark_satisfied(&format!("task_{n}"), 1, "never_run");
                })
                .await
                .expect("update");
            }));
        }
        for writer in writers {
            writer.await.expect("join");
        }

        let tasks = State::load(&paths).await.tasks;
        assert_eq!(
            tasks.len(),
            8,
            "a task record was lost; kept {:?}",
            tasks.keys().collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn round_trips_state() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());

        State::update(&paths, |state| {
            state.mark_satisfied("login", 1, "never_run")
        })
        .await
        .unwrap();

        let loaded = State::load(&paths).await;
        assert_eq!(loaded.tasks["login"].version, 1);
        assert_eq!(loaded.tasks["login"].last_reason, "never_run");
    }

    #[tokio::test]
    async fn unreadable_state_means_run_everything() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(paths.state_file(), "{{{ not json")
            .await
            .unwrap();

        // Not an error: a machine we cannot describe is a machine we re-check.
        assert!(State::load(&paths).await.tasks.is_empty());
    }

    #[tokio::test]
    async fn a_record_for_a_task_riabuild_no_longer_has_is_dropped() {
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("create ~/.riabuild");
        tokio::fs::write(
            paths.state_file(),
            r#"{"tasks":{
                "claude_profiles":{"version":1,"last_ok_at":1,"last_reason":"never_run"},
                "toolchain":{"version":2,"last_ok_at":2,"last_reason":"never_run"}
            }}"#,
        )
        .await
        .expect("write state");

        let state = State::load(&paths).await;

        assert!(
            !state.tasks.contains_key("claude_profiles"),
            "the retired record should be gone: {:?}",
            state.tasks.keys().collect::<Vec<_>>()
        );
        assert!(
            state.tasks.contains_key("toolchain"),
            "a live task's record must survive"
        );
    }

    #[tokio::test]
    async fn a_dropped_record_does_not_come_back_on_the_next_save() {
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("create ~/.riabuild");
        tokio::fs::write(
            paths.state_file(),
            r#"{"tasks":{"claude_profiles":{"version":1,"last_ok_at":1,"last_reason":"never_run"}}}"#,
        )
        .await
        .expect("write state");

        // `update` loads under the lock, which is where `forget_retired` runs,
        // so an empty closure is exactly the "next save" this is about.
        State::update(&paths, |_| {}).await.expect("save state");

        let written = tokio::fs::read_to_string(paths.state_file())
            .await
            .expect("read state");
        assert!(
            !written.contains("claude_profiles"),
            "the retired key must not be written back: {written}"
        );
    }
}

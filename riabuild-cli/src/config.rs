//! `~/.riabuild/state.json` and `~/.riabuild/config.json`.
//!
//! State records what each task last achieved. It is a cache of decisions, never
//! a source of truth about the machine — `check()` is that. A corrupt or missing
//! state file must therefore degrade to "run everything", not to an error.

use crate::paths::Paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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

    pub async fn save(&self, paths: &dyn Paths) -> Result<()> {
        write_json(&paths.state_file(), self).await
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    /// Where the repo lives. Absolute once chosen.
    #[serde(default)]
    pub project_path: Option<String>,
    /// Pinned by `toolchain` so every later run agrees on which Node is ours.
    #[serde(default)]
    pub node_version: Option<String>,
    #[serde(default)]
    pub pnpm_version: Option<String>,
    /// Claude Code config directories, in the order the developer numbers them.
    ///
    /// Position *is* the number: account 3 is index 2, and removing it makes
    /// what was account 4 into account 3 with no renumbering code at all. The
    /// UUID is the only identity anything persists.
    #[serde(default)]
    pub claude_accounts: Vec<String>,
    /// The single profile older riabuilds recorded.
    ///
    /// Read on load and folded into `claude_accounts`, never written back —
    /// which is what `skip_serializing` is for. Keeping it means a developer
    /// who upgrades does not lose the account they are already signed in to.
    #[serde(default, skip_serializing)]
    pub claude_profile: Option<String>,
    /// When this machine's riabuild session runs out, so `login` can refresh
    /// before a developer is interrupted. Not a secret — the token itself lives
    /// in the keychain.
    #[serde(default)]
    pub session_expires_at: Option<u64>,
    /// The `updatedAt` of the org Claude settings currently cached on disk.
    #[serde(default)]
    pub org_settings_updated_at: Option<u64>,
}

impl UserConfig {
    pub async fn load(paths: &dyn Paths) -> Self {
        let mut config: UserConfig = tokio::fs::read_to_string(paths.config_file())
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        config.fold_legacy_profile();
        config
    }

    pub async fn save(&self, paths: &dyn Paths) -> Result<()> {
        write_json(&paths.config_file(), self).await
    }

    /// Folds the single profile of an older riabuild into the account list.
    ///
    /// Takes the field rather than copying it, so no caller can read a value
    /// that will not be saved.
    fn fold_legacy_profile(&mut self) {
        // Taken unconditionally: a value that will not be saved must not be
        // readable either. `extend` over the Option keeps this one statement
        // rather than a nested `if`, which `clippy::collapsible_if` rejects.
        let legacy = self.claude_profile.take();
        if self.claude_accounts.is_empty() {
            self.claude_accounts.extend(legacy);
        }
    }
}

pub async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value)?;
    tokio::fs::write(path, format!("{text}\n"))
        .await
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Modification time of a file in epoch milliseconds, or 0 if unknown.
pub async fn modified_millis(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RealPaths;
    use tempfile::TempDir;

    #[tokio::test]
    async fn round_trips_state() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());

        let mut state = State::default();
        state.mark_satisfied("login", 1, "never_run");
        state.save(&paths).await.unwrap();

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

        let state = State::load(&paths).await;
        state.save(&paths).await.expect("save state");

        let written = tokio::fs::read_to_string(paths.state_file())
            .await
            .expect("read state");
        assert!(
            !written.contains("claude_profiles"),
            "the retired key must not be written back: {written}"
        );
    }

    #[tokio::test]
    async fn round_trips_user_config() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        let config = UserConfig {
            project_path: Some("/Users/ada/code/hub".into()),
            node_version: Some("22.23.1".into()),
            ..Default::default()
        };
        config.save(&paths).await.unwrap();

        let loaded = UserConfig::load(&paths).await;
        assert_eq!(loaded.project_path.as_deref(), Some("/Users/ada/code/hub"));
        assert_eq!(loaded.node_version.as_deref(), Some("22.23.1"));
        assert_eq!(loaded.claude_profile, None);
    }

    #[tokio::test]
    async fn a_legacy_profile_becomes_the_first_account() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(
            paths.config_file(),
            r#"{"claude_profile":"11111111-2222-4333-8444-555555555555"}"#,
        )
        .await
        .unwrap();

        let config = UserConfig::load(&paths).await;
        assert_eq!(
            config.claude_accounts,
            vec!["11111111-2222-4333-8444-555555555555".to_string()]
        );
        // Folded in on load, so nothing downstream ever sees the old field.
        assert_eq!(config.claude_profile, None);
        // The folded profile is the *primary* account, which is what position 1
        // means — read off the list, because that list is the only record of it.
        assert_eq!(
            config.claude_accounts.first().map(String::as_str),
            Some("11111111-2222-4333-8444-555555555555")
        );
    }

    #[tokio::test]
    async fn an_account_list_wins_over_a_legacy_profile() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(
            paths.config_file(),
            r#"{"claude_profile":"aaaaaaaa-2222-4333-8444-555555555555",
                "claude_accounts":["bbbbbbbb-2222-4333-8444-555555555555"]}"#,
        )
        .await
        .unwrap();

        let config = UserConfig::load(&paths).await;
        assert_eq!(
            config.claude_accounts,
            vec!["bbbbbbbb-2222-4333-8444-555555555555".to_string()]
        );
    }

    #[tokio::test]
    async fn saving_drops_the_legacy_profile_from_the_file() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        let config = UserConfig {
            claude_accounts: vec!["11111111-2222-4333-8444-555555555555".into()],
            claude_profile: Some("11111111-2222-4333-8444-555555555555".into()),
            ..Default::default()
        };
        config.save(&paths).await.unwrap();

        let text = tokio::fs::read_to_string(paths.config_file())
            .await
            .unwrap();
        assert!(!text.contains("claude_profile"), "{text}");
        assert!(text.contains("claude_accounts"), "{text}");
    }
}

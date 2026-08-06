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
        tokio::fs::read_to_string(paths.state_file())
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
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
    /// The Claude Code profile directory name (a UUID).
    #[serde(default)]
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
        tokio::fs::read_to_string(paths.config_file())
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub async fn save(&self, paths: &dyn Paths) -> Result<()> {
        write_json(&paths.config_file(), self).await
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
}

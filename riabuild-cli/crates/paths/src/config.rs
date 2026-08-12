//! `~/.riabuild/state.json` and `~/.riabuild/config.json`.
//!
//! State records what each task last achieved. It is a cache of decisions, never
//! a source of truth about the machine — `check()` is that. A corrupt or missing
//! state file must therefore degrade to "run everything", not to an error.

use crate::Paths;
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

    /// Acquires the lock, reads what is on disk *now*, applies `mutate`, and
    /// writes the result atomically. See `State::update` for why the read is
    /// inside the lock, and why there is no `save`.
    ///
    /// This one matters more than `State`'s. State is a cache, and a lost record
    /// costs one redundant `check()`. `config.json` is where the checkout path,
    /// the pinned versions and the ordered account list live — a lost update
    /// there drops a Claude account from the registry while its directory stays
    /// on disk, and because position *is* the account number, adopting that
    /// orphan later changes which account `claude-2` opens.
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self> {
        let _lock = crate::filelock::FileLock::acquire(&paths.state_lock_file(), || {}).await?;
        let mut config = Self::load(paths).await;
        mutate(&mut config);
        write_json(&paths.config_file(), &config).await?;
        Ok(config)
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
    let text = serde_json::to_string_pretty(value)?;
    write_atomic(path, format!("{text}\n").as_bytes()).await
}

/// Writes beside the target and renames over it, so a reader sees the whole old
/// file or the whole new one and never the gap between.
///
/// `tokio::fs::write` truncates and then writes, and an interrupt inside that
/// window leaves a truncated file. For `state.json` that is harmless — a cache
/// that will not parse means "check everything again". For `config.json` it is
/// not: `UserConfig::load` answers an unparseable file with `Default`, which
/// silently forgets the checkout, the pinned versions, and every Claude
/// account. Same reasoning as `archive/staging.rs`, and the same requirement
/// that the temporary share a directory with its target so the rename is atomic
/// rather than a copy.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("could not create {}", parent.display()))?;
    }

    let temp = temp_beside(path);
    let written = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(bytes).await?;
        // Durable before the rename, so a power loss cannot leave the new name
        // pointing at blocks that were never written.
        file.sync_all().await
    }
    .await;

    if let Err(error) = written {
        // Best effort: the error being returned says more than this could.
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error).with_context(|| format!("could not write {}", temp.display()));
    }

    tokio::fs::rename(&temp, path)
        .await
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

/// `…/.state.json.4171-3.tmp`, in the target's own directory.
///
/// The counter is not decoration, for the same reason `archive/staging.rs`
/// carries one: keyed on the pid alone, two writes to one path from a single
/// process would compute the same temporary and unpack over each other.
fn temp_beside(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let call = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}-{call}.tmp", std::process::id()))
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
    use crate::RealPaths;
    use tempfile::TempDir;

    /// Two `riabuild claude new` runs in two terminal windows.
    ///
    /// Before the lock, each run loaded `config.json` at startup and wrote its
    /// whole snapshot back later, so the later writer won with a list that
    /// never contained the earlier writer's account. The UUID vanished from the
    /// registry while its directory stayed on disk — and because position *is*
    /// the account number, adopting that orphan on a later run changes which
    /// account `claude-2` opens.
    #[tokio::test]
    async fn concurrent_account_additions_do_not_lose_an_account() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("mkdir");

        let mut writers = Vec::new();
        for n in 0..8 {
            let paths = RealPaths::rooted_at(home.path());
            writers.push(tokio::spawn(async move {
                UserConfig::update(&paths, |config| {
                    config.claude_accounts.push(format!("account-{n}"));
                })
                .await
                .expect("update");
            }));
        }
        for writer in writers {
            writer.await.expect("join");
        }

        let mut found = UserConfig::load(&paths).await.claude_accounts;
        found.sort();
        let expected: Vec<String> = (0..8).map(|n| format!("account-{n}")).collect();
        assert_eq!(found, expected, "an account was lost between two windows");
    }

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
    async fn an_update_returns_exactly_what_it_wrote() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());

        let written = UserConfig::update(&paths, |config| {
            config.project_path = Some("/srv/checkout".into());
        })
        .await
        .expect("update");

        assert_eq!(written.project_path.as_deref(), Some("/srv/checkout"));
        assert_eq!(
            UserConfig::load(&paths).await.project_path.as_deref(),
            Some("/srv/checkout"),
            "what was handed back must be what landed on disk"
        );
    }

    #[tokio::test]
    async fn a_write_leaves_no_temporary_behind() {
        let home = TempDir::new().unwrap();
        let path = home.path().join("state.json");

        write_json(&path, &State::default()).await.expect("write");

        let mut entries = tokio::fs::read_dir(home.path()).await.expect("read_dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(
            names,
            vec!["state.json".to_string()],
            "the temporary must be renamed away, not left beside the target"
        );
    }

    /// The torn-read regression. With `fs::write` this fails: it truncates
    /// before it writes, and `load` answers a truncated file with `Default`.
    #[tokio::test]
    async fn a_reader_never_observes_a_half_written_file() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("mkdir");

        let mut full = State::default();
        for n in 0..200 {
            full.mark_satisfied(&format!("task_{n}"), 1, "never_run");
        }
        write_json(&paths.state_file(), &full).await.expect("seed");

        let writer = {
            let paths = RealPaths::rooted_at(home.path());
            tokio::spawn(async move {
                for _ in 0..40 {
                    write_json(&paths.state_file(), &full).await.expect("write");
                    tokio::task::yield_now().await;
                }
            })
        };

        for _ in 0..40 {
            let seen = State::load(&paths).await;
            assert_eq!(
                seen.tasks.len(),
                200,
                "a reader saw a file that was neither the old one nor the new one"
            );
            tokio::task::yield_now().await;
        }

        writer.await.expect("join");
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

    #[tokio::test]
    async fn round_trips_user_config() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        UserConfig::update(&paths, |config| {
            config.project_path = Some("/Users/ada/code/hub".into());
            config.node_version = Some("22.23.1".into());
        })
        .await
        .unwrap();

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
        UserConfig::update(&paths, |config| {
            config.claude_accounts = vec!["11111111-2222-4333-8444-555555555555".into()];
            config.claude_profile = Some("11111111-2222-4333-8444-555555555555".into());
        })
        .await
        .unwrap();

        let text = tokio::fs::read_to_string(paths.config_file())
            .await
            .unwrap();
        assert!(!text.contains("claude_profile"), "{text}");
        assert!(text.contains("claude_accounts"), "{text}");
    }
}

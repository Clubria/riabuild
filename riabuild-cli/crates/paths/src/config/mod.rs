//! `~/.riabuild/state.json` and `~/.riabuild/config.json`.
//!
//! State records what each task last achieved. It is a cache of decisions, never
//! a source of truth about the machine — `check()` is that. A corrupt or missing
//! state file must therefore degrade to "run everything", not to an error.
//!
//! Three files under this directory. `state` is `state.json`, the cache above;
//! `user` is `config.json`, which is not a cache — losing it is re-onboarding,
//! and the difference between the two is argued out in each file. `atomic` is
//! riabuild's one atomic write, which both of them land through and which
//! `tasks::shims`, `remote::store` and `keychain`'s file store call directly.
//!
//! What stays here is the clock the two files record their timestamps against.

mod atomic;
mod state;
mod user;

pub use atomic::{write_atomic, write_json};
pub use state::{State, TaskRecord};
pub use user::UserConfig;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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

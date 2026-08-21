//! The developer's Claude Code accounts, in the order they are numbered.
//!
//! Position is the number: account 3 is `claude_accounts[2]`, and removing it
//! makes what was account 4 into account 3 without a line of renumbering code.
//! A design that stored the number would have an invariant to maintain on every
//! mutation, and would eventually fail to maintain it.
//!
//! Each account is a directory under `~/.riabuild/claude/<uuid>/` that Claude
//! Code is pointed at with `CLAUDE_CONFIG_DIR`. That variable scopes the login
//! as well as the settings — on macOS the keychain item is named for a hash of
//! the directory's path — so two accounts really are two independent sign-ins.

pub mod command;
pub mod render;
pub mod status;

use anyhow::Result;
use riabuild_paths::config::UserConfig;
use riabuild_ui::{Failure, plural};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Nine keeps every launcher name single-digit — `claude-1` … `claude-9` — and
/// makes `riabuild claude delete 12` an obvious mistake rather than something
/// to interpret.
pub const MAX: usize = 9;

/// A v4 UUID for an account directory name.
///
/// Infallible by design, and `rand::rng()` was not: it panics when the OS
/// entropy source is unreachable, which is the one failure this whole binary
/// is built to never have. Reading `getrandom` directly makes that a `Result`
/// worth answering, and the answer is the clock. An account id names a
/// directory and is never a secret — the keychain item derived from it is
/// guarded by an ACL, not by being hard to guess — so it only has to be
/// distinct from the at most eight others on this machine (`MAX`), which a
/// nanosecond timestamp is.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        bytes = nanos.to_le_bytes();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Whether a directory name is one riabuild would have created.
pub fn looks_like_id(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(expected, part)| part.len() == *expected)
        && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Every account directory actually on disk, in the order adoption would number
/// them: oldest first.
///
/// Here rather than beside the task that adopts them because "which directories
/// under `~/.riabuild/claude/` are accounts, and in what order" is a property of
/// the account model. `claude_accounts` asks it to adopt and to spot a directory
/// nothing recorded; `reset` asks it to count what a delete would destroy. Two
/// answers to that question would eventually disagree, and the one a developer
/// would meet is a reset warning naming a number no `riabuild claude list`
/// agrees with.
///
/// Ordered by creation time, falling back to modification time on a filesystem
/// that does not record one, and broken by the directory name so the order is
/// deterministic either way.
///
/// Creation time rather than mtime because Claude Code writes into
/// `CLAUDE_CONFIG_DIR` on every session: the account the developer actually uses
/// has the *newest* mtime, so sorting by that would hand `claude` to their least
/// recently used login on the one machine where the order is observable — config
/// lost, several directories on disk. Adoption is meant to keep their original
/// account as account 1.
pub async fn ids_on_disk(claude_dir: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(claude_dir).await else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, String)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !looks_like_id(&name) {
            continue;
        }
        // btime on APFS, statx on modern Linux. `or_else` and not `or`: both
        // answers come out of the same already-populated stat struct, so neither
        // is a syscall, but `or` would build the fallback for every entry
        // including the ones `created()` answered. Lazy is the right default
        // even where the cost is a `Result`.
        let born = meta
            .created()
            .or_else(|_| meta.modified())
            .unwrap_or(UNIX_EPOCH);
        found.push((born, name));
    }
    found.sort();
    found.into_iter().map(|(_, name)| name).collect()
}

/// The account a developer's number refers to.
///
/// `0` is not an account, and `wrapping_sub` turns it into an index nobody has
/// rather than into the last one.
pub fn id_of(config: &UserConfig, number: usize) -> Result<String> {
    match config.claude_accounts.get(number.wrapping_sub(1)) {
        Some(id) => Ok(id.clone()),
        None => Err(Failure::new(
            format!("finding Claude Code account {number}"),
            "Run `riabuild claude` to see the accounts you have.",
        )
        .detail(format!(
            "you have {}",
            plural(config.claude_accounts.len() as u64, "Claude Code account")
        ))
        .into()),
    }
}

/// Appends an account, refusing past `MAX`.
pub fn add(config: &mut UserConfig, id: String) -> Result<usize> {
    if config.claude_accounts.len() >= MAX {
        return Err(Failure::new(
            "adding a Claude Code account",
            "Delete one with `riabuild claude delete <number>` first.",
        )
        .detail(format!(
            "riabuild keeps at most {}, and you already have that many",
            plural(MAX as u64, "account")
        ))
        .into());
    }
    config.claude_accounts.push(id);
    Ok(config.claude_accounts.len())
}

/// Removes an account and returns its id.
///
/// Every later account shifts down one number. That is the feature, not a side
/// effect — see the module comment.
pub fn remove(config: &mut UserConfig, number: usize) -> Result<String> {
    let id = id_of(config, number)?;
    remove_id(config, &id);
    Ok(id)
}

/// Removes an account by id, and answers whether it was still registered.
///
/// **The spelling for a mutation applied under the config lock.** A number is a
/// *position*, and the list a command read at process start is not the list the
/// lock hands it: a second terminal that added or deleted an account in between
/// has shifted every number after it. Re-resolving a number inside the closure
/// would therefore act on whichever account had moved into that slot. An id is
/// the account itself, and it is what the directory a delete has already
/// removed was named for.
///
/// A missing id answers `false` rather than failing. Another riabuild having
/// removed the same account first is the state being asked for, not an error to
/// report to a developer who asked for exactly that.
pub fn remove_id(config: &mut UserConfig, id: &str) -> bool {
    let Some(index) = config.claude_accounts.iter().position(|held| held == id) else {
        return false;
    };
    config.claude_accounts.remove(index);
    true
}

/// Makes an account the primary one, preserving the order of the rest.
pub fn promote(config: &mut UserConfig, number: usize) -> Result<String> {
    let id = id_of(config, number)?;
    promote_id(config, &id);
    Ok(id)
}

/// Makes an account the primary one by id. Same reasoning as [`remove_id`].
pub fn promote_id(config: &mut UserConfig, id: &str) -> bool {
    if !remove_id(config, id) {
        return false;
    }
    config.claude_accounts.insert(0, id.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(count: usize) -> UserConfig {
        UserConfig {
            claude_accounts: (0..count).map(|n| format!("id-{n}")).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn deleting_an_account_renumbers_the_ones_after_it() {
        // The whole reason position is the number: this needs no renumbering
        // code, and so cannot have a renumbering bug.
        let mut config = config_with(5);
        assert_eq!(remove(&mut config, 3).unwrap(), "id-2");
        assert_eq!(config.claude_accounts, vec!["id-0", "id-1", "id-3", "id-4"]);
        assert_eq!(id_of(&config, 3).unwrap(), "id-3");
        assert_eq!(id_of(&config, 4).unwrap(), "id-4");
    }

    #[test]
    fn promoting_keeps_every_other_account_in_order() {
        let mut config = config_with(4);
        promote(&mut config, 3).unwrap();
        assert_eq!(config.claude_accounts, vec!["id-2", "id-0", "id-1", "id-3"]);
    }

    #[test]
    fn promoting_the_primary_changes_nothing() {
        let mut config = config_with(3);
        promote(&mut config, 1).unwrap();
        assert_eq!(config.claude_accounts, vec!["id-0", "id-1", "id-2"]);
    }

    #[test]
    fn a_tenth_account_is_refused() {
        let mut config = config_with(MAX);
        let error = add(&mut config, "one-too-many".into()).unwrap_err();
        assert!(error.to_string().contains("adding a Claude Code account"));
        assert_eq!(config.claude_accounts.len(), MAX);
    }

    #[test]
    fn a_number_nobody_has_is_refused_rather_than_wrapping() {
        let config = config_with(2);
        assert!(id_of(&config, 0).is_err());
        assert!(id_of(&config, 3).is_err());
        assert_eq!(id_of(&config, 2).unwrap(), "id-1");
    }

    #[test]
    fn generates_well_formed_ids() {
        let id = new_id();
        assert!(looks_like_id(&id), "{id}");
        assert_ne!(id, new_id());
        // Version 4, variant 1 — the bits a UUID library would set.
        assert_eq!(id.chars().nth(14), Some('4'));
        assert!(matches!(id.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
    }

    #[test]
    fn rejects_directories_that_are_not_accounts() {
        assert!(!looks_like_id("settings"));
        assert!(!looks_like_id("not-a-uuid"));
        assert!(!looks_like_id(""));
    }

    #[tokio::test]
    async fn only_directories_that_look_like_accounts_are_found() {
        // What `reset` counts and what `claude_accounts` adopts. A stray file —
        // or the `settings` directory an older riabuild left behind — is not an
        // account, and counting it puts a number in the reset warning that no
        // `riabuild claude list` agrees with.
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path();
        let id = new_id();
        tokio::fs::create_dir_all(dir.join(&id)).await.unwrap();
        tokio::fs::create_dir_all(dir.join("settings"))
            .await
            .unwrap();
        tokio::fs::write(dir.join("notes.txt"), "hi").await.unwrap();

        assert_eq!(ids_on_disk(dir).await, vec![id]);
        // A directory that is not there at all is no accounts, not an error.
        assert!(ids_on_disk(&dir.join("nowhere")).await.is_empty());
    }

    #[tokio::test]
    async fn the_older_directory_is_numbered_first() {
        // Account 1 is the one `claude` runs, so on the machine where adoption
        // happens at all — config lost, several directories on disk — getting
        // this backwards hands the developer's shell to the wrong login.
        //
        // This pins the ascending order and the tie-break. It does *not*
        // distinguish creation time from modification time: both directories are
        // made in sequence and never written to again, so the two orderings
        // agree. `creation_time_is_preferred_over_modification_time` is the test
        // that tells them apart, and it needs a filesystem that records a
        // creation time.
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path();
        let first = new_id();
        let second = new_id();
        tokio::fs::create_dir_all(dir.join(&first)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tokio::fs::create_dir_all(dir.join(&second)).await.unwrap();

        assert_eq!(ids_on_disk(dir).await, vec![first, second]);
    }

    #[tokio::test]
    #[ignore = "requires a filesystem that records creation time (APFS birthtime, statx btime)"]
    async fn creation_time_is_preferred_over_modification_time() {
        // The discriminating case, and the reason mtime is the wrong key: Claude
        // Code writes into CLAUDE_CONFIG_DIR on every session, so the account a
        // developer actually uses has the newest mtime. Here the older
        // directory is touched after the newer one exists, which is what a
        // session does — under an mtime sort it would become account 2.
        //
        // Ignored by default because `Metadata::created()` is an `Err` on a
        // filesystem without btime, where this legitimately falls back to mtime
        // and the assertion below cannot hold. Same reason as
        // `shims::claude_config_dir_smoke`: a real property, pinned where the
        // platform can answer for it.
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path();
        let older = new_id();
        let newer = new_id();
        tokio::fs::create_dir_all(dir.join(&older)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tokio::fs::create_dir_all(dir.join(&newer)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // A session in the older account: its mtime is now the newest of the two.
        tokio::fs::write(dir.join(&older).join(".claude.json"), "{}")
            .await
            .unwrap();

        assert_eq!(ids_on_disk(dir).await, vec![older, newer]);
    }
}

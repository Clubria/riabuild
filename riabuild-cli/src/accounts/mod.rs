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

// Nothing outside this module's own tests calls these yet — the identity
// lookup, the rendered box, the shims, and the `claude` subcommands are later
// tasks in this plan that wire them in. Remove this once one does; `dead_code`
// finding a real gap again is the point of not leaving it broader than needed.
#![allow(dead_code)]

pub mod render;
pub mod status;

use crate::config::UserConfig;
use crate::ui::{Failure, plural};
use anyhow::Result;
use rand::RngCore;

/// Nine keeps every launcher name single-digit — `claude-1` … `claude-9` — and
/// makes `riabuild claude delete 12` an obvious mistake rather than something
/// to interpret.
pub const MAX: usize = 9;

/// A v4 UUID for an account directory name.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
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
    config.claude_accounts.remove(number - 1);
    Ok(id)
}

/// Makes an account the primary one, preserving the order of the rest.
pub fn promote(config: &mut UserConfig, number: usize) -> Result<String> {
    let id = remove(config, number)?;
    config.claude_accounts.insert(0, id.clone());
    Ok(id)
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
}

//! Which sign-in a session runs under.
//!
//! riabuild keeps nine sign-ins for each of the three harnesses, and a developer
//! already knows them by the launchers on their `PATH`: `claude-2`, `codex-1`,
//! `grok-9`. This is that same set, handed to the window so a session can be
//! started under any of them rather than under the first one only.
//!
//! An account is resolved by the caller — from riabuild's own account list and
//! its path layout — and never here. This crate has no opinion about where a
//! profile lives; it carries what it was given and records it on the session,
//! because [`crate::store::Record`] is what a later turn resumes under.

use std::path::PathBuf;

use riabuild_harness::Kind;

/// One sign-in, as the window offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub kind: Kind,
    /// 1-based, and the same number the launcher carries: account 2 of Claude
    /// Code is `claude-2`. Position is the number for all three, which is why
    /// this is never stored anywhere it could disagree with the list it came
    /// from — see `riabuild_tasks::accounts`.
    pub number: usize,
    /// What [`Kind::home_env`] is set to for a turn under this account.
    ///
    /// `None` means "whatever the harness picks for itself", which is what a
    /// machine with no accounts set up yet has. It is not the same as the first
    /// account's home, and a session started under it is not resumable under
    /// one — which is exactly why the session records what it was started with.
    pub home: Option<PathBuf>,
}

impl Account {
    pub fn new(kind: Kind, number: usize, home: Option<PathBuf>) -> Self {
        Self { kind, number, home }
    }

    /// The name this account already goes by: `claude-2`, `grok-1`.
    ///
    /// Deliberately the launcher's spelling rather than a prettier one. A
    /// developer who ran `grok-3 auth login` an hour ago should be able to find
    /// that sign-in in this list without translating anything.
    pub fn name(&self) -> String {
        format!("{}-{}", self.kind.tag(), self.number)
    }
}

/// Every sign-in this window can start a session under, in the order shown.
///
/// Grouped by harness in [`Kind::ALL`] order and numbered within it, because
/// that is the order `riabuild claude` lists them in and the order the
/// launchers are numbered — one list, sorted one way, everywhere.
#[derive(Debug, Clone, Default)]
pub struct Accounts(Vec<Account>);

impl Accounts {
    pub fn all(&self) -> &[Account] {
        &self.0
    }

    pub fn get(&self, index: usize) -> Option<&Account> {
        self.0.get(index)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The account a harness opens on, which is its first.
    ///
    /// `None` only where a harness has no accounts at all, which the caller
    /// decides the meaning of: `riabuild agents` still opens a pane per harness,
    /// so it passes an account with no home rather than leaving one out.
    pub fn first(&self, kind: Kind) -> Option<&Account> {
        self.0.iter().find(|account| account.kind == kind)
    }

    /// Where `account` sits in this list, for opening the chooser on it.
    pub fn position(&self, kind: Kind, number: usize) -> Option<usize> {
        self.0
            .iter()
            .position(|account| account.kind == kind && account.number == number)
    }
}

impl From<Vec<Account>> for Accounts {
    fn from(all: Vec<Account>) -> Self {
        Self(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nine_of_everything() -> Accounts {
        let mut all = Vec::new();
        for kind in Kind::ALL {
            for number in 1..=9 {
                all.push(Account::new(kind, number, Some(PathBuf::from("/r"))));
            }
        }
        Accounts::from(all)
    }

    #[test]
    fn an_account_is_named_the_way_its_launcher_is() {
        // The developer signed in by running `grok-3 auth login`. Anything else
        // here would make them work out which row that was.
        assert_eq!(Account::new(Kind::Grok, 3, None).name(), "grok-3");
        assert_eq!(Account::new(Kind::Claude, 1, None).name(), "claude-1");
        assert_eq!(Account::new(Kind::Codex, 9, None).name(), "codex-9");
    }

    #[test]
    fn a_harness_opens_on_its_first_account() {
        let accounts = nine_of_everything();
        for kind in Kind::ALL {
            assert_eq!(accounts.first(kind).map(|a| a.number), Some(1), "{kind:?}");
        }
    }

    #[test]
    fn every_sign_in_riabuild_keeps_is_offered() {
        // The whole point: a window that could only ever reach account 1 left
        // the other eight of each harness with no way in at all.
        let accounts = nine_of_everything();
        assert_eq!(accounts.len(), 27);
        assert_eq!(accounts.position(Kind::Grok, 9), Some(26));
        assert!(accounts.position(Kind::Codex, 10).is_none());
    }
}

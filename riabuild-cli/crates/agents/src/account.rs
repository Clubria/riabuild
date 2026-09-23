//! Which sign-in a session runs under.
//!
//! riabuild keeps nine sign-ins for each of the three harnesses, and a developer
//! already knows them by the launchers on their `PATH`: `claude-2`, `codex-1`,
//! `grok-9`. This is that same set, handed to the window so a session can be
//! started under any of them rather than under the first one only — those of
//! them that are signed in.
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

/// A sign-in riabuild found signed in when the window opened.
///
/// The rail's NEW SESSION group is exactly these, one row each, and nothing
/// else: a sign-in that is not signed in has nowhere for a session to go, so
/// it is not offered at all. Signing in is done by the developer, outside this
/// window, with the harness's own command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    pub account: Account,
    /// The address it is signed in as, where the harness says. Claude Code
    /// does; Codex and Grok Build keep no address riabuild can read, and
    /// nothing is invented for them.
    pub email: Option<String>,
}

impl SignedIn {
    pub fn new(account: Account, email: Option<String>) -> Self {
        Self { account, email }
    }
}

/// How a developer signs a harness in, for the window to say so when nothing
/// is — spelled the way they would type it for that harness's first sign-in.
pub fn sign_in_command(kind: Kind) -> &'static str {
    match kind {
        Kind::Claude => "claude-1 auth login",
        Kind::Codex => "codex-1 login",
        Kind::Grok => "grok-1 login",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The first sign-in of each harness, all signed in and none with a known
    /// address — the rail most tests want.
    pub(crate) fn first_of_each() -> Vec<SignedIn> {
        Kind::ALL
            .into_iter()
            .map(|kind| SignedIn::new(Account::new(kind, 1, Some(PathBuf::from("/r"))), None))
            .collect()
    }

    #[test]
    fn an_account_is_named_the_way_its_launcher_is() {
        // The developer signed in by running `grok-3 login`. Anything else
        // here would make them work out which row that was.
        assert_eq!(Account::new(Kind::Grok, 3, None).name(), "grok-3");
        assert_eq!(Account::new(Kind::Claude, 1, None).name(), "claude-1");
        assert_eq!(Account::new(Kind::Codex, 9, None).name(), "codex-9");
    }

    #[test]
    fn every_harness_is_told_how_to_sign_in() {
        for kind in Kind::ALL {
            assert!(sign_in_command(kind).starts_with(kind.tag()), "{kind:?}");
        }
    }
}

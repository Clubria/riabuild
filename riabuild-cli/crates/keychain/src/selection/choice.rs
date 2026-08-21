//! The stores a machine can be given, and building the one that was chosen.
//!
//! Split from the decision beside it because there is no platform question in
//! here: `store_for` is handed an answer and constructs it. The one property
//! this file exists to hold is that the match is exhaustive and lives in a
//! single place — see `store_for` for what the three copies of it used to cost.

use crate::{EnvKeychain, FileKeychain, Keychain, SecretToolKeychain, SecurityCliKeychain};
use riabuild_runner::CommandRunner;
use std::path::PathBuf;
use std::sync::Arc;

/// The outcome of [`select`](super::select) — which store, and (for a file
/// store) where.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Choice {
    Env,
    /// A managed server's own session, in its namespace.
    ServerFile(PathBuf),
    Macos,
    Linux,
    /// A Linux machine with no Secret Service answering — see
    /// [`select`](super::select).
    LinuxFile(PathBuf),
}

/// Builds the store a [`Choice`] names.
///
/// One match, reached by all three of `for_platform_on`, `for_account_on` and
/// `for_password_on`. They each carried their own copy, two of them identical
/// and the third differing only in `account`, in different arm orders and with
/// different comments about which variants their caller could not produce —
/// which is a shape that reads as three decisions and is one. A `Choice` added
/// later is now a compile error here and nowhere else.
///
/// `account` is the whole of what the three wanted to say differently: `None`
/// is this machine's own session, and `Some(name)` is a named entry beside it —
/// a server's cached session, or a server's saved SSH password. It does not
/// reach the file stores, whose path already carries the distinction.
pub(super) fn store_for(
    choice: Choice,
    runner: Arc<dyn CommandRunner>,
    account: Option<&str>,
) -> Box<dyn Keychain> {
    match (choice, account) {
        (Choice::Env, _) => Box::new(EnvKeychain),
        (Choice::ServerFile(path), _) => Box::new(FileKeychain::server_namespace(path)),
        (Choice::LinuxFile(path), _) => Box::new(FileKeychain::keyringless_machine(path)),
        (Choice::Macos, Some(account)) => {
            Box::new(SecurityCliKeychain::for_account(runner, account))
        }
        (Choice::Macos, None) => Box::new(SecurityCliKeychain::new(runner)),
        (Choice::Linux, Some(account)) => {
            Box::new(SecretToolKeychain::for_account(runner, account))
        }
        (Choice::Linux, None) => Box::new(SecretToolKeychain::new(runner)),
    }
}

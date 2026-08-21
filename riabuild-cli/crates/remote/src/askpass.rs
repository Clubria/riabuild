//! Asking for a server's password once, and remembering it.
//!
//! ## Why there is a password at all
//!
//! `authorise` installs riabuild's own key and, when that works, nothing here
//! ever runs. It does not always work: a server can accept the key into
//! `authorized_keys` and still refuse to authenticate with it — an
//! `AuthorizedKeysFile` pointing elsewhere, a home directory on a mode sshd
//! will not trust, an `AuthenticationMethods publickey,password` policy. None
//! of those mean the developer cannot get in. They mean the developer gets in
//! with their password, so `authorise` warns and carries on rather than
//! stopping (see its module doc), and every `ssh` after that falls back to
//! password authentication on its own — `IdentitiesOnly=yes` restricts which
//! *keys* are offered, never which *methods*.
//!
//! ## Why riabuild now holds one
//!
//! `authorise`'s module doc used to argue that riabuild must never hold a
//! password, on two grounds. The first still stands: `Ui::ask` is a plain
//! `read_line` and echoes, which is why [`riabuild_ui::secret`] exists rather
//! than a reuse of it.
//!
//! The second — that there is no channel from riabuild to `ssh` that avoids
//! `ps` — was the load-bearing one, and it was wrong. `ssh` will not read a
//! password from stdin, but it will run the program named by `SSH_ASKPASS`
//! and read the answer from **that program's stdout pipe**. Nothing lands in
//! an argument vector, nothing lands in the environment but the *account
//! name*, and riabuild is not reimplementing SSH's password protocol — it is
//! answering a question SSH asked it. `sshpass -p <password>` is the approach
//! that doc was right to rule out: the secret sits in argv, readable by every
//! other developer on a shared box.
//!
//! One `riabuild remote` opens something like ten SSH connections —
//! `resolve_home`, four in the install step, the session write, `gh-sweep`,
//! `seed-github`, the setup run, the clipboard forward, the shell — each its
//! own process with its own authentication. Ten password prompts is worse
//! than the failure this replaces, so remembering the answer is not a
//! convenience on top of the fallback; it is what makes the fallback usable.
//!
//! ## The shape of it
//!
//! `SSH_ASKPASS` names a bare executable path — `ssh` appends the prompt text
//! as `argv[1]`, leaving nowhere to put a subcommand — so riabuild cannot
//! point it at its own binary and must write a shim that execs
//! `riabuild internal askpass`. [`ensure_helper`] writes it on every run, so a
//! binary that moved (a Homebrew upgrade, a `cargo install`) never leaves a
//! shim pointing at nothing.//!
//! [`store`] is the two slots a password lives in and what the helper answers
//! from them, [`helper`] is the shim itself, and [`verdict`] reads what one
//! connection says about the credential it used. This file is the account
//! names all three are keyed by, and what a prompt is asking for.

use super::Remote;

mod helper;
mod store;
mod verdict;

pub use helper::{ensure_helper, run_options, ssh_env};
pub use store::{Answer, accept, answer, discard_pending, forget, store};
pub use verdict::{Verdict, refused_a_password, verdict};

/// Names which server's password the helper is being asked for.
///
/// The account name, never the password: this is an environment variable, and
/// a environment is readable by anything that can read `/proc/<pid>/environ`.
pub const ACCOUNT_VAR: &str = "RIABUILD_ASKPASS_ACCOUNT";

/// Distinguishes a saved password from the session token
/// `keychain::remote_account` stores for the same server. One keychain, one
/// server, two different secrets — revoking the session must not forget the
/// password, and `remote forget` deletes both by name rather than by luck.
const ACCOUNT_PREFIX: &str = "remote-password:";

/// What separates a password the server has **taken** from one riabuild has
/// merely been **given**. See [`Slots`].
const PENDING_SUFFIX: &str = ".pending";

/// The keychain account this server's accepted password is stored under.
pub fn account(remote: &Remote) -> String {
    format!("{ACCOUNT_PREFIX}{}", remote.hash())
}

/// The account the *unaccepted* half lives under.
///
/// Deliberately built so [`hash_of`] refuses it: the suffix's `.` is not an
/// `is_ascii_alphanumeric`, so an `RIABUILD_ASKPASS_ACCOUNT` naming a pending
/// slot cannot be answered by the helper. Nothing should ever want to — the
/// pending slot is riabuild's own bookkeeping, not a place a password is read
/// back from to hand to `ssh`.
///
/// `pub(crate)` so the one test that watches `authorise` sweep this slot names
/// it through the function that builds it, rather than pasting the suffix into
/// an assertion — a test holding its own copy of the account shape would keep
/// passing after the shape changed. `hash_of` above is the reason nothing
/// outside the crate has any business with it.
pub(crate) fn pending_account(hash: &str) -> String {
    format!("{ACCOUNT_PREFIX}{hash}{PENDING_SUFFIX}")
}

/// The server hash back out of an account name.
///
/// The helper runs as its own process and is handed an account, not a
/// `Remote` — it has no `remotes.json` to consult and no host to parse. The
/// hash is what names the file the keyring-less fallback writes, so it has to
/// survive the round trip through the environment.
pub fn hash_of(account: &str) -> Option<&str> {
    account
        .strip_prefix(ACCOUNT_PREFIX)
        .filter(|hash| !hash.is_empty() && hash.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// What `ssh` is asking for, read out of the prompt text it hands the helper
/// as `argv[1]`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Asked {
    /// `ada@build-01's password:` — the account's password on the server.
    /// riabuild asked for this, so riabuild remembers it.
    Password,
    /// `Enter passphrase for key '/home/ada/.ssh/id_ed25519':` — the
    /// developer's own key, which riabuild neither generated nor manages.
    /// Answered, never saved.
    Passphrase,
}

/// Which of the two the prompt is. Pure, so the distinction is testable
/// without a terminal or an `ssh`.
///
/// Getting this wrong in the permissive direction is the expensive one: a
/// passphrase saved under this server's password account would be handed to
/// `sshd` as a password on every later connection, and the developer's own key
/// passphrase would be sitting in a store they never asked riabuild to put it
/// in. So the test is for the *passphrase* wording and everything else is a
/// password — an unrecognised prompt gets answered and forgotten, which is the
/// safe way to be wrong.
pub fn classify(prompt: &str) -> Asked {
    if prompt.to_ascii_lowercase().contains("passphrase") {
        Asked::Passphrase
    } else {
        Asked::Password
    }
}

/// A server the askpass tests are about. Defined once here rather than in each
/// of the four test modules below this file, so a change to what a `Remote`
/// carries lands in one place.
#[cfg(test)]
pub(crate) fn remote_fixture() -> Remote {
    Remote {
        name: "build-01".into(),
        host: "build-01.fly.dev".into(),
        port: 2222,
        user: "ada".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::askpass::remote_fixture as remote;
    #[test]
    fn a_password_and_a_session_are_two_accounts_for_one_server() {
        // `remote forget` deletes both, and a single account name would make
        // revoking the session silently forget the password too — or worse,
        // hand `ssh` a bearer token as a password.
        let hash = remote().hash();
        assert_ne!(account(&remote()), riabuild_keychain::remote_account(&hash));
        assert_eq!(hash_of(&account(&remote())), Some(hash.as_str()));
    }
    #[test]
    fn an_account_that_did_not_come_from_riabuild_names_no_file() {
        // The hash reaches `remote_password_file` as a path component, so a
        // value from the environment is validated rather than trusted:
        // `../../..` in an account name must not choose where a secret is
        // written.
        assert_eq!(hash_of("remote-password:../../etc/passwd"), None);
        assert_eq!(hash_of("remote-password:9f2c/../../x"), None);
        assert_eq!(hash_of("remote-password:"), None);
        assert_eq!(hash_of("remote:9f2c"), None);
        assert_eq!(hash_of("9f2c"), None);
        assert_eq!(
            hash_of("remote-password:9f2c0011aabb"),
            Some("9f2c0011aabb")
        );
    }
    #[test]
    fn a_key_passphrase_is_told_apart_from_an_account_password() {
        // The real wording, both of them, from OpenSSH.
        assert_eq!(
            classify("ada@build-01.fly.dev's password: "),
            Asked::Password
        );
        assert_eq!(classify("Password: "), Asked::Password);
        assert_eq!(
            classify("Enter passphrase for key '/home/ada/.ssh/id_ed25519': "),
            Asked::Passphrase
        );
        // A prompt riabuild does not recognise is answered and forgotten,
        // which is the safe direction to be wrong in: saving the developer's
        // own key passphrase as this server's password would hand it to sshd
        // on every later connection.
        assert_eq!(classify("Verification code: "), Asked::Password);
    }
    #[test]
    fn a_pending_slot_is_not_an_account_the_helper_will_answer_for() {
        // The pending half is riabuild's own bookkeeping. Nothing should ever
        // set `RIABUILD_ASKPASS_ACCOUNT` to it, and `hash_of` refusing it is
        // what makes "should" into "cannot".
        assert_eq!(hash_of(&pending_account("9f2c0011aabb")), None);
        assert_ne!(pending_account("9f2c0011aabb"), account(&remote()));
    }
}

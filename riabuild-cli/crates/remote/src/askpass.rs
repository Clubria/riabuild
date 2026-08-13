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
//! shim pointing at nothing.

use super::Remote;
use anyhow::Result;
use riabuild_keychain::Keychain;
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::Failure;
use std::path::PathBuf;
use std::sync::Arc;

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

/// The keychain account this server's password is stored under.
pub fn account(remote: &Remote) -> String {
    format!("{ACCOUNT_PREFIX}{}", remote.hash())
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

/// Where this account's password is kept. Keyring wherever there is one; see
/// `keychain::select_password_store` for the decision and why.
pub async fn store(
    runner: Arc<dyn CommandRunner>,
    paths: &dyn Paths,
    account: &str,
) -> Result<Box<dyn Keychain>> {
    let Some(hash) = hash_of(account) else {
        // Reachable only by something other than riabuild setting
        // `RIABUILD_ASKPASS_ACCOUNT`, which has no business being answered:
        // an unvalidated value reaches `remote_password_file` as a path
        // component.
        return Err(Failure::new(
            "answering an SSH password prompt",
            "Run `riabuild remote` rather than the askpass helper directly.",
        )
        .detail(format!(
            "`{ACCOUNT_VAR}` is not a riabuild password account"
        ))
        .into());
    };
    Ok(riabuild_keychain::for_password(runner, account, paths.remote_password_file(hash)).await)
}

/// The shim `SSH_ASKPASS` points at, rewritten on every run.
///
/// Mode 0700 like every other directory riabuild owns under `~/.riabuild`:
/// this one is only a path to riabuild's own binary, but it is a path `ssh`
/// will execute, and a co-tenant who can write it can answer password prompts
/// on this developer's behalf.
pub async fn ensure_helper(paths: &dyn Paths) -> Result<PathBuf> {
    let binary = std::env::current_exe().map_err(|error| {
        Failure::new(
            "preparing to ask for that server's password",
            "Run `riabuild remote` again from an installed riabuild.",
        )
        .detail(format!("could not locate riabuild's own binary: {error}"))
    })?;

    let dir = paths.ssh_dir();
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(&dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }

    let path = paths.askpass_helper();
    // `exec`, so `ssh` waits on riabuild itself rather than on a shell that
    // outlives it, and the prompt text `ssh` appends is passed through
    // untouched — it is what tells the helper a passphrase from a password.
    tokio::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             # Generated by riabuild. Edits here are overwritten.\n\
             exec {} internal askpass \"$@\"\n",
            super::shell_quote(&binary.to_string_lossy()),
        ),
    )
    .await?;
    make_executable(&path).await?;
    Ok(path)
}

#[cfg(unix)]
async fn make_executable(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn make_executable(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// The environment every `ssh`, `mosh` and `ssh-copy-id` riabuild starts for
/// a server carries.
///
/// `SSH_ASKPASS_REQUIRE=force` is what makes `ssh` consult the helper even
/// though a terminal exists — without it, askpass is reached only when there
/// is no tty *and* `DISPLAY` is set, which is neither of riabuild's cases. It
/// needs OpenSSH 8.4 (2020) or newer; older clients ignore the variable
/// entirely and prompt on the terminal themselves, which is exactly today's
/// behaviour, so an old `ssh` degrades rather than breaks.
pub fn ssh_env(remote: &Remote, paths: &dyn Paths) -> Vec<(String, String)> {
    vec![
        (
            "SSH_ASKPASS".to_string(),
            paths.askpass_helper().to_string_lossy().into_owned(),
        ),
        ("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string()),
        (ACCOUNT_VAR.to_string(), account(remote)),
    ]
}

/// [`ssh_env`] as a ready-made [`RunOptions`], which is what almost every
/// caller wants. Sites that also pipe stdin build
/// `RunOptions { stdin: Some(…), ..run_options(remote, paths) }`.
pub fn run_options(remote: &Remote, paths: &dyn Paths) -> RunOptions {
    RunOptions {
        env: ssh_env(remote, paths),
        ..Default::default()
    }
}

/// What the helper will hand back to `ssh`.
pub struct Answer {
    /// The password or passphrase itself.
    pub secret: String,
    /// Why it could not be remembered, if it could not.
    ///
    /// Never fatal, and deliberately not an `Err`: the answer in hand is
    /// right whether or not it could be written down, and failing here would
    /// turn a keyring that is merely locked into a server nobody can reach.
    /// The caller says so on stderr, which is what stops the developer
    /// wondering why the next connection asks again.
    pub not_saved: Option<String>,
}

/// Decides what to answer and whether to remember it.
///
/// `ask` is a closure rather than a direct call into [`riabuild_ui::secret`] so
/// the decision is testable without a terminal — which matters more here than
/// usual, because the branch that must *not* ask is the one that runs on
/// every connection after the first.
pub async fn answer(
    store: &dyn Keychain,
    prompt: &str,
    ask: impl FnOnce(&str) -> Result<String>,
) -> Result<Answer> {
    // The developer's own key, for a key riabuild neither generated nor
    // manages. Answered so `ssh-copy-id` can still use an existing key to
    // authorise the new one; never stored, and the store is not even read —
    // a saved *password* offered as a key passphrase would fail the key and
    // silently drop the identity that was about to work.
    if classify(prompt) == Asked::Passphrase {
        return Ok(Answer {
            secret: ask(prompt)?,
            not_saved: None,
        });
    }

    // A store that cannot be read is a miss, not a failure.
    if let Ok(Some(saved)) = store.get().await {
        return Ok(Answer {
            secret: saved,
            not_saved: None,
        });
    }

    let secret = ask(prompt)?;
    let not_saved = store
        .set(&secret)
        .await
        .err()
        .map(|error| error.to_string());
    Ok(Answer { secret, not_saved })
}

/// Forgets a saved password.
///
/// Called by `remote forget` beside the session it revokes, and by the flow
/// when the server rejects what was saved — a stale password that is never
/// cleared turns one wrong answer into every future run failing without ever
/// asking again.
pub async fn forget(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
) -> Result<()> {
    store(runner, paths, &account(remote)).await?.delete().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

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

    #[tokio::test]
    async fn a_bad_account_is_refused_rather_than_answered() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        // `.err()` rather than `expect_err`: the `Ok` side is a
        // `Box<dyn Keychain>`, which has no `Debug` for the panic message.
        let error = store(runner, &paths, "remote-password:../secrets")
            .await
            .err()
            .expect("a traversal in the account name is not a server");
        assert!(
            error.downcast_ref::<Failure>().is_some(),
            "must be an actionable Failure, not a bare error"
        );
    }

    #[test]
    fn every_ssh_riabuild_starts_names_the_helper_and_the_account() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let env = ssh_env(&remote(), &paths);
        let value = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };

        assert_eq!(
            value("SSH_ASKPASS"),
            paths.askpass_helper().to_string_lossy()
        );
        // Without `force`, `ssh` reaches the helper only when there is no tty
        // and `DISPLAY` is set — neither of which is riabuild's case, so the
        // helper would never run and every hop would prompt again.
        assert_eq!(value("SSH_ASKPASS_REQUIRE"), "force");
        assert_eq!(value(ACCOUNT_VAR), account(&remote()));
        // The password itself is never one of these: an environment is
        // readable through /proc/<pid>/environ.
        assert!(
            env.iter().all(|(_, v)| !v.contains("hunter2")),
            "only the account name may travel in the environment"
        );
    }

    /// Stands in for the terminal, and records whether it was reached at all.
    /// Every assertion below about "does not ask" is really an assertion that
    /// this was not called.
    fn typed(answer: &str, asked: &std::cell::Cell<bool>) -> impl FnOnce(&str) -> Result<String> {
        move |_prompt: &str| {
            asked.set(true);
            Ok(answer.to_string())
        }
    }

    #[tokio::test]
    async fn a_saved_password_is_reused_without_asking_again() {
        // The whole reason the password is saved: one `riabuild remote` opens
        // around ten connections, and this is what makes nine of them silent.
        let asked = std::cell::Cell::new(false);
        let store = riabuild_keychain::MemoryKeychain::with_token("hunter2");

        let answer = answer(&store, "ada@build-01's password: ", typed("typed", &asked))
            .await
            .expect("answers");

        assert_eq!(answer.secret, "hunter2");
        assert!(!asked.get(), "a saved password must not be asked for again");
    }

    #[tokio::test]
    async fn a_password_asked_for_once_is_remembered() {
        let asked = std::cell::Cell::new(false);
        let store = riabuild_keychain::MemoryKeychain::default();

        let answer = answer(
            &store,
            "ada@build-01's password: ",
            typed("hunter2", &asked),
        )
        .await
        .expect("answers");

        assert_eq!(answer.secret, "hunter2");
        assert!(asked.get(), "an empty store has to ask");
        assert!(answer.not_saved.is_none(), "{:?}", answer.not_saved);
        assert_eq!(
            store.get().await.expect("readable"),
            Some("hunter2".to_string()),
            "the next connection has to find it"
        );
    }

    #[tokio::test]
    async fn a_key_passphrase_is_answered_but_never_written_down() {
        // Two failures avoided, not one. Saving it would put the developer's
        // own key passphrase in a store they never asked riabuild to use —
        // and *reading* the store here would offer this server's password as
        // a passphrase, failing the key and silently dropping the identity
        // that was about to authorise the new one.
        let asked = std::cell::Cell::new(false);
        let store = riabuild_keychain::MemoryKeychain::with_token("the-servers-password");

        let answer = answer(
            &store,
            "Enter passphrase for key '/home/ada/.ssh/id_ed25519': ",
            typed("my-key-passphrase", &asked),
        )
        .await
        .expect("answers");

        assert_eq!(answer.secret, "my-key-passphrase");
        assert!(asked.get(), "a passphrase must be asked for, not looked up");
        assert_eq!(
            store.get().await.expect("readable"),
            Some("the-servers-password".to_string()),
            "the stored password must be neither read for this nor overwritten by it"
        );
    }

    #[tokio::test]
    async fn a_password_that_could_not_be_saved_is_still_the_answer() {
        // A locked or missing keyring must not become a server nobody can
        // reach: the password in hand is right whether or not it could be
        // written down.
        struct Unwritable;
        #[async_trait::async_trait]
        impl Keychain for Unwritable {
            async fn get(&self) -> Result<Option<String>> {
                Err(anyhow::anyhow!("no keyring daemon"))
            }
            async fn set(&self, _token: &str) -> Result<()> {
                Err(anyhow::anyhow!("no keyring daemon"))
            }
            async fn delete(&self) -> Result<()> {
                Ok(())
            }
            fn describe(&self) -> &'static str {
                "broken (test)"
            }
        }

        let asked = std::cell::Cell::new(false);
        let answer = answer(&Unwritable, "Password: ", typed("hunter2", &asked))
            .await
            .expect("a broken store is not a failed answer");

        assert_eq!(answer.secret, "hunter2");
        assert!(asked.get(), "an unreadable store is a miss, so it must ask");
        assert!(
            answer.not_saved.is_some(),
            "the developer has to be told why it will ask again"
        );
    }

    #[tokio::test]
    async fn the_helper_is_executable_and_execs_riabuilds_own_binary() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());

        let path = ensure_helper(&paths).await.expect("writes the helper");
        let script = tokio::fs::read_to_string(&path).await.expect("read");

        assert!(script.starts_with("#!/bin/sh\n"), "{script}");
        assert!(script.contains("internal askpass"), "{script}");
        // `"$@"` is what forwards the prompt text `ssh` appends — the helper
        // reads it to tell a key passphrase from an account password, and a
        // shim that dropped it would save the developer's own key passphrase
        // into the keychain as though it were the server's password.
        assert!(script.contains("\"$@\""), "{script}");
        assert!(script.trim_end().ends_with("\"$@\""), "{script}");

        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "ssh must be able to execute it, nobody else");
    }

    #[tokio::test]
    async fn a_helper_from_an_older_run_is_rewritten_not_left_alone() {
        // riabuild moves: a Homebrew upgrade, an apt upgrade, a `cargo build`
        // in a worktree. A shim written once and then trusted points at a
        // binary that is no longer there, and `ssh` answers every password
        // prompt with nothing at all.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.ssh_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(paths.askpass_helper(), "#!/bin/sh\nexec /gone/riabuild\n")
            .await
            .expect("write a stale shim");

        ensure_helper(&paths).await.expect("rewrites it");

        let script = tokio::fs::read_to_string(paths.askpass_helper())
            .await
            .expect("read");
        assert!(!script.contains("/gone/riabuild"), "{script}");
        assert!(script.contains("internal askpass"), "{script}");
    }
}

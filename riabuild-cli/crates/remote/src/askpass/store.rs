//! The two slots a server's password lives in, and answering `ssh` from them.
//!
//! Two rather than one because a password riabuild has *not* seen work is not
//! a password riabuild may offer: it is promoted to the accepted slot only
//! once a connection using it has authenticated, and dropped when sshd refuses
//! it. Without that, one typo answers every prompt on every future run.

use std::sync::Arc;

use anyhow::Result;
use riabuild_keychain::Keychain;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::Failure;

use super::{
    ACCOUNT_PREFIX, ACCOUNT_VAR, Asked, PENDING_SUFFIX, classify, hash_of, pending_account,
};
use crate::Remote;

/// The two places one server's password can be, and the rule that moves it
/// between them.
///
/// **`get` reads only what the server has accepted; `set` writes only what it
/// has not.** That asymmetry is the whole of the fix for a password riabuild
/// used to save the instant it was typed and then replay for ever. `ssh` hands
/// the helper a prompt from *inside* an authentication attempt and never tells
/// it how the attempt ended, so the helper cannot know whether the answer it
/// just gave was right — and one mistyped password, written straight into the
/// account the next `get` reads, became the answer to all ten connections of
/// every future run. sshd counts those, and enough of them lock the account.
///
/// So the helper's answer lands in `pending`, which nothing ever reads back as
/// a password, and it is promoted by [`accept`] once an `ssh` carrying it has
/// authenticated — or dropped by [`forget`] when sshd refuses it. That also
/// fixes the smaller version of the same bug inside a single connection: `ssh`
/// re-prompts up to three times, and the old store answered attempts two and
/// three with the typo from attempt one, so a slip of the finger could not be
/// corrected without `riabuild remote forget`.
struct Slots {
    accepted: Box<dyn Keychain>,
    pending: Box<dyn Keychain>,
}

impl Slots {
    /// Moves the pending half across, now that an `ssh` carrying it has
    /// authenticated. A no-op when there is nothing pending, which is the
    /// ordinary case — the developer already had one saved, or a key got in
    /// and no password was ever asked for.
    async fn accept(&self) -> Result<()> {
        let Some(secret) = self.pending.get().await? else {
            return Ok(());
        };
        self.accepted.set(&secret).await?;
        self.pending.delete().await
    }

    /// Drops the unconfirmed half and leaves the accepted one alone.
    async fn discard(&self) -> Result<()> {
        self.pending.delete().await
    }
}

#[async_trait::async_trait]
impl Keychain for Slots {
    async fn get(&self) -> Result<Option<String>> {
        self.accepted.get().await
    }

    async fn set(&self, secret: &str) -> Result<()> {
        self.pending.set(secret).await
    }

    /// Both, always. `remote forget` deletes "the password for this server",
    /// and a half riabuild happened to be holding unconfirmed is still a
    /// secret it should no longer have. Both are attempted whatever the first
    /// answers, so a keyring that refuses one does not leave the other behind.
    async fn delete(&self) -> Result<()> {
        let pending = self.pending.delete().await;
        let accepted = self.accepted.delete().await;
        accepted.and(pending)
    }

    fn describe(&self) -> &'static str {
        self.accepted.describe()
    }
}

/// Where this server's password is kept. Keyring wherever there is one; see
/// `keychain::select_password_store` for the decision and why.
///
/// Two stores means two `keyring_answers` probes — a second `secret-tool
/// lookup` on a Linux laptop, and none at all on macOS, which short-circuits
/// before the probe. Named because the helper runs inside an authentication
/// attempt and `internal::askpass`'s doc is right that nothing slow belongs
/// there: a read-only lookup against a service that is already answering is
/// milliseconds, and building the pending store lazily to save one would put
/// the store behind a constructor no test could hand a `MemoryKeychain` to.
async fn slots(runner: Arc<dyn CommandRunner>, paths: &dyn Paths, hash: &str) -> Slots {
    Slots {
        accepted: riabuild_keychain::for_password(
            runner.clone(),
            &format!("{ACCOUNT_PREFIX}{hash}"),
            paths.remote_password_file(hash),
        )
        .await,
        pending: riabuild_keychain::for_password(
            runner,
            &pending_account(hash),
            paths.remote_password_file(&format!("{hash}{PENDING_SUFFIX}")),
        )
        .await,
    }
}

/// The store the askpass helper answers out of — see [`Slots`] for what its
/// `get` and `set` actually reach.
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
    Ok(Box::new(slots(runner, paths, hash).await))
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

    // Written down, but not yet *kept*: `Slots::set` puts this in the pending
    // slot, which the `get` above will never read. It becomes this server's
    // password when `accept` says the server took it, and is deleted when
    // `forget` says the server refused it.
    let secret = ask(prompt)?;
    let not_saved = store
        .set(&secret)
        .await
        .err()
        .map(|error| error.to_string());
    Ok(Answer { secret, not_saved })
}

/// Forgets a saved password — both slots, accepted and pending alike.
///
/// Called by `remote forget` beside the session it revokes, and by the copy
/// step when the server rejects what was saved — a stale password that is
/// never cleared turns one wrong answer into every future run failing without
/// ever asking again, which is what the remote-password spec means by "a
/// stored password that the server rejects is cleared".
pub async fn forget(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
) -> Result<()> {
    slots(runner, paths, &remote.hash()).await.delete().await
}

/// Promotes the password the helper was given, now that an `ssh` carrying it
/// has authenticated.
///
/// A no-op when nothing is pending, which is the ordinary case: the developer
/// had one saved already, or riabuild's own key — or an issued one — got in
/// and no password was ever asked for.
pub async fn accept(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
) -> Result<()> {
    slots(runner, paths, &remote.hash()).await.accept().await
}

/// Drops a password the helper wrote down and nothing ever confirmed.
///
/// A run killed between the helper's write and [`crate::authorise::copy`]'s
/// verdict leaves one there. It is inert — nothing reads the pending slot back
/// as a password, and `remote forget` clears it with the rest — but nothing
/// swept it either, so a laptop that later got riabuild's own key onto that
/// server kept an unconfirmed password for it indefinitely, in the keyring,
/// for a server that no longer needs one.
///
/// Called from the one place that knows there is nothing to confirm: the
/// `can_sign_in` early return in [`crate::authorise::authorise`], where
/// riabuild's own key just worked and no password was asked for. Never from
/// `can_sign_in` itself, which `--check` also calls and which must leave the
/// machine exactly as it found it, and never on the [`Verdict::Unanswered`]
/// path, where the answer is still waiting on a later run to confirm it.
pub async fn discard_pending(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
) -> Result<()> {
    slots(runner, paths, &remote.hash()).await.discard().await
}
#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;

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

    /// Two empty slots, with no keyring and no platform in the way — the
    /// asymmetry between them is the behaviour under test, and which CLI a
    /// given machine would store them through is not.
    fn two_slots() -> Slots {
        Slots {
            accepted: Box::new(riabuild_keychain::MemoryKeychain::default()),
            pending: Box::new(riabuild_keychain::MemoryKeychain::default()),
        }
    }

    #[tokio::test]
    async fn a_password_asked_for_once_is_remembered_once_the_server_takes_it() {
        let asked = std::cell::Cell::new(false);
        let store = two_slots();

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

        store.accept().await.expect("the server took it");
        assert_eq!(
            store.get().await.expect("readable"),
            Some("hunter2".to_string()),
            "the next connection has to find it"
        );
        assert_eq!(
            store.pending.get().await.expect("readable"),
            None,
            "and it must not be left in both places"
        );
    }

    #[tokio::test]
    async fn a_password_the_server_has_not_taken_is_never_offered_to_anything() {
        // I036, the critical one. `answer` used to write straight into the
        // account the next `get` reads, so a single mistyped password became
        // the answer to every prompt of all ten connections of every future
        // run — roughly thirty failed authentications a run, enough to lock
        // the account, and no way out but `riabuild remote forget`.
        //
        // `ssh` cannot tell the helper how the attempt ended, so "only persist
        // what the server accepted" cannot be a decision the helper makes: it
        // is `accept` and `forget`, called by the one connection that knows.
        let asked = std::cell::Cell::new(false);
        let store = two_slots();

        let first = answer(&store, "ada@build-01's password: ", typed("typo", &asked))
            .await
            .expect("answers");
        assert_eq!(first.secret, "typo", "ssh still gets the answer");
        assert!(
            first.not_saved.is_none(),
            "and it was written down: {:?}",
            first.not_saved
        );

        assert_eq!(
            store.get().await.expect("readable"),
            None,
            "nothing may read back a password the server has not accepted"
        );

        // The same bug inside one connection: `ssh` re-prompts up to three
        // times, and the old store answered attempts two and three with the
        // typo from attempt one — so a slip of the finger could not be
        // corrected at the prompt that was offering to correct it.
        let again = std::cell::Cell::new(false);
        let second = answer(
            &store,
            "ada@build-01's password: ",
            typed("hunter2", &again),
        )
        .await
        .expect("answers");
        assert!(again.get(), "the retry has to reach the developer");
        assert_eq!(second.secret, "hunter2");
    }

    #[tokio::test]
    async fn forgetting_a_password_clears_both_halves_of_it() {
        // `remote forget` deletes "the password for this server", and a half
        // riabuild happened to be holding unconfirmed is still a secret it
        // should no longer have.
        let store = two_slots();
        store.accepted.set("accepted").await.expect("set");
        store.pending.set("pending").await.expect("set");

        store.delete().await.expect("deletes");

        assert_eq!(store.accepted.get().await.expect("readable"), None);
        assert_eq!(store.pending.get().await.expect("readable"), None);
    }

    #[tokio::test]
    async fn a_password_nothing_will_ever_confirm_is_swept_and_a_working_one_is_not() {
        // A run killed between the helper's write and `copy`'s verdict leaves
        // the pending half behind, and on every later run where riabuild's own
        // key works there is nothing that would ever promote or clear it —
        // `accept` and `forget` are both reached from the copy, which that run
        // never performs. It is inert, and it is still a secret riabuild is
        // holding for a server that no longer needs one.
        let store = two_slots();
        store.accepted.set("accepted").await.expect("set");
        store.pending.set("unconfirmed").await.expect("set");

        store.discard().await.expect("discards");

        assert_eq!(store.pending.get().await.expect("readable"), None);
        assert_eq!(
            store.accepted.get().await.expect("readable"),
            Some("accepted".to_string()),
            "the password that does work is not what this sweeps"
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
}

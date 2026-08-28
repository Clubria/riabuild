//! `riabuild remote forget <name>` — the reverse of `flow::connect_and_setup`.
//!
//! Kept separate from `flow.rs` for the same reason `flow.rs` was itself split
//! out of `mod.rs` (see its module doc): folding the teardown in as well
//! pushed that file well past the crate's ~300-line production budget. Nothing
//! here shares state with the setup flow — it is reached from a different arm
//! of the same `match`, takes its own arguments, and returns before the setup
//! path begins — so the split costs no threading.
//!
//! [`api`] holds the two calls this makes to riabuild-web, [`session`] is
//! revoking the server's session, and [`server_side`] is the traces left on
//! the server itself. What stays here is the command and this laptop's own
//! records.

use super::{Remote, identity, store, windows};
use anyhow::{Result, anyhow};
use riabuild_api::ApiClient;
use riabuild_keychain as keychain;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::Ui;
use std::sync::Arc;
mod api;
mod revoke;
mod server_side;

use api::{ApiRevokes, Carries, IssuedCarries, Revokes};
use revoke::revoke_session;
use server_side::cleanup_server_side;

/// `riabuild remote forget <name>` — done in the one order that is safe to
/// interrupt: revoke on riabuild-web, then best-effort clean up the server,
/// then delete what is local.
///
/// **Why this order and no other.** An earlier draft deleted the local SSH
/// key first. That left `ssh -o IdentitiesOnly=yes` unable to authenticate,
/// so the server-side cleanup silently failed, and the store entry was gone
/// too — nobody could retry, and the token stayed live on the server
/// forever, unrecorded anywhere on this laptop. Revoking first means that if
/// anything after it fails, the token is already dead: a live credential
/// with no local record of it is the one state this function must never
/// produce, but a dead credential whose local record briefly outlives it is
/// harmless.
///
/// **What "unreachable" means at each step, and why they differ.** The API
/// revoke talks to riabuild-web, which this laptop needs for everything else
/// it does; a failure there stops this function outright; loudly, before
/// anything local changes, because the token's fate is genuinely unknown.
/// The SSH cleanup talks to the server being forgotten, which may be off,
/// rebuilt, or simply unreachable from here right now — that failure is
/// reported but never fatal, because a server that happens to be down must
/// not become a server nobody can ever forget. The local delete (keychain
/// item, key pair, `remotes.json` entry) always runs once the API step has
/// succeeded, for the same reason: those are the developer's own records,
/// not the server's, and there is nothing left that could make deleting them
/// unsafe.
pub async fn forget_remote(
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    api: &ApiClient,
    member_id: &str,
    store: &mut store::Store,
    name: &str,
) -> Result<()> {
    forget_with(
        paths,
        runner,
        ui,
        &ApiRevokes(api),
        &IssuedCarries(api),
        member_id,
        store,
        name,
    )
    .await
}

/// The body of [`forget_remote`], taking [`Revokes`] and [`Carries`] as seams.
#[allow(clippy::too_many_arguments)]
async fn forget_with(
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    revokes: &dyn Revokes,
    carries: &dyn Carries,
    member_id: &str,
    store: &mut store::Store,
    name: &str,
) -> Result<()> {
    let Some(record) = store.find(name).cloned() else {
        return Err(anyhow!("there is no saved server named \"{name}\""));
    };

    warn_about_open_windows(paths, ui, &record).await;

    retire_identity(paths, runner, ui, revokes, carries, member_id, &record).await?;
    super::store::forget_one(paths, store, name).await?;

    ui.note(&format!("Forgot {}.", record.display_name()));
    Ok(())
}

/// Says out loud that this will end sessions the developer has open elsewhere.
///
/// **A warning and not a refusal.** `forget` is a destructive command the
/// developer typed by name, and the reason to run it is usually that something
/// about that server has gone wrong — so a riabuild that refused while a window
/// was open would be a riabuild that cannot clean up the case it is most needed
/// for, and there is no prompt to fall back on: this runs unattended in
/// `shared::reconcile` too. What was wrong before was not that it went ahead;
/// it is that it went ahead in silence.
///
/// Said *first*, before anything is revoked, because a sentence that arrives
/// after the session is dead tells the developer what happened rather than what
/// is about to. It names what will break, because "another window is open" is
/// not actionable and "the shell in your other terminal will stop working" is.
///
/// Counts this laptop's own windows and claims nothing more — see `windows`. A
/// colleague on a second laptop leaves no trace here, which is why the sentence
/// says "your".
async fn warn_about_open_windows(paths: &dyn Paths, ui: &Ui, record: &store::Record) {
    let remote: Remote = record.into();
    let open = windows::live(paths, &remote).await;
    if open == 0 {
        return;
    }
    ui.warn(&format!(
        "{} of your riabuild windows {} still connected to {}. Forgetting it revokes that \
         server's session and takes riabuild's key back out of its authorized_keys, so the \
         shells in those terminals will stop working.",
        open,
        if open == 1 { "is" } else { "are" },
        record.display_name()
    ));
}

/// Everything [`forget_remote`] does *except* dropping the record: revoke the
/// session, clean up on the server, delete what is local.
///
/// Extracted because a lead editing one of the team's server addresses produces
/// the same situation as a forget, minus the forgetting. `Remote::hash` is
/// taken over `user@host:port`, so an edited address is a different identity —
/// leaving behind a key riabuild authorised on the old machine and, if this
/// laptop ever connected, a live session on it. What has to happen to that
/// machine is exactly this, and the record then goes on to describe the new
/// one. See `shared::reconcile`.
///
/// The order is the one [`forget_remote`]'s own doc argues for, and for the
/// same reason: revoke first, so that anything failing after it leaves a dead
/// credential rather than a live one nothing on this laptop still records.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn retire_identity(
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    revokes: &dyn Revokes,
    carries: &dyn Carries,
    member_id: &str,
    record: &store::Record,
) -> Result<()> {
    let remote: Remote = record.into();

    // 1. Revoke first. An empty `session_id` means no session was ever
    //    minted for this server (it was only ever added, never connected
    //    to) — nothing to revoke, so this is not skipped as a failure.
    if revocable(&record.session_id) {
        revoke_session(revokes, ui, &record.session_id).await?;
    } else if !record.session_id.is_empty() {
        // Not silently skipped: something is recorded there, so a session may
        // well be live — it just is not an id this can safely put in a URL.
        ui.warn(&format!(
            "The session id saved for {} is not one riabuild recognises, so it is not being \
             revoked. If that server has a live riabuild session, end it from the dashboard's \
             session list.",
            record.display_name()
        ));
    }

    // 2. Best-effort cleanup on the server itself.
    cleanup_server_side(
        &remote,
        paths,
        runner.clone(),
        ui,
        carries,
        record,
        member_id,
    )
    .await;

    // 3. Local delete: the keychain items and the key pair.
    let account = keychain::for_account(
        runner.clone(),
        &keychain::remote_account(&remote.hash()),
        paths.remote_session_file(&remote.hash()),
    )
    .await;
    account.delete().await?;

    // The session and the password are two accounts for one server (see
    // `askpass::account`), so revoking the first leaves the second behind
    // unless it is named. A password for a server the developer has asked
    // riabuild to forget is the clearest case there is of a secret riabuild
    // should no longer be holding.
    super::askpass::forget(&remote, paths, runner).await?;

    // Both halves of the key pair, not just the private one. A `<hash>.pub`
    // left behind is this laptop still holding a file naming a server the
    // developer asked it to forget — and `authorise` reads it, so the next run
    // against a re-added server of the same address would offer a key whose
    // private half is gone.
    let key = identity::key_path(&remote, paths);
    remove_if_present(&key).await?;
    remove_if_present(&key.with_extension("pub")).await?;

    // And the directory the issued-key agent works in. It holds public halves
    // of the *org's* keys and, if a run ended badly, a socket that may still
    // have an `ssh-agent` behind it — neither of which belongs to a server
    // this laptop has been told to let go of.
    match tokio::fs::remove_dir_all(paths.agent_dir(&remote.hash())).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    // And the two markers this laptop keeps *about its own windows* into that
    // server: the lock that serialises minting its session, and the directory
    // counting the terminals that have it open. Neither is a secret and neither
    // does anything on its own, which is precisely why they would have been
    // left behind for ever — a server forgotten and re-added under a new
    // address is a new `Remote::hash` and would never reuse them.
    remove_if_present(&paths.remote_session_lock_file(&remote.hash())).await?;
    match tokio::fs::remove_dir_all(windows::dir(paths, &remote)).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    Ok(())
}

/// Deletes a file this laptop should no longer be holding, treating "it was
/// not there" as done.
///
/// Nothing to remove is success: the step that would have created it never
/// ran, or this is a second `forget` after a first one already got this far.
async fn remove_if_present(path: &std::path::Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Whether a saved session id is one riabuild will put in a URL path.
///
/// It is read straight out of `remotes.json` and formatted into
/// `/api/v1/cli/sessions/{id}`, which is the one call whose failure must stop
/// `forget` before anything local changes — so an id carrying a `/`, a `?` or a
/// `..` would be aiming that stop at a route nobody chose. Convex ids are
/// `[a-z0-9]` in practice; the set here is the wider one every id riabuild-web
/// has ever minted fits inside, so this refuses hand edits and encoding
/// accidents rather than second-guessing the format.
fn revocable(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// Lets go of the machine one of the team's servers used to name, after a lead
/// edited its address.
///
/// Best effort, and deliberately not fatal: the developer asked to connect to
/// the server the leads are pointing at now, and a machine that has been
/// decommissioned — which is the usual reason an address changes — must not
/// stop them. What is left behind if this fails is a key line on a box nobody
/// uses; what would be left behind if it were skipped is that plus a live
/// session, which is why it is attempted at all.
pub async fn retire_superseded(
    ctx: &riabuild_tasks::Ctx,
    member_id: &str,
    superseded: &store::Record,
) -> Result<()> {
    ctx.ui.note(&format!(
        "{} points at a different machine now. Letting go of {}@{} — the key and the session \
         riabuild left there.",
        superseded.display_name(),
        superseded.user,
        superseded.host,
    ));
    retire_identity(
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &ApiRevokes(&ctx.api),
        &IssuedCarries(&ctx.api),
        member_id,
        superseded,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::api::Carried;
    use super::revoke::api_error;
    use super::*;
    use crate::issued;
    use async_trait::async_trait;
    use riabuild_api::ApiError;
    use riabuild_runner::FakeRunner;

    const MEMBER_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const SESSION_ID: &str = "js7c9f0kq2m4n6p8r0t2v4x6";

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// The keychain call the local-delete step makes, whichever platform the
    /// test process happens to be on: `security` on macOS, `secret-tool` on
    /// Linux. Both are registered on every `FakeRunner` below, so the step is
    /// recorded either way — a test that asserted "no calls at all" would be
    /// green on CI's ubuntu runners and red on every developer's Mac, where
    /// riabuild actually ships.
    fn is_keychain_delete(call: &str) -> bool {
        call.starts_with("security delete-generic-password")
            || call.starts_with("secret-tool clear")
    }
    /// A runner with the keychain CLI of *both* platforms registered, plus
    /// whatever `ssh` behaviour the test needs.
    fn runner_with_ssh(code: i32, stderr: &str) -> Arc<FakeRunner> {
        Arc::new(
            FakeRunner::new()
                .with("ssh", code, "", stderr)
                .with("security", 0, "", "")
                .with("secret-tool", 0, "", ""),
        )
    }
    /// A `Revokes` that answers however the test says, and records the
    /// attempt into the same ordered list the runner writes to — which is
    /// what makes "revoke, then SSH, then local delete" assertable as one
    /// sequence rather than three separate facts that might still be in the
    /// wrong order relative to each other.
    struct ScriptedRevoke {
        calls: Arc<FakeRunner>,
        answer: Box<dyn Fn() -> Result<()> + Send + Sync>,
    }

    impl ScriptedRevoke {
        fn ok(calls: Arc<FakeRunner>) -> Self {
            Self {
                calls,
                answer: Box::new(|| Ok(())),
            }
        }

        fn failing(calls: Arc<FakeRunner>, error: ApiError) -> Self {
            Self {
                calls,
                answer: Box::new(move || Err(error.clone().into())),
            }
        }
    }

    #[async_trait]
    impl Revokes for ScriptedRevoke {
        async fn revoke(&self, session_id: &str) -> Result<()> {
            self.calls.calls.lock().expect("calls").push(format!(
                "riabuild-web DELETE /api/v1/cli/sessions/{session_id}"
            ));
            (self.answer)()
        }
    }

    /// The laptop with no issued key that gets in — which is every laptop
    /// against an ordinary server, and what every test but the gateway one
    /// below is about. Resolving one for real means a fetch from riabuild-web
    /// and an `ssh-agent`; this is why the seam exists.
    struct NoCarry;

    #[async_trait]
    impl Carries for NoCarry {
        async fn carry(
            &self,
            _remote: &Remote,
            _paths: &dyn Paths,
            _runner: Arc<dyn CommandRunner>,
            _ui: &Ui,
        ) -> Option<Carried> {
            None
        }
    }

    /// A managed gateway riabuild's own key can never sign in to, and the
    /// issued identity that can. `Issued::preset(None)` gives `Carried` an
    /// `Issued` with no agent behind it, so `stop` is a no-op and nothing is
    /// started.
    struct GatewayCarry;

    const CARRIED_SOCKET: &str = "/tmp/riabuild-test-agent/agent.sock";

    #[async_trait]
    impl Carries for GatewayCarry {
        async fn carry(
            &self,
            _remote: &Remote,
            _paths: &dyn Paths,
            _runner: Arc<dyn CommandRunner>,
            _ui: &Ui,
        ) -> Option<Carried> {
            Some(Carried {
                working: issued::Working {
                    label: "bastion".into(),
                    socket: CARRIED_SOCKET.into(),
                    public_key_path: "/tmp/riabuild-test-agent/bastion.pub".into(),
                },
                issued: issued::Issued::preset(None),
            })
        }
    }

    /// A store holding one record for `remote()`, with the server's home
    /// already resolved so the cleanup step has something to clean.
    fn store_with(session_id: &str) -> store::Store {
        let mut store = store::Store::default();
        let mut record = store::record_for(&remote());
        record.home = "/home/dev".to_string();
        record.session_id = session_id.to_string();
        store.remotes.push(record);
        store
    }
    /// One of the team's servers, as a run that connected to it left it.
    fn shared_store(fresh: bool) -> store::Store {
        let mut store = store::Store::default();
        let mut record = store::shared_record_for(&remote(), "k17abc");
        record.name = "build-01".into();
        record.home = "/home/dev".to_string();
        record.session_id = SESSION_ID.to_string();
        record.fresh = fresh;
        store.remotes.push(record);
        store
    }
    async fn key_on_disk(paths: &dyn Paths) {
        tokio::fs::create_dir_all(paths.identity_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(paths.identity_dir().join(remote().hash()), "KEY")
            .await
            .expect("key");
    }

    /// `record_for` leaves `session_id` empty, the same as a server that was
    /// only ever added, never connected to — nothing was ever minted, so the
    /// API revoke step is skipped entirely. What this pins is everything
    /// after it: the server-side cleanup runs, and the key file and the store
    /// entry both go.
    #[tokio::test]
    async fn forgetting_a_server_removes_the_key_the_entry_and_the_ssh_line() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        let mut store = store_with("");
        let fake = runner_with_ssh(0, "");
        let api = ApiClient::new("0.1.0");

        forget_remote(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets");

        assert!(store.find("build-01").is_none());
        assert!(!paths.identity_dir().join(remote().hash()).exists());
        assert!(
            fake.calls().iter().any(|call| call.contains("rm -rf")),
            "the namespace on the server goes too: {:?}",
            fake.calls()
        );
    }
    #[tokio::test]
    async fn forgetting_an_unreachable_server_says_what_it_left_behind() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = store_with("");

        let fake = runner_with_ssh(255, "Connection refused");
        let api = ApiClient::new("0.1.0");

        forget_remote(
            &paths,
            fake,
            &Ui::new(true),
            &api,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("must still forget locally");

        // The local half always succeeds: a server you cannot reach must not
        // be a server you cannot remove.
        assert!(store.find("build-01").is_none());
    }
    /// The three steps, in the one order that is safe to interrupt, asserted
    /// as one sequence. A comment cannot hold this: every reordering of these
    /// three still compiles and still leaves the store entry gone at the end.
    #[tokio::test]
    async fn the_revoke_precedes_the_server_cleanup_which_precedes_the_local_delete() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets");

        let calls = fake.calls();
        let revoke = calls
            .iter()
            .position(|call| call.contains("DELETE /api/v1/cli/sessions/"))
            .expect("a saved session id must actually be revoked, not skipped");
        let cleanup = calls
            .iter()
            .position(|call| call.contains("rm -rf"))
            .expect("the server-side cleanup ran");
        let local = calls
            .iter()
            .position(|call| is_keychain_delete(call))
            .expect("the local keychain item was deleted");

        assert!(
            revoke < cleanup,
            "the token has to be dead before anything else is attempted: {calls:?}"
        );
        assert!(
            cleanup < local,
            "the cleanup needs the key the local delete removes: {calls:?}"
        );
        assert!(store.find("build-01").is_none());
        assert!(!paths.identity_dir().join(remote().hash()).exists());
    }
    /// A server the developer has asked riabuild to forget is the clearest
    /// case there is of a secret riabuild should no longer be holding — and a
    /// saved SSH password is a *second* keychain account for the same server,
    /// so revoking the session leaves it behind unless it is named too.
    ///
    /// Asserted on both accounts appearing in the deletes rather than on a
    /// count, so the test says which secret survived when it fails.
    #[tokio::test]
    async fn forgetting_a_server_forgets_its_password_as_well_as_its_session() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets");

        let deletes: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|call| is_keychain_delete(call))
            .collect();
        let password = crate::askpass::account(&remote());
        let session = riabuild_keychain::remote_account(&remote().hash());
        assert!(
            deletes.iter().any(|call| call.contains(&password)),
            "the saved SSH password outlived the server it belongs to: {deletes:?}"
        );
        assert!(
            deletes.iter().any(|call| call.contains(&session)),
            "and the session must still go too: {deletes:?}"
        );
    }
    /// The step whose failure must stop everything. Until this test existed,
    /// no test reached the revoke at all, so "stop loudly before touching
    /// anything local" was a doc comment and nothing else.
    #[tokio::test]
    async fn a_failed_revoke_stops_before_anything_local_is_touched() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::failing(fake.clone(), api_error("unreachable", 0));

        let error = forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect_err("a token whose fate is unknown must not be forgotten quietly");
        assert!(
            error.to_string().contains("still live"),
            "the developer has to be told the token may still be live: {error}"
        );

        // Everything local survives, so a retry can still find the server —
        // and the key it needs to reach it.
        assert!(store.find("build-01").is_some());
        assert!(paths.identity_dir().join(remote().hash()).exists());
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.contains("rm -rf") || is_keychain_delete(call)),
            "nothing after step 1 may run: {:?}",
            fake.calls()
        );
    }
    /// `session_unknown` is the one failure that reads as success — and the
    /// one that riabuild-web deliberately makes ambiguous, because it answers
    /// the same way for a session belonging to somebody else. `forget` still
    /// completes (a retry must not be stuck forever), but it must not do so
    /// silently.
    // Named for what it asserts, not for what the code also does. The warning
    // `revoke_session` emits here is the substance of the change, but `Ui` has
    // no capture seam, so nothing below can check it — and a test name is not
    // an assertion. Reword or delete that warning and this test stays green;
    // giving `Ui` a test sink is what would close it.
    #[tokio::test]
    async fn an_unrecognised_session_is_forgotten_rather_than_left_behind() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::failing(fake.clone(), api_error("session_unknown", 404));

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("already gone must not block a retry");

        assert!(store.find("build-01").is_none());
        assert!(
            fake.calls().iter().any(|call| call.contains("rm -rf")),
            "the cleanup still runs: {:?}",
            fake.calls()
        );
    }
    #[tokio::test]
    async fn a_server_that_never_resolved_a_home_has_nothing_on_it_to_clean_up() {
        // No `record.home` means `resolve_home` never succeeded — the server
        // was added, maybe attempted, but riabuild never got far enough to
        // install anything there. `cleanup_server_side` must not construct a
        // namespace out of an empty home and must not touch `ssh` at all. The
        // local delete below it still runs — and on macOS that is a real
        // `security` invocation — so this asserts the absence of `ssh`, not
        // the absence of every call.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = store::Store::default();
        store.remotes.push(store::record_for(&remote()));

        let fake = runner_with_ssh(0, "");
        let api = ApiClient::new("0.1.0");

        forget_remote(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets");

        assert!(
            !fake.calls().iter().any(|call| call.starts_with("ssh")),
            "nothing was ever installed on this server: {:?}",
            fake.calls()
        );
        assert!(store.find("build-01").is_none());
    }
    #[tokio::test]
    async fn forgetting_a_server_that_was_never_saved_is_an_error_not_a_silent_no_op() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = store::Store::default();
        let api = ApiClient::new("0.1.0");

        let error = forget_remote(
            &paths,
            runner_with_ssh(0, ""),
            &Ui::new(true),
            &api,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect_err("nothing named build-01 was ever saved");
        assert!(error.to_string().contains("build-01"), "{error}");
    }
    #[tokio::test]
    async fn forgetting_one_of_the_teams_servers_clears_this_laptop_and_nothing_else() {
        // The honest reading of "the CLI cannot remove a shared server": it
        // cannot take the machine away from the team, and it can always let go
        // of this laptop's own key, password and session for it. The row in
        // riabuild-web is untouched, so the server is back in the picker on the
        // next run — with nothing of this laptop's left on it.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;
        let mut store = shared_store(true);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "shared-build-01",
        )
        .await
        .expect("forgets");

        assert!(store.find("shared-build-01").is_none());
        assert!(!paths.identity_dir().join(remote().hash()).exists());
        assert!(
            fake.calls().iter().any(|call| call.contains("rm -rf")),
            "{:?}",
            fake.calls()
        );
        // The one call this must *not* make: there is no endpoint here that
        // removes a shared server, and there must never be one.
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.contains("remotes/shared")),
            "{:?}",
            fake.calls()
        );
    }
    #[tokio::test]
    async fn a_server_the_leads_removed_can_still_be_forgotten_by_name() {
        // The case that keeps a removed server's session revocable. Its record
        // is Stale — riabuild will not connect to the address in it — but the
        // session recorded beside it may still be live, and this is the only
        // command that can clear it.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = shared_store(false);
        assert_eq!(
            store
                .find("shared-build-01")
                .expect("still findable")
                .origin(),
            store::Origin::Stale
        );
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "shared-build-01",
        )
        .await
        .expect("forgets");

        assert!(store.remotes.is_empty());
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("/api/v1/cli/sessions/")),
            "the session has to be revoked, which is the whole point: {:?}",
            fake.calls()
        );
    }
    #[tokio::test]
    async fn retiring_an_edited_address_revokes_the_session_of_the_machine_being_left() {
        // A lead edited the address, so `shared::reconcile` handed back the old
        // copy. Everything here is aimed at the *old* machine: its session, its
        // namespace, its key — which is why the old address is kept on the
        // record rather than only its hash.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;
        let store = shared_store(true);
        let old = store.remotes[0].clone();
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());

        retire_identity(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &old,
        )
        .await
        .expect("retires");

        assert!(
            fake.calls().iter().any(|call| call.ends_with(SESSION_ID)),
            "{:?}",
            fake.calls()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("build-01.fly.dev")),
            "the cleanup has to be aimed at the address being left: {:?}",
            fake.calls()
        );
        assert!(
            !paths.identity_dir().join(remote().hash()).exists(),
            "the key riabuild put on the old machine is no longer this laptop's"
        );
    }
    /// I047. On a managed SSH gateway riabuild's own key can never sign in —
    /// the box accepts the write to `authorized_keys` and then authenticates
    /// against its own registry regardless, which is the whole reason issued
    /// keys exist. `cleanup_server_side` hardcoded `carry: None`, so on exactly
    /// those servers `forget` could never authenticate: it always warned and
    /// always left the namespace and the key line behind.
    #[tokio::test]
    async fn a_gateway_that_refuses_riabuilds_own_key_is_cleaned_up_with_the_issued_one() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;
        let mut store = store_with(SESSION_ID);
        // Every plain `ssh` is refused; the one carrying the agent socket is
        // not. `FakeRunner` matches the longest registered prefix, so naming
        // the socket is what tells the two apart.
        let fake = Arc::new(
            FakeRunner::new()
                .with("ssh", 255, "", "Permission denied (publickey).")
                .containing(&format!("IdentityAgent={CARRIED_SOCKET}"), 0, "", "")
                .with("security", 0, "", "")
                .with("secret-tool", 0, "", ""),
        );
        let revokes = ScriptedRevoke::ok(fake.clone());
        let ui = Ui::new(false);

        forget_with(
            &paths,
            fake.clone(),
            &ui,
            &revokes,
            &GatewayCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets");

        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains(CARRIED_SOCKET) && call.contains("rm -rf")),
            "the cleanup has to be retried through the identity that can actually sign in: {:?}",
            fake.calls()
        );
        assert!(
            ui.warned().is_empty(),
            "a server that was cleaned up must not be reported as unreachable: {:?}",
            ui.warned()
        );
    }
    #[tokio::test]
    async fn a_server_that_is_simply_off_does_not_pay_for_an_issued_key_hunt() {
        // The over-correction to guard against: resolving an issued identity
        // costs a fetch from riabuild-web, an `ssh-agent` and a probe per key.
        // A box that never answered would pay all of it to be told again that
        // it never answered.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = store_with(SESSION_ID);
        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh",
                    255,
                    "",
                    "ssh: connect to host build-01.fly.dev port 22: Connection refused",
                )
                .with("security", 0, "", "")
                .with("secret-tool", 0, "", ""),
        );
        let revokes = ScriptedRevoke::ok(fake.clone());
        let ui = Ui::new(false);

        forget_with(
            &paths,
            fake.clone(),
            &ui,
            &revokes,
            &GatewayCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets locally regardless");

        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.contains(CARRIED_SOCKET)),
            "nothing refused us, so there is nothing an issued key would fix: {:?}",
            fake.calls()
        );
        assert!(
            ui.warned()
                .iter()
                .any(|warning| warning.contains("Could not reach")),
            "{:?}",
            ui.warned()
        );
    }
    /// I048. The private key was the only thing removed. Both are this
    /// laptop's own traces of a server it has been told to let go of — and
    /// `authorise` reads the `.pub`, so leaving it means the next run against a
    /// re-added server of the same address offers a key whose private half is
    /// gone.
    #[tokio::test]
    async fn forgetting_a_server_removes_the_public_half_and_the_agent_directory_too() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;
        let public = paths
            .identity_dir()
            .join(remote().hash())
            .with_extension("pub");
        tokio::fs::write(&public, "ssh-ed25519 AAAA riabuild")
            .await
            .expect("pub");
        let agent = paths.agent_dir(&remote().hash());
        tokio::fs::create_dir_all(&agent).await.expect("mkdir");
        tokio::fs::write(agent.join("org.pub"), "ssh-ed25519 AAAA org")
            .await
            .expect("org key");

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets");

        assert!(!paths.identity_dir().join(remote().hash()).exists());
        assert!(!public.exists(), "the public half is a trace too");
        assert!(
            !agent.exists(),
            "the agent directory holds public halves of the org's keys and a socket that \
             may still have an ssh-agent behind it"
        );
    }
    /// A forget that will break the developer's other terminal says so first.
    ///
    /// It used to say nothing. Revoking the server's session, taking riabuild's
    /// key back out of its `authorized_keys` and clearing the namespace are
    /// three things that stop a shell somebody is sitting in, and the only
    /// framing riabuild had for a second session on one server was "a
    /// colleague" — which the developer's own second window is not. Now it is
    /// counted and named.
    ///
    /// A warning and not a refusal, and the test says so twice over: the
    /// `expect` below is the assertion that it still forgets. `forget` is a
    /// destructive command typed by name, usually because something about that
    /// server has gone wrong, and one that downed tools while a window was open
    /// could not clean up the case it is most needed for.
    #[tokio::test]
    async fn forgetting_a_server_another_window_is_using_warns_before_it_starts() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        // The other terminal: a window marker whose lock is held, which is what
        // an open `riabuild remote` leaves behind for the length of its shell.
        let windows = crate::windows::dir(&paths, &remote());
        tokio::fs::create_dir_all(&windows).await.expect("mkdir");
        let _other = riabuild_paths::filelock::FileLock::try_acquire(&windows.join("4242.lock"))
            .await
            .expect("lock")
            .expect("free");

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());
        let ui = Ui::new(false);

        forget_with(
            &paths,
            fake.clone(),
            &ui,
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("a warning is not a refusal");

        let warning = ui
            .warned()
            .into_iter()
            .find(|warning| warning.contains("still connected"))
            .unwrap_or_else(|| panic!("nothing warned about the open window: {:?}", ui.warned()));
        // Named, not merely counted: "another window is open" is not something
        // a developer can act on, and "the shell in it will stop working" is.
        assert!(warning.contains("stop working"), "{warning}");
        assert!(warning.contains("build-01"), "{warning}");
    }

    /// …and a server nobody has open is forgotten without a word about windows.
    ///
    /// The half that keeps the warning worth reading. A sentence printed on
    /// every `forget` is one nobody reads by the third time, and the count is
    /// swept by the very call that reads it — so a window that ended yesterday
    /// must not still be warned about today.
    #[tokio::test]
    async fn forgetting_a_server_nobody_has_open_says_nothing_about_windows() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        // A window that has ended: its marker is still on disk, and nothing
        // holds the lock.
        let windows = crate::windows::dir(&paths, &remote());
        tokio::fs::create_dir_all(&windows).await.expect("mkdir");
        drop(
            riabuild_paths::filelock::FileLock::try_acquire(&windows.join("4242.lock"))
                .await
                .expect("lock")
                .expect("free"),
        );

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());
        let ui = Ui::new(false);

        forget_with(
            &paths,
            fake.clone(),
            &ui,
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("forgets");

        assert!(
            !ui.warned()
                .iter()
                .any(|warning| warning.contains("still connected")),
            "a window that has ended is not a window: {:?}",
            ui.warned()
        );
        assert!(
            !windows.exists(),
            "and the markers go with the server they name"
        );
    }

    /// I010. The id is read out of `remotes.json` and formatted straight into
    /// `/api/v1/cli/sessions/{id}`. This is the one call whose failure must
    /// stop `forget` before anything local changes, so an id carrying a `/` or
    /// a `..` would be aiming that stop at a route nobody chose.
    #[tokio::test]
    async fn a_session_id_that_is_not_an_id_is_not_put_in_a_url() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = store_with("../../org/members?x=");
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());
        let ui = Ui::new(false);

        forget_with(
            &paths,
            fake.clone(),
            &ui,
            &revokes,
            &NoCarry,
            MEMBER_ID,
            &mut store,
            "build-01",
        )
        .await
        .expect("the rest of forget still runs");

        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.contains("/api/v1/cli/sessions/")),
            "{:?}",
            fake.calls()
        );
        assert!(
            ui.warned()
                .iter()
                .any(|warning| warning.contains("not being")),
            "a session that may be live must not be dropped in silence: {:?}",
            ui.warned()
        );
        assert!(store.find("build-01").is_none());
    }
    #[test]
    fn what_counts_as_a_revocable_session_id() {
        assert!(revocable(SESSION_ID));
        assert!(revocable("a-b_C9"));
        assert!(!revocable(""));
        assert!(!revocable("../sessions"));
        assert!(!revocable("id with space"));
        assert!(!revocable("id/../.."));
        assert!(!revocable("id?query=1"));
    }
}

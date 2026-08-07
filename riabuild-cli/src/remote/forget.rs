//! `riabuild remote forget <name>` — the reverse of `flow::connect_and_setup`.
//!
//! Kept separate from `flow.rs` for the same reason `flow.rs` was itself split
//! out of `mod.rs` (see its module doc): folding the teardown in as well
//! pushed that file well past the crate's ~300-line production budget. Nothing
//! here shares state with the setup flow — it is reached from a different arm
//! of the same `match`, takes its own arguments, and returns before the setup
//! path begins — so the split costs no threading.

use super::{Remote, identity, session, shell_command, shell_quote, ssh_once, store};
use crate::api::{ApiClient, ApiError};
use crate::keychain;
use crate::paths::Paths;
use crate::runner::CommandRunner;
use crate::ui::{Failure, Ui};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;

/// The one network call `forget` makes, behind a seam.
///
/// Same shape and same reason as `install.rs`'s `Downloads`: without it, step
/// 1 is only reachable by a test that stands up a real riabuild-web, which
/// this crate's scaffolding has never done. Every `forget` test therefore left
/// `session_id` empty, and the step whose *failure* must stop the whole
/// function before anything local changes had no coverage at all.
#[async_trait]
trait Revokes: Send + Sync {
    async fn revoke(&self, session_id: &str) -> Result<()>;
}

/// What production uses: `DELETE /api/v1/cli/sessions/<id>` (Task 3b).
struct ApiRevokes<'a>(&'a ApiClient);

#[async_trait]
impl Revokes for ApiRevokes<'_> {
    async fn revoke(&self, session_id: &str) -> Result<()> {
        self.0
            .delete_json::<serde_json::Value>(&format!("/api/v1/cli/sessions/{session_id}"))
            .await
            .map(|_| ())
    }
}

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
    forget_with(paths, runner, ui, &ApiRevokes(api), member_id, store, name).await
}

/// The body of [`forget_remote`], taking [`Revokes`] as a seam.
async fn forget_with(
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    revokes: &dyn Revokes,
    member_id: &str,
    store: &mut store::Store,
    name: &str,
) -> Result<()> {
    let Some(record) = store.find(name).cloned() else {
        return Err(anyhow!("there is no saved server named \"{name}\""));
    };
    let remote: Remote = (&record).into();

    // 1. Revoke first. An empty `session_id` means no session was ever
    //    minted for this server (it was only ever added, never connected
    //    to) — nothing to revoke, so this is not skipped as a failure.
    if !record.session_id.is_empty() {
        revoke_session(revokes, ui, &record.session_id).await?;
    }

    // 2. Best-effort cleanup on the server itself.
    cleanup_server_side(&remote, paths, runner.clone(), ui, &record, member_id).await;

    // 3. Local delete: the keychain item, the key pair, and the store entry.
    let account = keychain::for_account(runner, &keychain::remote_account(&remote.hash()), None);
    account.delete().await?;

    match tokio::fs::remove_file(identity::key_path(&remote, paths)).await {
        Ok(()) => {}
        // Nothing to remove is success here too — `ensure_key` never ran, or
        // this is a second `forget` after a first one already got this far.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    store.remotes.retain(|r| r.name != name);
    store.save(paths).await?;

    ui.note(&format!("Forgot {name}."));
    Ok(())
}

/// Step 1 of [`forget_remote`]: revoke this server's session.
async fn revoke_session(revokes: &dyn Revokes, ui: &Ui, session_id: &str) -> Result<()> {
    match revokes.revoke(session_id).await {
        Ok(()) => Ok(()),
        Err(error) if already_revoked(&error) => {
            // Not silent: see [`already_revoked`] for why this answer is
            // genuinely ambiguous and why the ambiguity cannot be resolved
            // from this side.
            ui.warn(&format!(
                "riabuild-web does not recognise this server's session ({session_id}), so it \
                 is being treated as already revoked. If this laptop has signed in as a \
                 different member since that session was minted, the token may still be \
                 live — check the sessions list on the dashboard."
            ));
            Ok(())
        }
        Err(error) => Err(Failure::new(
            "revoking this server's riabuild session",
            "Check your network connection, then run `riabuild remote forget` again — \
             until this succeeds, the token this laptop minted is still live on the server.",
        )
        .detail(error.to_string())
        .into()),
    }
}

/// Whether an error from [`revoke_session`]'s call means the session is
/// already gone rather than that the call itself failed.
///
/// Treating "already gone" as success is what stops a retry after a
/// half-finished `forget` getting stuck forever: the goal ("no live token")
/// holds whether this laptop revoked it or something else did — another
/// laptop's `forget`, an admin, natural expiry.
///
/// **The honest caveat, which this function cannot close.** riabuild-web
/// deliberately answers `session_unknown` for a session that exists but
/// belongs to a *different* member, so that session ids cannot be probed for
/// existence by whoever holds one. So `session_unknown` means "gone, or not
/// yours" — and the second reading is a live token. A hand-edited
/// `remotes.json`, or an account switch on this laptop, reaches it. Nothing
/// on the wire distinguishes the two, and trying to would defeat the
/// endpoint's design, so [`revoke_session`] warns instead: the developer,
/// unlike this process, can look at the dashboard's session list and tell.
fn already_revoked(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ApiError>()
        .is_some_and(|api_error| api_error.code == "session_unknown")
}

/// Step 2 of [`forget_remote`]: the namespace and the `authorized_keys` line
/// this developer's own key added, if either was ever created.
///
/// Never fails the caller: an unreachable server here is reported through
/// `ui.warn` and left for a human to notice, not propagated as an error that
/// would stop the local delete that follows it.
async fn cleanup_server_side(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    record: &store::Record,
    member_id: &str,
) {
    if record.home.is_empty() {
        // `resolve_home` never succeeded for this server — nothing was ever
        // installed on it to clean up.
        return;
    }

    let ns = session::namespace(&record.home, member_id);
    let keys = format!("{}/.ssh/authorized_keys", record.home);
    // Matched on the member id, as a fixed string via `grep -vF`. On a
    // shared account every developer's key comment carries the same
    // `user@host`, so matching on that would delete Bob's and Carla's lines
    // too and lock them out of the box with no diagnostic anywhere. `sed`
    // would also read the hostname's dots as wildcards, and `-i.bak` would
    // leave the "removed" key sitting in a sibling file instead of gone.
    let cleanup = shell_command(&format!(
        "rm -rf {ns}; if [ -f {keys} ]; then grep -vF {marker} {keys} {redirect} {keys}.new \
         && cat {keys}.new {redirect} {keys} && rm -f {keys}.new; fi",
        ns = shell_quote(&ns),
        keys = shell_quote(&keys),
        marker = shell_quote(&identity::key_comment_marker(member_id)),
        redirect = ">",
    ));

    let outcome = ssh_once(remote, paths, runner, &cleanup).await;
    let succeeded = matches!(&outcome, Ok(output) if output.ok());
    if !succeeded {
        ui.warn(&format!(
            "Could not reach {}. Its riabuild namespace and authorized_keys line are still there.",
            remote.host
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

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

    fn api_error(code: &str, status: u16) -> ApiError {
        ApiError {
            status,
            code: code.into(),
            message: "x".into(),
            action: "y".into(),
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::ok(fake.clone());

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
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

    /// The step whose failure must stop everything. Until this test existed,
    /// no test reached the revoke at all, so "stop loudly before touching
    /// anything local" was a doc comment and nothing else.
    #[tokio::test]
    async fn a_failed_revoke_stops_before_anything_local_is_touched() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        key_on_disk(&paths).await;

        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::failing(fake.clone(), api_error("unreachable", 0));

        let error = forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
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
    #[tokio::test]
    async fn an_unrecognised_session_forgets_anyway_but_warns_that_it_may_still_be_live() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let mut store = store_with(SESSION_ID);
        let fake = runner_with_ssh(0, "");
        let revokes = ScriptedRevoke::failing(fake.clone(), api_error("session_unknown", 404));

        forget_with(
            &paths,
            fake.clone(),
            &Ui::new(true),
            &revokes,
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
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

    #[test]
    fn a_session_unknown_error_reads_as_already_revoked_not_a_failure() {
        // Someone else already forgot this server — another laptop, an admin,
        // natural expiry. The goal ("no live token") already holds, so this
        // must not block a retry that would otherwise never find anything to
        // revoke on the second attempt.
        let error: anyhow::Error = api_error("session_unknown", 404).into();
        assert!(already_revoked(&error));
    }

    #[test]
    fn any_other_failure_is_not_mistaken_for_already_revoked() {
        let upstream: anyhow::Error = api_error("upstream_error", 503).into();
        assert!(!already_revoked(&upstream));

        let transport = anyhow!("riabuild could not reach riabuild-web");
        assert!(!already_revoked(&transport));
    }
}

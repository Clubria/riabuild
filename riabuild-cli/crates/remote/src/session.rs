//! The riabuild session a server runs on.
//!
//! Minted by the laptop, labelled after the server so the dashboard lists it as
//! its own revocable device, and written to the server's namespace at 0600 —
//! the one amendment to "no secrets in ~/.riabuild", argued in the design.//!
//! [`namespace`] is where a developer's things live on the server and how a
//! file gets there; this file is whether the session already on it is still
//! worth reusing, and minting a new one when it is not.

use super::Remote;
use anyhow::Result;
use riabuild_api::{ApiClient, Member, auth};
use riabuild_keychain as keychain;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::{Failure, Ui};
use std::sync::Arc;
mod namespace;

use namespace::{basename, remote_layout, write_into_namespace};
pub use namespace::{gitconfig, namespace, owner_json};

/// Whether the store's record of this session's expiry is recent enough that
/// probing the server for liveness is worth the round trip at all.
///
/// Split out from `ensure` so this decision — the only part of "is a cached
/// token worth reusing" that does not need a real riabuild-web to answer — is
/// unit-testable. The round trip itself (`ApiClient::me`) is not: this crate's
/// test scaffolding (`testing.rs`) has never stood up a fake riabuild-web, the
/// same reason `tasks::login`'s own `apply()` — which drives the identical
/// browser-login flow this calls into — has no test of its own beyond
/// `check()`.
fn expires_soon(record: &super::store::Record) -> bool {
    record.session_expires_at <= riabuild_paths::config::now_millis()
}

/// What a probe of the cached token actually established, and therefore
/// whether the token may still be used.
///
/// Split out because the three answers are what the bug conflated. `me()`
/// returning `Err` used to mean "mint a new one", and it has two very
/// different causes:
///
/// - **The token is dead.** riabuild-web says so — `unauthenticated`,
///   `session_expired`, `session_revoked`, which is `ApiError::needs_login`.
///   Another laptop's `forget` revoked it, or it ran out. Minting is right.
/// - **riabuild-web could not answer.** A timeout, a 503, a 409, a captive
///   portal. Nothing was established about the token at all — and minting on
///   that answer overwrites `session_id` with a new row, orphaning the
///   previous session: still live on the server, and no longer nameable by
///   `remote forget`. On a blip that resolves in the seconds between the probe
///   and the mint, that is exactly what happened.
///
/// So an ambiguous answer keeps the token. The record has already been checked
/// against its expiry by [`expires_soon`], so what is being reused is a
/// credential this laptop has every local reason to believe in — and the
/// alternative is not a better token, it is two.
///
/// Takes the probe's `Result` rather than making the call, because the call is
/// the one thing this crate's scaffolding cannot stand up (see [`expires_soon`]),
/// and a decision nothing can test is how this shipped.
fn usable_token(token: String, probe: Result<Member>, ui: &Ui) -> Option<String> {
    match probe {
        Ok(_) => Some(token),
        Err(error) if dead_session(&error) => None,
        Err(error) => {
            ui.note(&format!(
                "Could not check this server's saved session with riabuild-web ({error}), so \
                 riabuild is reusing it rather than minting a second one that would leave the \
                 first live and unrevocable."
            ));
            Some(token)
        }
    }
}

/// Whether riabuild-web said the token is no longer good, as opposed to not
/// answering. Only an `ApiError` can say so; a transport failure says nothing.
fn dead_session(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<riabuild_api::ApiError>()
        .is_some_and(riabuild_api::ApiError::needs_login)
}

/// Records what minting a session produced, on the record it belongs to.
///
/// A miss is an error rather than the silent nothing it used to be. By the time
/// this is reached the token has been minted *and* is about to be written onto
/// the server, so a write-back that quietly does nothing produces the one state
/// `session_id` exists to prevent: a live 90-day session that no `riabuild
/// remote forget` can name.
///
/// `find_mut`, not a match on the bare `name`. `remote.name` is the *display*
/// name, so for one of the team's servers (`shared-gpu`) it never equalled the
/// record's own bare `gpu` — which is how every run re-minted, wrote another
/// token onto the server, and recorded none of them.
fn remember_session(
    store: &mut super::store::Store,
    name: &str,
    session_id: String,
    expires_at: u64,
) -> Result<()> {
    let Some(saved) = store.find_mut(name) else {
        return Err(Failure::new(
            format!("recording the riabuild session for {name}"),
            "Run `riabuild remote list` and check that server is still saved, then revoke the \
             session riabuild just minted from the dashboard's session list.",
        )
        .detail(format!(
            "a session ({session_id}) was minted for \"{name}\" but there is no record of that \
             server in remotes.json to record it on, so `riabuild remote forget` would never \
             be able to revoke it"
        ))
        .into());
    };
    saved.session_expires_at = expires_at;
    saved.session_id = session_id;
    Ok(())
}

/// Mints (or reuses) the session a server's own riabuild runs as, and writes
/// it — with a git identity and an owner label — into that developer's
/// namespace on `remote`.
///
/// `store` is threaded through rather than owned: `resolve_home` (below) reads
/// and writes it to cache the server's home directory, and minting a fresh
/// session records its expiry there too, under the same entry, so a second
/// `ensure` for the same server finds both without asking again.
///
/// Called from `remote::flow::connect_and_setup`, which is `riabuild remote`'s
/// real orchestration and the only production caller.
#[allow(clippy::too_many_arguments)]
pub async fn ensure(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    api: &ApiClient,
    member: &Member,
    // Only for the `ApiClient` that probes a cached token below. There is no
    // `web_url` beside it: nothing on this path builds a dashboard link any
    // more, because nothing on this path sends the developer to one.
    version: &str,
    store: &mut super::store::Store,
    carry: Option<&crate::issued::Working>,
) -> Result<()> {
    let home = super::resolve_home(remote, paths, runner.clone(), store, carry).await?;

    // One window at a time, from here until the token is recorded.
    //
    // **What follows is a read and a write with a network round trip between
    // them, and one person with two terminals into one server is the ordinary
    // way remote mode is used.** Run concurrently against a server whose token
    // has expired, both windows find no usable one, both mint, and the second
    // `remember_session` overwrites the first's `session_id` — leaving a live
    // 90-day session on riabuild-web that no `riabuild remote forget` can name,
    // which is the one state `usable_token` above says out loud must never be
    // produced. The window that waits finds the other's token already in the
    // keychain and on the record, and mints nothing.
    //
    // Held rather than tried: the wait is a second at most, and the thing on
    // the other side of it is exactly the answer this window wants.
    let _minting = riabuild_paths::filelock::FileLock::acquire(
        &paths.remote_session_lock_file(&remote.hash()),
        || ui.info("Waiting for the riabuild already signing this server in…"),
    )
    .await?;

    // Re-read now the lock is held. A sibling window may have minted and
    // recorded a session while this one waited, and reusing it is the whole
    // point of having waited — the in-memory `store` was loaded before that
    // happened. Only the two session fields are taken: everything else on this
    // record is this run's own, including the home directory `resolve_home`
    // just cached on it.
    if let Some(fresh) = super::store::Store::load(paths).await.find(&remote.name) {
        let (session_id, session_expires_at) = (fresh.session_id.clone(), fresh.session_expires_at);
        if let Some(mine) = store.find_mut(&remote.name) {
            mine.session_id = session_id;
            mine.session_expires_at = session_expires_at;
        }
    }

    // The laptop's own cache of this server's session, kept under an account
    // named for the server so several servers never collide on one laptop
    // keychain entry, and revoking one server's session can never sign the
    // laptop itself out. Never `RIABUILD_TOKEN`: that is this machine's
    // override, and honouring it here would hand every server the same token.
    let keychain = keychain::for_account(
        runner.clone(),
        &keychain::remote_account(&remote.hash()),
        paths.remote_session_file(&remote.hash()),
    )
    .await;

    // A stored token is not automatically a live one. It expires, and
    // `forget` on another laptop may have revoked it. Writing a dead token to
    // the server strands whoever lands on it: the server's own riabuild 401s,
    // and while the device-code flow *can* now sign in over SSH, doing so from
    // the server would mint a session nothing on this laptop recorded — so no
    // `riabuild remote forget` could ever revoke it again.
    let record = store.find(&remote.name).cloned();
    let usable = match (keychain.get().await?, record.as_ref()) {
        (Some(token), Some(record)) if !expires_soon(record) => {
            let mut probe = ApiClient::new(version);
            probe.set_token(Some(token.clone()));
            // Not `.is_ok().then_some(token)`: see `usable_token` for why "the
            // token was rejected" and "riabuild-web did not answer" must not
            // be the same answer.
            usable_token(token, probe.me().await, ui)
        }
        _ => None,
    };

    let token = match usable {
        Some(token) => token,
        None => {
            ui.heading(&format!("Signing {} in to riabuild", remote.name));
            // No browser, and nothing for the developer to approve. This
            // laptop is already signed in, and `auth::for_server` asks
            // riabuild-web to mint a second session on the server's behalf
            // under that authority — so setting up a server costs the person
            // doing it nothing beyond the sign-in they already did.
            //
            // The server's hostname is the label, so the dashboard lists this
            // session as its own revocable device rather than a second copy of
            // the laptop, which is what `riabuild remote forget` relies on.
            let auth::ServerSession {
                token,
                session_id,
                expires_at,
            } = auth::for_server(api, &remote.host).await?;
            keychain.set(&token).await?;
            // Recorded so the check above can skip the round trip next time,
            // and so `riabuild remote list` can show it — which requires
            // actually saving the store, not just mutating it in memory.
            // `session_id` is what lets `riabuild remote forget` name this
            // exact session when it revokes it through
            // `DELETE /api/v1/cli/sessions/<id>` — see `forget::forget_remote`.
            //
            // The expiry is the server's own answer rather than a TTL computed
            // here, which is what removes the last copy of riabuild-web's 90
            // days from this file.
            remember_session(store, &remote.name, session_id, expires_at)?;
            super::store::persist_one(paths, store, &remote.name).await?;
            token
        }
    };

    let ns = namespace(&home, &member.member_id);
    let layout = remote_layout(&home, &member.member_id);

    let session_token_name = basename(&layout.session_token_file());
    write_into_namespace(
        remote,
        paths,
        &runner,
        &ns,
        &session_token_name,
        token.into_bytes(),
        carry,
    )
    .await?;

    // The git identity. Nothing else writes this file, and GIT_CONFIG_GLOBAL
    // pointing at a file that does not exist is what makes `git commit` fail.
    let identity = gitconfig(&member.display_name(), &member.email);
    write_into_namespace(
        remote,
        paths,
        &runner,
        &ns,
        "gitconfig",
        identity.into_bytes(),
        carry,
    )
    .await?;

    let owner = owner_json(&member.github_login, &member.display_name(), &member.email);
    let owner_name = basename(&layout.owner_file());
    write_into_namespace(
        remote,
        paths,
        &runner,
        &ns,
        &owner_name,
        owner.into_bytes(),
        carry,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_fixture as remote;

    fn record_expiring_at(millis: u64) -> super::super::store::Record {
        let mut record = super::super::store::record_for(&remote());
        record.session_expires_at = millis;
        record
    }

    #[test]
    fn an_expired_record_is_not_worth_probing() {
        assert!(expires_soon(&record_expiring_at(0)));
        assert!(expires_soon(&record_expiring_at(1)));
    }
    #[test]
    fn a_record_expiring_well_in_the_future_is_worth_probing() {
        // Ninety days, the same order as the expiry riabuild-web hands back —
        // written out rather than shared with a constant, because this file no
        // longer owns a copy of that number and should not grow one back.
        let far_future = riabuild_paths::config::now_millis() + 90 * 24 * 60 * 60 * 1000;
        assert!(!expires_soon(&record_expiring_at(far_future)));
    }
    fn api_error(code: &str, status: u16) -> riabuild_api::ApiError {
        riabuild_api::ApiError {
            status,
            code: code.into(),
            message: "x".into(),
            action: "y".into(),
        }
    }

    /// I039. The three answers a probe can give, and the two that used to be
    /// one. Minting on an answer that established nothing overwrites
    /// `session_id` with a new row and leaves the old session live on the
    /// server with nothing left on this laptop able to name it.
    #[test]
    fn a_riabuild_web_that_could_not_answer_is_not_a_dead_session() {
        let ui = Ui::new(true);

        for (code, status) in [
            ("upstream_error", 503),
            ("rate_limited", 429),
            ("conflict", 409),
        ] {
            assert_eq!(
                usable_token("tok".into(), Err(api_error(code, status).into()), &ui),
                Some("tok".to_string()),
                "{code} says nothing about the token, so re-minting orphans the live one"
            );
        }

        // A transport failure — no `ApiError` at all — is the same case.
        assert_eq!(
            usable_token(
                "tok".into(),
                Err(anyhow::anyhow!("riabuild could not reach riabuild-web")),
                &ui
            ),
            Some("tok".to_string())
        );
    }
    #[test]
    fn a_token_riabuild_web_rejected_is_replaced() {
        // The other direction, and the reason the probe exists at all: a
        // session another laptop's `forget` revoked must not be written onto
        // the server, or whoever lands there gets a 401 from their own
        // riabuild.
        let ui = Ui::new(true);
        for code in ["unauthenticated", "session_expired", "session_revoked"] {
            assert_eq!(
                usable_token("tok".into(), Err(api_error(code, 401).into()), &ui),
                None,
                "{code}"
            );
        }
    }
    /// I034. `Remote::from(&Record)` carries the display name, so for one of
    /// the team's servers `remote.name` is `shared-build-01` while the record
    /// holds the bare `build-01`. The write-back matched on the bare field,
    /// found nothing, and said nothing — so every run minted another 90-day
    /// session, wrote another token onto the server, and recorded none of
    /// them for `forget` to revoke.
    #[test]
    fn one_of_the_teams_servers_records_the_session_it_just_minted() {
        let mut store = super::super::store::Store::default();
        store
            .remotes
            .push(super::super::store::shared_record_for(&remote(), "k17abc"));
        let shared: Remote = (&store.remotes[0]).into();
        assert_eq!(shared.name, "shared-build-01");

        remember_session(&mut store, &shared.name, "sess_live".into(), 99).expect("records it");

        assert_eq!(store.remotes[0].session_id, "sess_live");
        assert_eq!(store.remotes[0].session_expires_at, 99);
    }
    /// I040. The token has been minted and is about to be written onto the
    /// server by the time this runs, so "no record to put it on" is the one
    /// state that must never pass quietly: it is a live session no `riabuild
    /// remote forget` could ever name.
    #[test]
    fn a_minted_session_with_no_record_to_hold_it_is_an_error_not_a_shrug() {
        let mut store = super::super::store::Store::default();

        let error = remember_session(&mut store, "build-01", "sess_live".into(), 99)
            .expect_err("a session nothing can revoke is not a success");
        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(failure.detail.contains("sess_live"), "{}", failure.detail);
        assert!(failure.action.contains("dashboard"), "{}", failure.action);
    }
}

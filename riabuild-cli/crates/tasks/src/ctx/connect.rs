//! Establishing who this machine belongs to: the two riabuild-web requests
//! a run opens with, the order they are made in, and what happens to a
//! session the server no longer accepts.

use crate::Ctx;
use anyhow::Result;
use riabuild_api::ApiError;
use riabuild_api::Member;
use riabuild_api::org::OrgConfig;

impl Ctx {
    /// Asks riabuild-web who this machine belongs to, before any task runs.
    ///
    /// A missing or expired session is not an error here — the `login` task
    /// exists to fix exactly that. Anything else (suspended, removed from the
    /// org) is surfaced immediately, because no amount of provisioning will
    /// help.
    ///
    /// **`/org/config` is asked before `/me`, and that order is the whole of
    /// what keeps a raised version floor from locking every older riabuild out
    /// of its own upgrade.** `/me` enforces `minCliVersion`; `/org/config`
    /// deliberately does not, and that exemption exists so a CLI below the
    /// floor can learn it is below the floor. Asking `/me` first defeated it
    /// from this side: the 409 arrived before anything had read the floor,
    /// `connect` failed as a whole, and `main::keep_current` never reached
    /// `update::action_for` — so the one mechanism for forcing an upgrade
    /// stopped applying to exactly the builds that needed it. `self.org` is
    /// therefore left **set** when `/me` refuses, even though this still
    /// returns the error: the flows that need the API fail loudly as before,
    /// and the update check has the two version numbers it exists to compare.
    ///
    /// Idempotent within a run, and that is load-bearing rather than tidy:
    /// every command now connects once at startup so `update` can read the
    /// version floor, and the four flows that connect for themselves
    /// (`provision`, `remote`, `remote forget`, `login`) must keep doing so —
    /// none of them may depend on its caller having connected first. Without
    /// this guard each of them pays for a second `me` and a second
    /// `org/config` on every run.
    ///
    /// The **pair** is the thing to test for. `org` and `member` are set
    /// together — here and in `login::apply` — and always by a live request,
    /// so holding both means the question this method asks has already been
    /// answered. Holding only the first means `/me` refused this build, which
    /// is a question worth asking again rather than one to report as settled.
    /// A machine with no session holds neither and is asked again, which is
    /// exactly what the sign-in flow needs.
    pub async fn connect(&mut self) -> Result<()> {
        if self.org.is_some() && self.member.is_some() {
            return Ok(());
        }
        let Some(token) = self.keychain.get().await? else {
            return Ok(());
        };
        self.api.set_token(Some(token));

        // Both futures are built here and neither has run — an `async fn` is
        // lazy — so `connect_through` alone decides which is asked first.
        let api = self.api.clone();
        self.connect_through(riabuild_api::org::fetch_config(&api), api.me())
            .await
    }

    /// The two requests `connect` makes, in the order it makes them, with the
    /// requests themselves handed in.
    ///
    /// Split out so the ordering is something a test can watch rather than
    /// something a reader has to trust. Taking two futures — rather than an
    /// `ApiClient` pointed somewhere — is what lets a test record which was
    /// polled first without standing up an HTTP server to do it.
    async fn connect_through(
        &mut self,
        config: impl std::future::Future<Output = Result<OrgConfig>>,
        member: impl std::future::Future<Output = Result<Member>>,
    ) -> Result<()> {
        match config.await {
            Ok(config) => self.org = Some(config),
            Err(error) => return self.forget_session_or_report(error),
        }

        match member.await {
            Ok(member) => {
                self.member = Some(member);
                self.adopt_recorded_repo();
                Ok(())
            }
            // `self.org` stays set on purpose — see `connect` above. A 409 here
            // is what a CLI below the floor gets, and the floor it has to climb
            // past is in the answer already in hand.
            Err(error) => self.forget_session_or_report(error),
        }
    }

    /// A session riabuild-web no longer accepts is forgotten rather than
    /// reported: `login` exists to fix exactly that, and it must not find a
    /// half-loaded `Ctx` claiming the question has already been answered.
    /// Anything else is the server explaining something a developer must act
    /// on, and is passed straight through.
    fn forget_session_or_report(&mut self, error: anyhow::Error) -> Result<()> {
        match error.downcast_ref::<ApiError>() {
            Some(api_error) if api_error.needs_login() => {
                self.api.set_token(None);
                self.org = None;
                self.member = None;
                Ok(())
            }
            _ => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::{ctx_with, org_config};
    use riabuild_api::{ApiError, Member};
    use riabuild_keychain::MemoryKeychain;
    use riabuild_runner::FakeRunner;
    use std::sync::{Arc, Mutex};

    fn member() -> Member {
        Member {
            github_login: "ada".into(),
            member_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@clubria.com".into(),
            role: "developer".into(),
            status: "active".into(),
        }
    }

    /// The 409 `/me` answers a CLI below `minCliVersion` with.
    fn cli_too_old() -> anyhow::Error {
        ApiError {
            status: 409,
            code: "cli_too_old".into(),
            message: "This riabuild is 2026.07.30; the team requires 2026.08.04 or newer.".into(),
            action: "Run `brew upgrade clubria/tap/riabuild`.".into(),
        }
        .into()
    }

    #[tokio::test]
    async fn the_version_floor_is_learned_before_the_route_that_enforces_it() {
        // The lockout this ordering exists to prevent. `/me` enforces
        // `minCliVersion` and `/org/config` is exempt, so asking `/me` first
        // meant a raised floor 409'd before anything had read the floor:
        // `connect` failed as a whole and `main::keep_current` never reached
        // `update::action_for`, leaving the CLI unable to upgrade out of the
        // lockout. Swap the two awaits in `connect_through` and both of the
        // assertions below fail at once.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.org = None;
        let asked: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        let config_log = Arc::clone(&asked);
        let member_log = Arc::clone(&asked);
        let error = ctx
            .connect_through(
                async move {
                    config_log.lock().unwrap().push("/org/config");
                    Ok(org_config())
                },
                async move {
                    member_log.lock().unwrap().push("/me");
                    Err(cli_too_old())
                },
            )
            .await
            .expect_err("a below-floor CLI is still refused by /me");

        assert_eq!(
            *asked.lock().unwrap(),
            vec!["/org/config", "/me"],
            "the exempt route is the one that has to be asked first"
        );
        assert!(
            ctx.org.is_some(),
            "the floor riabuild has to climb past is in the answer already in hand"
        );
        assert!(
            ctx.member.is_none(),
            "and nothing may claim this session was fully established"
        );
        assert!(format!("{error}").contains("2026.08.04"), "{error}");
    }

    #[tokio::test]
    async fn a_session_the_server_no_longer_accepts_is_forgotten_rather_than_reported() {
        // `login` is the fix for an expired session, and it must not find a
        // half-loaded `Ctx` claiming the question was already answered.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.org = None;

        ctx.connect_through(async { Ok(org_config()) }, async {
            Err(ApiError {
                status: 401,
                code: "session_expired".into(),
                message: "x".into(),
                action: "y".into(),
            }
            .into())
        })
        .await
        .expect("an expired session is the login task's job, not an error here");

        assert!(ctx.org.is_none());
        assert!(ctx.member.is_none());
    }

    #[tokio::test]
    async fn a_second_connect_in_one_run_does_no_work() {
        // Every command now connects once at startup, and four of them
        // (`provision`, `remote`, `remote forget`, `login`) still call
        // `connect` themselves — as they must, since none of them may depend
        // on its caller having done it. Without this, each of those pays for a
        // second `me` and a second `org/config` on every run.
        //
        // "Did no work" is only observable as "never read the keychain", which
        // is the first thing `connect` would do.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(ctx.org.is_some(), "the test ctx starts already connected");
        // Both halves, because both are what `connect` now tests for: a `Ctx`
        // holding the team configuration and no member is one whose `/me` was
        // refused, and that is a question worth asking again.
        ctx.member = Some(member());
        ctx.keychain = Arc::new(MemoryKeychain::unreadable());

        ctx.connect()
            .await
            .expect("a connect with the team configuration already loaded");
    }
}

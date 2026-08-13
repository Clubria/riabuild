//! Where a riabuild session comes from. There are two ways, and this file is
//! both of them.
//!
//! **A human approves a device code.** `login` asks the server for a pair of
//! codes, prints the short one, and polls until a developer approves it in a
//! browser. Nothing here binds a socket and nothing travels through a
//! redirect, so a terminal on a server reached over SSH signs in exactly the
//! way a terminal on a laptop does.
//!
//! Two codes, two jobs. The `device_code` never leaves this process and is what
//! the poll is authenticated with; the `user_code` is read aloud off the
//! terminal and typed into the dashboard, and can be exchanged for nothing. The
//! developer matching one against the other is what stops a stranger's approval
//! link signing a stranger's machine in.
//!
//! **A signed-in laptop asks for one on a server's behalf.** `for_server` is
//! how `riabuild remote` gets the session it writes onto a server, and it
//! involves no browser at all — the bearer token on the request already proves
//! what a second device code would have asked the developer to prove again.
//! The server still cannot sign *itself* in; there is no path here that does
//! not start from a session a human approved.

use crate::{ApiClient, Member};
use anyhow::{Result, anyhow};
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::{Failure, Ui};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::{Instant, sleep};

/// Bounds on the poll interval the server asks for.
///
/// The server picks the cadence, but not without limits: a zero would spin this
/// loop against the API and a very large one would leave a developer staring at
/// an approved browser tab and a terminal that has not noticed.
const MIN_POLL: Duration = Duration::from_secs(1);
const MAX_POLL: Duration = Duration::from_secs(60);
const DEFAULT_POLL: Duration = Duration::from_secs(5);

/// A ceiling on how long to wait when the server does not say.
const FALLBACK_EXPIRY: Duration = Duration::from_secs(15 * 60);

/// What `POST /api/v1/cli/device` hands back.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceStart {
    #[serde(rename = "deviceCode")]
    pub device_code: String,
    #[serde(rename = "userCode")]
    pub user_code: String,
    #[serde(rename = "verificationUri")]
    pub verification_uri: String,
    #[serde(rename = "verificationUriComplete")]
    pub verification_uri_complete: Option<String>,
    /// Seconds, not a timestamp: a machine on its first boot may not have
    /// finished talking to NTP, and a duration does not care what time it is.
    #[serde(rename = "expiresIn")]
    pub expires_in: Option<u64>,
    pub interval: Option<u64>,
}

/// One tick of the poll loop.
///
/// Tagged by `status` so the wire contract is the type: "not yet" is an
/// ordinary 200 rather than an error to unwind, because it is the answer this
/// loop expects most of the time.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PollResponse {
    Pending {
        interval: Option<u64>,
    },
    Denied,
    Ok {
        token: String,
        member: Member,
        /// The `cliSessions` row this token belongs to. `remote::session::ensure`
        /// keeps it in `remotes.json` so `riabuild remote forget` knows exactly
        /// which session to revoke through `DELETE /api/v1/cli/sessions/<id>`
        /// rather than guessing from a device label.
        ///
        /// `#[serde(default)]` removes a deploy-order dependency, and costs
        /// nothing: without it, a CLI that ships before — or ahead of a rollback
        /// of — the riabuild-web that sends this field fails *login itself* on a
        /// decode error, which is a far worse outcome than not knowing a session
        /// id. `store::Record::session_id` already carries the same attribute and
        /// already treats empty as "nothing to revoke", so an empty string flows
        /// through the rest of remote mode as a state it is written to handle.
        #[serde(rename = "sessionId", default)]
        session_id: String,
    },
}

/// What a completed sign-in produces.
///
/// A struct rather than the `(String, Member, String)` tuple this used to
/// return: two of the three values are a `String`, and swapping them at a call
/// site compiles perfectly while writing a session id into the keychain and a
/// live bearer token into `remotes.json`. The names are the check the compiler
/// cannot otherwise make.
#[derive(Debug, Clone)]
pub struct Session {
    pub token: String,
    pub member: Member,
    /// Not a secret — it names a row, not a credential. Only
    /// `remote::session::ensure` keeps it, for `riabuild remote forget` to
    /// revoke by later; a laptop's own sign-in has nothing to revoke it with.
    pub session_id: String,
}

/// A session minted for a *server* by the laptop provisioning it.
///
/// Separate from `Session` because the two are obtained differently and are
/// read differently. This one carries `expires_at` — the server's own answer,
/// which `remote::session::ensure` records so it knows when to re-mint —
/// and carries no `Member`: the developer this belongs to is the one whose
/// laptop asked, who the caller is already holding.
#[derive(Debug, Clone)]
pub struct ServerSession {
    pub token: String,
    /// Names the `cliSessions` row, so `riabuild remote forget` can revoke
    /// exactly this session through `DELETE /api/v1/cli/sessions/<id>`.
    pub session_id: String,
    /// Unix milliseconds, the server's own reckoning. Not `now + 90 days`
    /// computed here: the TTL is riabuild-web's to choose, and a second copy
    /// of it on this side is a number that can silently disagree.
    pub expires_at: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct ServerSessionReply {
    token: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "expiresAt")]
    expires_at: u64,
}

/// Asks riabuild-web for a session belonging to this developer but labelled
/// after, and destined for, `label` — a server.
///
/// Requires `api` to be carrying a live token: this is a laptop asking, and
/// the answer is refused to a session that was itself obtained this way. One
/// hop only, enforced on the server; see `convex/sessions.ts`.
///
/// There is deliberately no fallback to `login` when the endpoint is missing.
/// Falling back would mean a laptop silently doing the two-approval dance
/// again against a dashboard that had been rolled back, and the developer
/// wondering why the thing that stopped happening started happening — so a
/// missing endpoint says so instead.
pub async fn for_server(api: &ApiClient, label: &str) -> Result<ServerSession> {
    let reply: ServerSessionReply = api
        .post_json(
            "/api/v1/cli/sessions",
            serde_json::json!({ "deviceLabel": label }),
        )
        .await
        .map_err(explain_a_dashboard_that_cannot_delegate)?;

    Ok(ServerSession {
        token: reply.token,
        session_id: reply.session_id,
        expires_at: reply.expires_at,
    })
}

/// Turns "HTTP 404" into the sentence a developer can act on.
///
/// A Convex deployment with no such route answers with its own 404 and no
/// error envelope, so `interpret` reports `upstream_error` and the generic
/// "replied with HTTP 404" — which reads as an outage. The real cause is a
/// riabuild-web older than this binary, and the fix is a deploy, so this is
/// worth naming rather than leaving to be guessed at from a status code.
///
/// Only a 404 is rewritten. A 403 here is `delegation_not_permitted`, which
/// the server already explains far better than this function could.
fn explain_a_dashboard_that_cannot_delegate(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<crate::ApiError>() {
        Some(api_error) if api_error.status == 404 => Failure::new(
            "asking riabuild.clubria.com to sign this server in",
            "Ask your team lead to deploy riabuild-web, then run `riabuild remote` again.",
        )
        .detail("That dashboard is older than this riabuild and has no way to sign a server in without a browser.")
        .into(),
        _ => error,
    }
}

/// Clamps whatever the server asked for into something sane.
pub fn poll_delay(requested: Option<u64>) -> Duration {
    match requested {
        None => DEFAULT_POLL,
        Some(seconds) => Duration::from_secs(seconds).clamp(MIN_POLL, MAX_POLL),
    }
}

/// What riabuild has to know to decide whether opening a browser is worth it.
///
/// Passed in rather than read here so the decision is testable without
/// rewriting the process environment underneath a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserEnv {
    pub over_ssh: bool,
    pub macos: bool,
    pub has_display: bool,
}

/// Whether to try to open a browser at all.
///
/// Over SSH the answer is always no: the terminal is on a server and the
/// browser that matters is on the laptop in front of the developer, so spawning
/// anything here at best opens a window nobody is looking at.
pub fn browser_available(env: BrowserEnv) -> bool {
    if env.over_ssh {
        return false;
    }
    env.macos || env.has_display
}

fn current_browser_env() -> BrowserEnv {
    let set = |key: &str| std::env::var(key).is_ok_and(|value| !value.is_empty());
    BrowserEnv {
        over_ssh: set("SSH_CONNECTION") || set("SSH_TTY") || set("SSH_CLIENT"),
        macos: cfg!(target_os = "macos"),
        has_display: set("DISPLAY") || set("WAYLAND_DISPLAY"),
    }
}

/// A label for this machine, from its hostname. `pub`: `tasks::login` calls
/// this for a laptop's own login; `remote::session::ensure` passes the
/// server's hostname instead, so the dashboard lists each session correctly.
pub async fn device_label(runner: &dyn CommandRunner) -> String {
    let hostname = runner
        .run("hostname", &[], &RunOptions::default())
        .await
        .ok()
        .filter(|output| output.ok())
        .map(|output| output.trimmed().to_string())
        .filter(|name| !name.is_empty());
    hostname.unwrap_or_else(|| "this machine".to_string())
}

async fn open_browser(runner: &dyn CommandRunner, url: &str) -> bool {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    runner
        .run(opener, &[url], &RunOptions::default())
        .await
        .map(|output| output.ok())
        .unwrap_or(false)
}

/// Asks the server to start a device authorisation.
async fn start_device(api: &ApiClient, label: &str) -> Result<DeviceStart> {
    api.post_json(
        "/api/v1/cli/device",
        serde_json::json!({ "deviceLabel": label }),
    )
    .await
}

/// Whether a failed poll is worth another attempt before the deadline.
///
/// Only a failure to reach the server at all. This loop stays open for up to
/// fifteen minutes while a developer walks to another machine, and a closed lid
/// or a wifi handover in the middle of that is not a reason to throw away a code
/// they are about to approve.
///
/// Everything the server actually answered is final. Retrying a 401 would poll a
/// spent code until the deadline and then report that nobody approved it, which
/// is both wrong and the least useful thing to say.
pub fn survives_a_failed_poll(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::ApiError>(),
        Some(api_error) if api_error.code == "unreachable"
    )
}

/// Polls until the request is answered, expires, or the developer gives up.
async fn wait_for_approval(api: &ApiClient, start: &DeviceStart) -> Result<Session> {
    let lifetime = start
        .expires_in
        .map(Duration::from_secs)
        .unwrap_or(FALLBACK_EXPIRY);
    let deadline = Instant::now() + lifetime;
    let mut delay = poll_delay(start.interval);

    loop {
        // Waiting first, not last: the developer has not had time to read the
        // code yet, let alone type it, so an immediate poll only ever costs a
        // request to be told "pending".
        sleep(delay).await;

        if Instant::now() >= deadline {
            return Err(anyhow!(
                "that code expired before it was approved (codes last {} minutes)",
                lifetime.as_secs() / 60
            ));
        }

        let polled = api
            .post_json::<PollResponse>(
                "/api/v1/cli/token",
                serde_json::json!({ "deviceCode": start.device_code }),
            )
            .await;

        let response = match polled {
            Ok(response) => response,
            Err(error) if survives_a_failed_poll(&error) => continue,
            Err(error) => return Err(error),
        };

        match response {
            PollResponse::Pending { interval } => {
                delay = poll_delay(interval.or(start.interval));
            }
            PollResponse::Denied => {
                return Err(anyhow!("that request was denied in the browser"));
            }
            PollResponse::Ok {
                token,
                member,
                session_id,
            } => {
                return Ok(Session {
                    token,
                    member,
                    session_id,
                });
            }
        }
    }
}

/// Runs the whole flow and returns the session token, the member it belongs
/// to, and the `cliSessions` row id behind it. The caller stores the token in
/// the keychain; it is never written to `~/.riabuild`.
///
/// Takes neither a dashboard URL nor a version: the server builds the
/// verification URL, because it is the thing that knows where the dashboard is
/// deployed, and reads the version off the `x-riabuild-cli-version` header
/// `ApiClient` already sends on every request.
///
/// `label` *is* the caller's to choose, so the dashboard lists each session
/// under the device it belongs to rather than always the hostname of the
/// machine running this code: `remote::session::ensure` signs a *server* in
/// from the laptop's browser, and labelling that session after the laptop
/// would leave `riabuild remote list` and `forget` unable to tell the two
/// apart. `device_label` is the answer a laptop's own sign-in passes.
/// For the same reason, printing *why* this login is happening is the
/// caller's too: the heading lives at each call site, not here.
pub async fn login(
    api: &ApiClient,
    runner: &dyn CommandRunner,
    ui: &Ui,
    label: &str,
) -> Result<Session> {
    let start = start_device(api, label).await?;

    // Printed before any browser is attempted, and printed whatever happens.
    // Over SSH this is the whole interface, and on a laptop it is what the
    // developer checks the browser against.
    // The code is highlighted and the link is not: the link is the one a
    // terminal makes clickable on its own, while the code is typed by hand off
    // this screen into a browser that may be on another machine.
    ui.note(&format!("Open {}", start.verification_uri));
    ui.note_value("Enter code", &start.user_code);

    if browser_available(current_browser_env()) {
        let target = start
            .verification_uri_complete
            .as_deref()
            .unwrap_or(&start.verification_uri);
        if !open_browser(runner, target).await {
            ui.note("Could not open your browser — use the link above.");
        }
    }

    ui.info("");
    ui.info("Waiting for you to approve this machine…");

    wait_for_approval(api, &start).await.map_err(|error| {
        Failure::new(
            "waiting for you to approve this machine in the browser",
            "Run `riabuild login` again and approve the request.",
        )
        .detail(error.to_string())
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_without_a_session_id_still_signs_the_developer_in() {
        // A riabuild-web older than this binary — or one that has just been
        // rolled back — does not send `sessionId`. Failing the decode would
        // fail *login*, on every command, over a field only `riabuild remote
        // forget` ever reads. Empty is the same state `store::Record` already
        // treats as "no session to revoke".
        let older: PollResponse = serde_json::from_value(serde_json::json!({
            "status": "ok",
            "token": "rb_live_abc",
            "member": {
                "githubLogin": "ada",
                "memberId": "550e8400-e29b-41d4-a716-446655440000",
                "firstName": "Ada",
                "lastName": "Lovelace",
                "email": "ada@clubria.dev",
                "role": "member",
                "status": "active"
            }
        }))
        .expect("a missing sessionId must not fail login");
        match older {
            PollResponse::Ok {
                token, session_id, ..
            } => {
                assert_eq!(session_id, "");
                assert_eq!(token, "rb_live_abc");
            }
            other => panic!("expected a grant, got {other:?}"),
        }
    }

    #[test]
    fn a_server_session_carries_the_token_the_row_and_the_servers_own_expiry() {
        // The expiry is read rather than computed: `remote::session::ensure`
        // used to add its own copy of riabuild-web's ninety days, which is a
        // number that can disagree with the one the session actually has.
        let reply: ServerSessionReply = serde_json::from_str(
            r#"{"token":"rb_live_srv","sessionId":"sess_9","expiresAt":1786000000000,
                "member":{"githubLogin":"ada","memberId":"550e8400-e29b-41d4-a716-446655440000",
                "firstName":"Ada","lastName":"Lovelace","email":"ada@clubria.dev",
                "role":"developer","status":"active"}}"#,
        )
        .unwrap();
        assert_eq!(reply.token, "rb_live_srv");
        assert_eq!(reply.session_id, "sess_9");
        assert_eq!(reply.expires_at, 1_786_000_000_000);
    }

    #[test]
    fn a_dashboard_with_no_such_endpoint_is_named_rather_than_reported_as_an_outage() {
        // Convex answers an unrouted path with its own 404 and no error
        // envelope, so `interpret` produces the generic "replied with HTTP
        // 404" — which reads as riabuild-web being down. The cause is a
        // dashboard older than this binary and the fix is a deploy, so the
        // message has to say so.
        let translated = explain_a_dashboard_that_cannot_delegate(
            crate::ApiError {
                status: 404,
                code: "upstream_error".into(),
                message: "riabuild.clubria.com replied with HTTP 404.".into(),
                action: "Try again in a minute; if it persists, tell your team lead.".into(),
            }
            .into(),
        );
        let failure = translated
            .downcast_ref::<Failure>()
            .expect("a 404 here must become an actionable Failure");
        assert!(
            failure.action.contains("deploy riabuild-web"),
            "{failure:?}"
        );
    }

    #[test]
    fn a_refusal_to_delegate_is_left_exactly_as_the_server_worded_it() {
        // A server's own token asking to sign a third machine in gets a 403
        // the server explains precisely. Rewriting it would replace "run this
        // from your laptop" with a sentence about deploys.
        let refused: anyhow::Error = crate::ApiError {
            status: 403,
            code: "delegation_not_permitted".into(),
            message: "This machine's riabuild session was itself signed in by another machine."
                .into(),
            action: "Run `riabuild remote` from your own laptop.".into(),
        }
        .into();
        let passed_through = explain_a_dashboard_that_cannot_delegate(refused);
        let api_error = passed_through
            .downcast_ref::<crate::ApiError>()
            .expect("a 403 must stay the server's own error");
        assert_eq!(api_error.code, "delegation_not_permitted");
    }

    #[test]
    fn a_pending_poll_is_a_normal_reply_not_an_error() {
        // The CLI sees this dozens of times per login. Decoding it as anything
        // other than an ordinary response would mean unwinding on every tick.
        let response: PollResponse =
            serde_json::from_str(r#"{"status":"pending","interval":5}"#).unwrap();
        assert!(matches!(
            response,
            PollResponse::Pending { interval: Some(5) }
        ));
    }

    #[test]
    fn a_pending_poll_without_an_interval_still_decodes() {
        let response: PollResponse = serde_json::from_str(r#"{"status":"pending"}"#).unwrap();
        assert!(matches!(response, PollResponse::Pending { interval: None }));
    }

    #[test]
    fn a_denial_is_distinguishable_from_a_wait() {
        // "No" and "not yet" lead to opposite behaviour: one stops, the other
        // keeps polling. Collapsing them would hang a refused login forever.
        let response: PollResponse = serde_json::from_str(r#"{"status":"denied"}"#).unwrap();
        assert!(matches!(response, PollResponse::Denied));
    }

    #[test]
    fn a_grant_carries_the_token_the_member_and_the_session_it_opened() {
        // The session id is what `riabuild remote forget` revokes a *server's*
        // token by, through `DELETE /api/v1/cli/sessions/<id>`. Dropping it
        // here would compile and would leave a live 90-day bearer credential
        // on a shared box after a `forget` that reported success.
        let response: PollResponse = serde_json::from_str(
            r#"{"status":"ok","token":"tok_1","sessionId":"sess_1","expiresAt":123,"member":{
                 "githubLogin":"ada","memberId":"550e8400-e29b-41d4-a716-446655440000",
                 "firstName":"Ada","lastName":"Lovelace",
                 "email":"ada@clubria.dev","role":"developer","status":"active"}}"#,
        )
        .unwrap();
        match response {
            PollResponse::Ok {
                token,
                member,
                session_id,
            } => {
                assert_eq!(token, "tok_1");
                assert_eq!(member.github_login, "ada");
                assert_eq!(session_id, "sess_1");
            }
            other => panic!("expected a grant, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_status_is_refused_rather_than_guessed() {
        // A future server state must not be read as one of today's. Failing to
        // decode surfaces as an error; guessing "ok" would invent a session.
        assert!(serde_json::from_str::<PollResponse>(r#"{"status":"slow_down"}"#).is_err());
    }

    #[test]
    fn the_device_start_reads_the_fields_the_server_sends() {
        let start: DeviceStart = serde_json::from_str(
            r#"{"deviceCode":"dc_1","userCode":"WXZB-CDFG",
                "verificationUri":"https://riabuild.clubria.com/cli",
                "verificationUriComplete":"https://riabuild.clubria.com/cli?code=WXZB-CDFG",
                "expiresIn":900,"interval":5}"#,
        )
        .unwrap();
        assert_eq!(start.device_code, "dc_1");
        assert_eq!(start.user_code, "WXZB-CDFG");
        assert_eq!(start.expires_in, Some(900));
    }

    #[test]
    fn a_server_that_omits_the_optional_fields_still_starts_a_login() {
        // Every optional field has a working default, so an older or trimmed
        // response degrades rather than failing the login outright.
        let start: DeviceStart = serde_json::from_str(
            r#"{"deviceCode":"dc_1","userCode":"WXZB-CDFG",
                "verificationUri":"https://riabuild.clubria.com/cli"}"#,
        )
        .unwrap();
        assert_eq!(start.verification_uri_complete, None);
        assert_eq!(poll_delay(start.interval), DEFAULT_POLL);
    }

    #[test]
    fn the_poll_interval_is_clamped_at_both_ends() {
        // A zero would spin this loop against the API; an hour would leave a
        // developer looking at an approved tab and a terminal that has not
        // noticed. Neither is worth trusting a server field for.
        assert_eq!(poll_delay(Some(0)), MIN_POLL);
        assert_eq!(poll_delay(Some(3600)), MAX_POLL);
        assert_eq!(poll_delay(Some(5)), Duration::from_secs(5));
        assert_eq!(poll_delay(None), DEFAULT_POLL);
    }

    fn api_error(code: &str) -> anyhow::Error {
        crate::ApiError {
            status: 0,
            code: code.into(),
            message: "x".into(),
            action: "y".into(),
        }
        .into()
    }

    #[test]
    fn a_dropped_connection_does_not_end_a_fifteen_minute_wait() {
        // This loop runs for up to fifteen minutes while a developer walks to
        // another machine. A closed lid or a wifi handover in the middle of
        // that must not throw away a code they are about to approve.
        assert!(survives_a_failed_poll(&api_error("unreachable")));
    }

    #[test]
    fn a_server_that_says_the_code_is_dead_stops_the_loop() {
        // The opposite mistake: retrying a 401 would poll a spent code until
        // the deadline and then blame the developer for not approving it.
        assert!(!survives_a_failed_poll(&api_error("unauthenticated")));
        assert!(!survives_a_failed_poll(&api_error("suspended")));
        assert!(!survives_a_failed_poll(&api_error("cli_too_old")));
    }

    #[test]
    fn an_error_that_is_not_the_servers_stops_the_loop() {
        // A malformed body is a bug, not weather. Retrying hides it.
        assert!(!survives_a_failed_poll(&anyhow!(
            "could not read the reply"
        )));
    }

    #[test]
    fn ssh_never_opens_a_browser() {
        // The whole reason this flow exists: over SSH the browser that matters
        // is on the laptop, and anything opened here is on the wrong machine.
        for env in [
            BrowserEnv {
                over_ssh: true,
                macos: true,
                has_display: true,
            },
            BrowserEnv {
                over_ssh: true,
                macos: false,
                has_display: false,
            },
        ] {
            assert!(!browser_available(env), "{env:?}");
        }
    }

    #[test]
    fn a_desktop_session_still_gets_its_browser_opened() {
        assert!(browser_available(BrowserEnv {
            over_ssh: false,
            macos: true,
            has_display: false,
        }));
        assert!(browser_available(BrowserEnv {
            over_ssh: false,
            macos: false,
            has_display: true,
        }));
    }

    #[test]
    fn a_linux_box_with_no_display_is_not_offered_a_browser() {
        // A headless server someone is sitting at physically, or a container.
        assert!(!browser_available(BrowserEnv {
            over_ssh: false,
            macos: false,
            has_display: false,
        }));
    }
}

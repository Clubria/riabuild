//! CLI login — device authorisation, the flow shaped after RFC 8628.
//!
//! riabuild asks the server for a pair of codes, prints the short one, and
//! polls until a developer approves it in a browser. Nothing here binds a
//! socket and nothing travels through a redirect, so a terminal on a server
//! reached over SSH signs in exactly the way a terminal on a laptop does.
//!
//! Two codes, two jobs. The `device_code` never leaves this process and is what
//! the poll is authenticated with; the `user_code` is read aloud off the
//! terminal and typed into the dashboard, and can be exchanged for nothing. The
//! developer matching one against the other is what stops a stranger's approval
//! link signing a stranger's machine in.

use crate::api::{ApiClient, Member};
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::{Result, anyhow};
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
    Pending { interval: Option<u64> },
    Denied,
    Ok { token: String, member: Member },
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

async fn device_label(runner: &dyn CommandRunner) -> String {
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
        error.downcast_ref::<crate::api::ApiError>(),
        Some(api_error) if api_error.code == "unreachable"
    )
}

/// Polls until the request is answered, expires, or the developer gives up.
async fn wait_for_approval(api: &ApiClient, start: &DeviceStart) -> Result<(String, Member)> {
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
            PollResponse::Ok { token, member } => return Ok((token, member)),
        }
    }
}

/// Runs the whole flow and returns the session token. The caller stores it in
/// the keychain; it is never written to `~/.riabuild`.
///
/// Takes neither a dashboard URL nor a version: the server builds the
/// verification URL, because it is the thing that knows where the dashboard is
/// deployed, and reads the version off the `x-riabuild-cli-version` header
/// `ApiClient` already sends on every request.
pub async fn login(
    api: &ApiClient,
    runner: &dyn CommandRunner,
    ui: &Ui,
) -> Result<(String, Member)> {
    let label = device_label(runner).await;
    let start = start_device(api, &label).await?;

    ui.heading("Signing this machine in to riabuild");

    // Printed before any browser is attempted, and printed whatever happens.
    // Over SSH this is the whole interface, and on a laptop it is what the
    // developer checks the browser against.
    ui.note(&format!("Open {}", start.verification_uri));
    ui.note(&format!("Enter code {}", start.user_code));

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
    fn a_grant_carries_the_token_and_the_member() {
        let response: PollResponse = serde_json::from_str(
            r#"{"status":"ok","token":"tok_1","expiresAt":123,"member":{
                 "githubLogin":"ada","firstName":"Ada","lastName":"Lovelace",
                 "email":"ada@clubria.dev","role":"developer","status":"active"}}"#,
        )
        .unwrap();
        match response {
            PollResponse::Ok { token, member } => {
                assert_eq!(token, "tok_1");
                assert_eq!(member.github_login, "ada");
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
        crate::api::ApiError {
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

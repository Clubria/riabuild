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

//! This file is the device flow itself. [`reply`] holds what the server sends
//! back, [`browser`] decides whether one is opened and which, and
//! [`delegate`] is the second way in.

use crate::ApiClient;
use anyhow::{Result, anyhow};
use riabuild_runner::CommandRunner;
use riabuild_ui::{Failure, Ui};
use std::time::Duration;
use tokio::time::{Instant, sleep};

mod browser;
mod delegate;
mod reply;

pub use browser::{BrowserEnv, browser_available, device_label};
use browser::{current_browser_env, open_browser, verification_link};
pub use delegate::for_server;
pub use reply::{DeviceStart, PollResponse, ServerSession, Session};

/// Bounds on the poll interval the server asks for.
///
/// The server picks the cadence, but not without limits: a zero would spin this
/// loop against the API and a very large one would leave a developer staring at
/// an approved browser tab and a terminal that has not noticed.
const MIN_POLL: Duration = Duration::from_secs(1);
const MAX_POLL: Duration = Duration::from_secs(60);
pub(crate) const DEFAULT_POLL: Duration = Duration::from_secs(5);

/// A ceiling on how long to wait when the server does not say.
const FALLBACK_EXPIRY: Duration = Duration::from_secs(15 * 60);

/// Clamps whatever the server asked for into something sane.
pub fn poll_delay(requested: Option<u64>) -> Duration {
    match requested {
        None => DEFAULT_POLL,
        Some(seconds) => Duration::from_secs(seconds).clamp(MIN_POLL, MAX_POLL),
    }
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
    // terminal makes clickable on its own, while the code is read off this
    // screen and checked against the machine named in the browser.
    let link = verification_link(&start);
    ui.note(&format!("Open {link}"));
    ui.note_value("Enter code", &start.user_code);

    // One read of the environment feeds both halves: whether to try at all, and
    // which opener to try. Asking `cfg!` again inside `open_browser` is how the
    // two came to disagree about the same machine.
    let browser = current_browser_env();
    if browser_available(browser) && !open_browser(browser.macos, runner, link).await {
        ui.note("Could not open your browser — use the link above.");
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
}

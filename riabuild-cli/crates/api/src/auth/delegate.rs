//! A signed-in laptop asking for a session on a server's behalf.
//!
//! No browser is involved: the bearer token on the request already proves what
//! a second device code would have asked the developer to prove again. There
//! is deliberately no fallback to the device flow when the endpoint is
//! missing — see [`for_server`].

use anyhow::Result;
use riabuild_ui::Failure;

use super::reply::{ServerSession, ServerSessionReply};
use crate::ApiClient;

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
#[cfg(test)]
mod tests {
    use super::*;

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
}

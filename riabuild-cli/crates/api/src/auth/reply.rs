//! What the two sign-in routes hand back, as the wire sends it.
//!
//! Deserialization only: nothing here asks the server anything, and every
//! shape has a test that reads a literal body the dashboard could send.

use serde::Deserialize;

use crate::Member;

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
pub(super) struct ServerSessionReply {
    pub(super) token: String,
    #[serde(rename = "sessionId")]
    pub(super) session_id: String,
    #[serde(rename = "expiresAt")]
    pub(super) expires_at: u64,
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{DEFAULT_POLL, poll_delay};

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
}

//! The riabuild-web client.
//!
//! The server ships data, never logic: every response here is settings, a slug,
//! a version floor or a brokered token. Nothing returned by this module is ever
//! executed.

pub mod auth;
pub mod org;
pub mod secrets;

use crate::ui::Failure;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

/// The dashboard a browser is sent to.
pub const DEFAULT_WEB_URL: &str = "https://riabuild.clubria.com";

/// The `/api/v1` origin — the Convex deployment's own hostname.
///
/// Not `api.riabuild.clubria.com`, which cannot be made to work for free:
/// unproxied, TLS terminates at Convex, which only holds a certificate for
/// `*.convex.site`; proxied, Cloudflare's Universal SSL covers only one label
/// below the apex, and `api.riabuild` is two. Pointing a custom name here needs
/// Convex's custom-domain feature.
///
/// No developer ever types this, so a pretty name buys nothing. Overridable with
/// `RIABUILD_API_URL` for local development.
pub const DEFAULT_API_URL: &str = "https://handsome-vulture-127.eu-west-1.convex.site";

pub fn web_url() -> String {
    trim(std::env::var("RIABUILD_WEB_URL").unwrap_or_else(|_| DEFAULT_WEB_URL.to_string()))
}

pub fn api_url() -> String {
    trim(std::env::var("RIABUILD_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string()))
}

fn trim(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

/// An error the server described in terms a developer can act on. Printed
/// verbatim rather than reworded — the server knows why, the CLI does not.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub status: u16,
    pub code: String,
    pub message: String,
    pub action: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.message, self.action)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    /// The CLI must not try to re-authenticate its way out of a 403.
    pub fn needs_login(&self) -> bool {
        matches!(
            self.code.as_str(),
            "unauthenticated" | "session_expired" | "session_revoked"
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorEnvelope {
    error: ApiError,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    #[serde(rename = "githubLogin")]
    pub github_login: String,
    /// Immutable and ours. Names this developer's directory on a server.
    /// Deliberately not `#[serde(default)]`: an identifier that half the
    /// deployments might not send is not an identifier.
    #[serde(rename = "memberId", deserialize_with = "uuid_only")]
    pub member_id: String,
    #[serde(rename = "firstName")]
    pub first_name: String,
    #[serde(rename = "lastName")]
    pub last_name: String,
    pub email: String,
    pub role: String,
    pub status: String,
}

/// Refuses anything that is not a lowercase, hyphenated UUID.
///
/// An empty or malformed `member_id` is worse than a missing one: it reaches a
/// remote command line, and an empty one collapses `~/.riabuild-remote/<member-id>`
/// to `~/.riabuild-remote`, which puts every developer in one namespace and
/// makes `forget`'s cleanup delete all of them.
fn uuid_only<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = String::deserialize(deserializer)?;
    let shaped = value.len() == 36
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit() && !character.is_ascii_uppercase(),
            });
    if shaped {
        Ok(value)
    } else {
        Err(D::Error::custom(format!("{value:?} is not a member id")))
    }
}

impl Member {
    pub fn display_name(&self) -> String {
        let name = format!("{} {}", self.first_name, self.last_name);
        let name = name.trim();
        if name.is_empty() {
            format!("@{}", self.github_login)
        } else {
            name.to_string()
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiClient {
    api_url: String,
    token: Option<String>,
    version: String,
    /// One client for the process. Cloning an `ApiClient` shares this pool
    /// rather than copying it, which is why `Clone` is still cheap.
    client: reqwest::Client,
}

impl ApiClient {
    pub fn new(version: impl Into<String>) -> Self {
        let version = version.into();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(format!("riabuild/{version}"))
            .build()
            // A client that will not build is a broken TLS backend, not a
            // developer's mistake. `default()` still produces a usable client,
            // and every request through it will report its own failure with an
            // action attached — better than panicking during startup.
            .unwrap_or_default();
        Self {
            api_url: api_url(),
            token: None,
            version,
            client,
        }
    }

    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .request(method, format!("{}{path}", self.api_url))
            .header("x-riabuild-cli-version", &self.version);
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        interpret(self.request(reqwest::Method::GET, path).send().await, path).await
    }

    pub async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        interpret(
            self.request(reqwest::Method::POST, path)
                .json(&body)
                .send()
                .await,
            path,
        )
        .await
    }

    /// `DELETE`, for `remote::flow::forget_remote`'s call to
    /// `/api/v1/cli/sessions/<id>`. No body either direction: the id is in
    /// the path, and the server replies with a small JSON envelope.
    pub async fn delete_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        interpret(
            self.request(reqwest::Method::DELETE, path).send().await,
            path,
        )
        .await
    }

    /// `GET /api/v1/me`
    ///
    /// Fetches the envelope untyped through `interpret`, then hands the
    /// `member` field to `decode_member` as a separate step. `interpret`
    /// already distinguishes an `ApiError` (the server explaining a 4xx/5xx)
    /// from a transport failure; keeping the `Member` decode out of that same
    /// `?` — and out of a `map_err` that would also catch the `ApiError`
    /// `interpret` can return — is what keeps a *decode* failure (a
    /// dashboard older than this binary, sending no `memberId`) from ever
    /// being confused with an ordinary expired session. Conflating the two
    /// would stop `main::connect`'s `needs_login()` check from ever seeing a
    /// 401 and send the developer to fix infrastructure instead of signing
    /// in again.
    pub async fn me(&self) -> Result<Member> {
        let envelope: serde_json::Value = self.get_json("/api/v1/me").await?;
        decode_member(envelope)
    }
}

/// Pulls `Member` out of the `{ "member": { ... } }` envelope `/api/v1/me`
/// returns, reporting a decode failure as "the dashboard is stale" rather
/// than the raw serde error `main.rs` would otherwise print as an unnamed
/// bug. Kept as a standalone function — rather than inlined into `me()` —
/// specifically so a test can call the exact code `me()` runs instead of a
/// hand-copied stand-in that would silently stop matching it.
fn decode_member(envelope: serde_json::Value) -> Result<Member> {
    let member = envelope
        .get("member")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value::<Member>(member).map_err(|error| {
        Failure::new(
            "reading your riabuild profile",
            "Ask your team lead to deploy the dashboard — this riabuild is newer than it.",
        )
        .detail(error.to_string())
        .into()
    })
}

/// Turns a reqwest result into either the decoded body or an `ApiError`.
///
/// The shape differs from the ureq version in one way that matters: ureq
/// signalled an HTTP failure through `Err(Error::Status(..))`, whereas reqwest
/// returns `Ok(response)` and expects the status to be inspected. Dropping that
/// check would silently treat every 4xx as a successful reply.
async fn interpret<T: serde::de::DeserializeOwned>(
    result: Result<reqwest::Response, reqwest::Error>,
    path: &str,
) -> Result<T> {
    let response = match result {
        Ok(response) => response,
        Err(transport) => {
            return Err(ApiError {
                status: 0,
                code: "unreachable".into(),
                message: format!("riabuild could not reach riabuild-web ({transport})."),
                action: "Check your network connection and try again.".into(),
            }
            .into());
        }
    };

    let status = response.status().as_u16();
    if response.status().is_success() {
        return response
            .json::<T>()
            .await
            .with_context(|| format!("riabuild could not read the reply from {path}"));
    }

    // A structured error is the server explaining itself; anything else is a
    // proxy or an outage, and gets a generic shape.
    match response.json::<ErrorEnvelope>().await {
        Ok(envelope) => {
            let mut error = envelope.error;
            error.status = status;
            Err(error.into())
        }
        Err(_) => Err(ApiError {
            status,
            code: "upstream_error".into(),
            message: format!("riabuild.clubria.com replied with HTTP {status}."),
            action: "Try again in a minute; if it persists, tell your team lead.".into(),
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_403_is_never_treated_as_a_login_problem() {
        // Re-authenticating after losing org membership would succeed and loop.
        let lost_org = ApiError {
            status: 403,
            code: "not_org_member".into(),
            message: "x".into(),
            action: "y".into(),
        };
        assert!(!lost_org.needs_login());

        let expired = ApiError {
            status: 401,
            code: "session_expired".into(),
            message: "x".into(),
            action: "y".into(),
        };
        assert!(expired.needs_login());
    }

    #[test]
    fn urls_lose_their_trailing_slash() {
        assert_eq!(
            trim("https://example.com/".to_string()),
            "https://example.com"
        );
    }

    #[test]
    fn a_member_without_a_name_still_has_something_to_greet() {
        let member = Member {
            github_login: "ada".into(),
            member_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            first_name: String::new(),
            last_name: String::new(),
            email: "ada@clubria.dev".into(),
            role: "developer".into(),
            status: "active".into(),
        };
        assert_eq!(member.display_name(), "@ada");
    }

    #[test]
    fn a_member_payload_carries_the_member_id() {
        let member: Member = serde_json::from_str(
            r#"{"githubLogin":"ada","githubId":"1234","memberId":"550e8400-e29b-41d4-a716-446655440000",
                "firstName":"Ada","lastName":"Lovelace","email":"ada@clubria.dev",
                "role":"developer","status":"active"}"#,
        )
        .expect("payload should parse");
        assert_eq!(member.member_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn a_payload_without_a_member_id_is_refused() {
        // A deployment older than this binary. Failing here is correct: the
        // alternative is a namespace directory named after an empty string,
        // silently shared by every developer on a server.
        let parsed = serde_json::from_str::<Member>(
            r#"{"githubLogin":"ada","githubId":"1234","firstName":"Ada","lastName":"Lovelace",
                "email":"ada@clubria.dev","role":"developer","status":"active"}"#,
        );
        assert!(parsed.is_err(), "a missing memberId must not default");
    }

    #[test]
    fn a_member_id_that_is_not_a_uuid_is_refused() {
        for bad in [
            "",
            "../../etc",
            "not-a-uuid",
            "550E8400-E29B-41D4-A716-446655440000",
            " 550e8400-e29b-41d4-a716-446655440000",
            "550e8400-e29b-41d4-a716-446655440000 ",
        ] {
            let json = format!(
                r#"{{"githubLogin":"ada","githubId":"1","memberId":"{bad}","firstName":"A",
                    "lastName":"B","email":"a@b.c","role":"developer","status":"active"}}"#
            );
            assert!(
                serde_json::from_str::<Member>(&json).is_err(),
                "{bad:?} must not be accepted as a member id"
            );
        }
    }

    // The auth half of the decode-vs-auth split (an ApiError surviving as an
    // ApiError through `?`) is exercised end to end by
    // `a_403_is_never_treated_as_a_login_problem` above — this file has no
    // HTTP stub, so there is no `ApiClient::me()` call to drive through a
    // real 401/403 without inventing that scaffolding. A prior version of
    // this test constructed an `ApiError` by hand and asserted a
    // downcast/`needs_login` property that belongs to `anyhow` and
    // `ApiError`, not to anything in this file's control flow, and never
    // called `me()` — so it was deleted rather than kept as padding.

    #[test]
    fn decode_member_reports_a_missing_member_id_as_a_stale_dashboard_not_a_bug() {
        // Calls the exact function `me()` calls — not a hand-copied
        // stand-in — so this test tracks `me()`'s real behavior. If `me()`
        // regressed to wrapping its whole body (including the propagated
        // `ApiError` from `interpret`) in one `map_err`, that regression
        // would not touch this function's signature or this test's call
        // site, which is exactly why the split into `decode_member` matters:
        // a test on `me()` itself would need an HTTP stub this file does not
        // have, but `decode_member` is where the actual risk (a decode
        // failure) lives, and it is plain data in, `Result` out.
        let envelope = serde_json::json!({
            "member": {
                "githubLogin": "ada",
                "firstName": "Ada",
                "lastName": "Lovelace",
                "email": "ada@clubria.dev",
                "role": "developer",
                "status": "active",
            }
        });
        let error =
            decode_member(envelope).expect_err("a payload with no memberId must not decode");
        let failure = error
            .downcast_ref::<Failure>()
            .expect("a decode failure must surface as a Failure, not an opaque error");
        assert!(failure.action.contains("deploy the dashboard"));
    }

    #[test]
    fn decode_member_reads_a_well_formed_envelope() {
        let envelope = serde_json::json!({
            "member": {
                "githubLogin": "ada",
                "memberId": "550e8400-e29b-41d4-a716-446655440000",
                "firstName": "Ada",
                "lastName": "Lovelace",
                "email": "ada@clubria.dev",
                "role": "developer",
                "status": "active",
            }
        });
        let member = decode_member(envelope).expect("a well-formed envelope should decode");
        assert_eq!(member.member_id, "550e8400-e29b-41d4-a716-446655440000");
    }
}

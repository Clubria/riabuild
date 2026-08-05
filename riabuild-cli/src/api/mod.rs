//! The riabuild-web client.
//!
//! The server ships data, never logic: every response here is settings, a slug,
//! a version floor or a brokered token. Nothing returned by this module is ever
//! executed.

pub mod auth;
pub mod org;
pub mod secrets;

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
    #[serde(rename = "firstName")]
    pub first_name: String,
    #[serde(rename = "lastName")]
    pub last_name: String,
    pub email: String,
    pub role: String,
    pub status: String,
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
}

impl ApiClient {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            api_url: api_url(),
            token: None,
            version: version.into(),
        }
    }

    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let request = ureq::request(method, &format!("{}{path}", self.api_url))
            .timeout(Duration::from_secs(30))
            .set("x-riabuild-cli-version", &self.version)
            .set("user-agent", &format!("riabuild/{}", self.version));
        match &self.token {
            Some(token) => request.set("authorization", &format!("Bearer {token}")),
            None => request,
        }
    }

    pub fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        interpret(self.request("GET", path).call(), path)
    }

    pub fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        interpret(self.request("POST", path).send_json(body), path)
    }

    /// `GET /api/v1/me`
    pub fn me(&self) -> Result<Member> {
        #[derive(Deserialize)]
        struct Envelope {
            member: Member,
        }
        Ok(self.get_json::<Envelope>("/api/v1/me")?.member)
    }
}

fn interpret<T: serde::de::DeserializeOwned>(
    result: Result<ureq::Response, ureq::Error>,
    path: &str,
) -> Result<T> {
    match result {
        Ok(response) => response
            .into_json::<T>()
            .with_context(|| format!("riabuild could not read the reply from {path}")),
        Err(ureq::Error::Status(status, response)) => {
            // A structured error is the server explaining itself; anything else
            // is a proxy or an outage, and gets a generic shape.
            match response.into_json::<ErrorEnvelope>() {
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
        Err(transport) => Err(ApiError {
            status: 0,
            code: "unreachable".into(),
            message: format!("riabuild could not reach riabuild-web ({transport})."),
            action: "Check your network connection and try again.".into(),
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
            first_name: String::new(),
            last_name: String::new(),
            email: "ada@clubria.dev".into(),
            role: "developer".into(),
            status: "active".into(),
        };
        assert_eq!(member.display_name(), "@ada");
    }
}

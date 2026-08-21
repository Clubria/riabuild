//! What a failed request becomes.
//!
//! The server describes its own failures in terms a developer can act on, and
//! they are printed verbatim rather than reworded — the server knows why, the
//! CLI does not. [`interpret`] is the one place an HTTP status becomes one of
//! these, which is what keeps "a 4xx is not a successful reply" a property of
//! the client rather than of each call site.

use anyhow::{Context, Result};
use serde::Deserialize;

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

/// Turns a reqwest result into either the decoded body or an `ApiError`.
///
/// The shape differs from the ureq version in one way that matters: ureq
/// signalled an HTTP failure through `Err(Error::Status(..))`, whereas reqwest
/// returns `Ok(response)` and expects the status to be inspected. Dropping that
/// check would silently treat every 4xx as a successful reply.
pub(crate) async fn interpret<T: serde::de::DeserializeOwned>(
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
}

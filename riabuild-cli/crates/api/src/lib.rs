//! The riabuild-web client.
//!
//! The server ships data, never logic: every response here is settings, a slug,
//! a version floor or a brokered token. Nothing returned by this module is ever
//! executed.

// The panic lints are denied workspace-wide. In tests a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture there
// is correct and this keeps the deny from forcing ceremony into every test
// module. The exemption is `test` and nothing wider — see the workspace
// manifest for what an `any(test, feature = "testing")` spelling of it costs.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

pub mod auth;
pub mod issued;
/// The client driven over real HTTP against a server on a loopback port —
/// URLs, headers, status mapping and deserialization, none of which a fake can
/// see. Tests only; see the module's own documentation.
#[cfg(test)]
mod loopback;
pub mod ngrok;
pub mod openssh;
pub mod org;
pub mod remotes;
pub mod repo;
pub mod secrets;

pub use repo::Repo;
mod error;
mod member;

pub use error::ApiError;
use error::interpret;
pub use member::Member;
use member::decode_member;

use anyhow::Result;
use std::time::Duration;

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

// There is deliberately no `web_url()`. The CLI no longer builds any dashboard
// link: `POST /api/v1/cli/device` returns the verification URL, because the
// server is the thing that knows where the dashboard is deployed. A second copy
// of that answer here could disagree with it.

pub fn api_url() -> String {
    trim(std::env::var("RIABUILD_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string()))
}

fn trim(url: String) -> String {
    url.trim_end_matches('/').to_string()
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

    /// The client `new` builds, pointed at a URL a test chose.
    ///
    /// `new` takes the origin from `RIABUILD_API_URL`, and two tests running at
    /// once cannot each have their own copy of the process environment — nor
    /// may a test mutate it under the others. Everything else is the
    /// production path, because this *is* `new`: the same timeout, the same
    /// user agent, the same `request()` that attaches the version header and
    /// the bearer token.
    #[cfg(test)]
    pub(crate) fn pointed_at(version: impl Into<String>, api_url: &str) -> Self {
        let mut client = Self::new(version);
        client.api_url = trim(api_url.to_string());
        client
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

    /// `DELETE`, for `remote::forget::forget_remote`'s call to
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_their_trailing_slash() {
        assert_eq!(
            trim("https://example.com/".to_string()),
            "https://example.com"
        );
    }
    // The auth half of the decode-vs-auth split — an `ApiError` surviving as
    // an `ApiError` through `?`, and a decode failure never becoming one — is
    // driven through a real `ApiClient::me()` over a loopback server in
    // `loopback`, which is also where the header, the bearer token and the
    // status mapping are asserted against what `convex/http.ts` sends.
}

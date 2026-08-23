//! The client driven over real HTTP, against a server bound on loopback.
//!
//! Every other test in this crate hands a decoder a `&str` or a predicate a
//! hand-built [`ApiError`]. What none of them can see is the layer between the
//! two — the URL a path is joined onto, the header the version floor is
//! enforced against, the bearer token, and which HTTP status becomes which
//! error — and that layer is the one that breaks in production, because the
//! thing on the other end of it is a server this repository also owns and
//! tests separately. Two suites either side of a contract that nothing
//! exercises is the shape of an outage nobody's tests can see coming.
//!
//! The server is hand-rolled rather than a canned-response crate, following
//! `riabuild-fetch`'s `download::tests::serve_once`. These tests need to
//! control the shape of the *response* — a body that stops part way through, a
//! captive portal's HTML where JSON was promised — which is exactly the layer
//! such a crate hides, and it keeps the dependency tree as it is.
//!
//! Two rules hold throughout. Every server binds an **ephemeral** port on
//! `127.0.0.1`, so nothing here reaches a network or collides with another
//! test. And every wait is **bounded** by [`within`]: the failure this file
//! exists to catch — a client that never sends, a server that never answers —
//! presents as a hang, and a hang must be a red test rather than a slow one.

use crate::{ApiClient, ApiError};
use riabuild_ui::Failure;
use std::future::Future;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The version these tests claim to be. Any value that `org::version_only`
/// would accept; what matters is that the header carries *this* one.
const VERSION: &str = "2026.8.19";

/// Long enough that a loaded machine does not fail spuriously, short enough
/// that a wedged test is reported rather than waited on. `ApiClient`'s own
/// timeout is thirty seconds, so this fires first and names what happened.
const BOUND: Duration = Duration::from_secs(10);

async fn within<T>(work: impl Future<Output = T>) -> T {
    match tokio::time::timeout(BOUND, work).await {
        Ok(value) => value,
        Err(_) => {
            panic!("nothing finished in {BOUND:?} against a server on loopback — this is hung")
        }
    }
}

/* -------------------------------------------------------------------------- */
/* The server                                                                  */
/* -------------------------------------------------------------------------- */

/// The bytes the client put on the wire, kept whole so a test can assert on
/// the request line, a header, or the body without the server having parsed
/// anything on its behalf.
struct Recorded(String);

impl Recorded {
    fn request_line(&self) -> &str {
        self.0.lines().next().unwrap_or_default()
    }

    fn head(&self) -> &str {
        match self.0.split_once("\r\n\r\n") {
            Some((head, _)) => head,
            None => &self.0,
        }
    }

    /// Case-insensitively, because a header name is. A test that matched
    /// `X-Riabuild-Cli-Version` exactly would pass or fail on which HTTP
    /// version `reqwest` negotiated rather than on anything riabuild does.
    fn header(&self, name: &str) -> Option<&str> {
        self.head().lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn body(&self) -> &str {
        match self.0.split_once("\r\n\r\n") {
            Some((_, body)) => body,
            None => "",
        }
    }
}

/// A loopback server that answers exactly one request with `reply`, and hands
/// the test what the client sent.
struct Loopback {
    url: String,
    served: JoinHandle<Option<Recorded>>,
}

impl Loopback {
    async fn request(self) -> Recorded {
        within(self.served)
            .await
            .expect("the server task must not panic")
            .expect("the client never sent a request")
    }
}

async fn serve_once(reply: String) -> Loopback {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback, ephemeral port");
    let address = listener
        .local_addr()
        .expect("a bound socket has an address");
    let served = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.ok()?;
        let request = read_request(&mut socket).await;
        // The record is returned whatever the writes do: a client that hung up
        // early is still a request worth asserting on.
        let _ = socket.write_all(reply.as_bytes()).await;
        let _ = socket.flush().await;
        // A FIN rather than the lingering read `riabuild-fetch`'s server does.
        // `reqwest` pools connections, so waiting for the client to hang up
        // would be waiting for the test process to end.
        let _ = socket.shutdown().await;
        Some(Recorded(request))
    });
    Loopback {
        url: format!("http://{address}"),
        served,
    }
}

/// Reads one request whole — the headers, then exactly the body its
/// `content-length` promised.
///
/// A single `read` is not enough. A POST arrives as at least two writes, and a
/// test asserting on a body that had not turned up yet would pass for the
/// wrong reason: it would be asserting that the client sent nothing.
async fn read_request(socket: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(head) = head_end(&buffer)
            && buffer.len() >= head + content_length(&buffer[..head])
        {
            break;
        }
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Where the body starts, or `None` while the headers are still arriving.
fn head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

fn content_length(head: &[u8]) -> usize {
    let head = String::from_utf8_lossy(head);
    for line in head.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("content-length") {
            return value.trim().parse().unwrap_or(0);
        }
    }
    0
}

/* -------------------------------------------------------------------------- */
/* The replies riabuild-web really sends                                       */
/* -------------------------------------------------------------------------- */

/// What `convex/lib/responses.ts`'s `jsonResponse` puts on the wire.
fn reply(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncache-control: no-store\r\n\
         content-length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The four-field envelope `apiError` sends, which is the whole of what the
/// CLI is allowed to know about why a request failed.
fn error_reply(status: &str, code: &str, message: &str, action: &str) -> String {
    reply(
        status,
        &serde_json::json!({ "error": { "code": code, "message": message, "action": action } })
            .to_string(),
    )
}

/// Something other than JSON, with an honest length — a proxy, a captive
/// portal, or a Convex deployment with no such route.
fn foreign_reply(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// `GET /api/v1/org/config`, field for field as `convex/http.ts` returns it —
/// including `defaultProjectPath`, the retired field the CLI must go on
/// ignoring rather than tripping over.
const ORG_CONFIG: &str = r#"{"repoSlug":"Clubria/ai-builders-hub",
    "defaultProjectPath":"~/code/ai-builders-hub","minCliVersion":"2026.8.4",
    "latestCliVersion":"2026.8.19","secretsUpdatedAt":1755000000,
    "secretEnvironments":["dev","staging"],"ngrokAuthTokenUpdatedAt":1755100000}"#;

/// `GET /api/v1/me`, wrapping `memberPayload` — which carries `githubId` and
/// `joinedAt` that `Member` does not name.
const ME: &str = r#"{"member":{"memberId":"550e8400-e29b-41d4-a716-446655440000",
    "githubLogin":"ada","githubId":"1234567","firstName":"Ada","lastName":"Lovelace",
    "email":"ada@clubria.dev","role":"developer","status":"active","joinedAt":1754000000000}}"#;

/// `POST /api/v1/cli/sessions`, which also carries the whole `member` payload
/// that `ServerSessionReply` deliberately does not read.
const SERVER_SESSION: &str = r#"{"token":"riab_server_token","sessionId":"k9700abcdef",
    "expiresAt":1762000000000,"member":{"memberId":"550e8400-e29b-41d4-a716-446655440000",
    "githubLogin":"ada","githubId":"1234567","firstName":"Ada","lastName":"Lovelace",
    "email":"ada@clubria.dev","role":"developer","status":"active","joinedAt":1754000000000}}"#;

fn client_for(server: &Loopback) -> ApiClient {
    ApiClient::pointed_at(VERSION, &server.url)
}

fn signed_in_client_for(server: &Loopback) -> ApiClient {
    let mut api = client_for(server);
    api.set_token(Some("riab_live_token".into()));
    api
}

fn as_api_error(error: &anyhow::Error) -> &ApiError {
    match error.downcast_ref::<ApiError>() {
        Some(api_error) => api_error,
        None => panic!("expected an ApiError, got {error:#}"),
    }
}

/* -------------------------------------------------------------------------- */
/* What the client sends                                                       */
/* -------------------------------------------------------------------------- */

#[tokio::test]
async fn a_request_names_its_route_and_says_which_riabuild_is_asking() {
    // `x-riabuild-cli-version` is not decoration. `guard`'s `enforceMinVersion`
    // reads an absent header as version `0`, so a request that omits it is
    // refused with a 409 by any team that has set a floor — which makes
    // dropping the header a total outage rather than a degraded one, and makes
    // it invisible to every test that goes through a fake.
    let server = serve_once(reply("200 OK", ORG_CONFIG)).await;
    let api = client_for(&server);

    within(crate::org::fetch_config(&api))
        .await
        .expect("a real /org/config payload should decode");

    let request = server.request().await;
    assert_eq!(request.request_line(), "GET /api/v1/org/config HTTP/1.1");
    assert_eq!(request.header("x-riabuild-cli-version"), Some(VERSION));
    assert_eq!(request.header("user-agent"), Some("riabuild/2026.8.19"));
    assert!(request.header("host").is_some(), "{}", request.head());
    // Nothing to send, so nothing sent — rather than an empty `Bearer`, which
    // riabuild-web's `bearerToken` would read as a malformed session.
    assert_eq!(request.header("authorization"), None);
}

#[tokio::test]
async fn an_authenticated_route_carries_the_session_token_as_a_bearer() {
    let server = serve_once(reply("200 OK", ME)).await;
    let api = signed_in_client_for(&server);

    let member = within(api.me()).await.expect("a real /me payload decodes");
    assert_eq!(member.member_id, "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(member.display_name(), "Ada Lovelace");

    let request = server.request().await;
    assert_eq!(request.request_line(), "GET /api/v1/me HTTP/1.1");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer riab_live_token")
    );
    // The floor applies to an authenticated route too — `/me` is guarded with
    // `version: true`.
    assert_eq!(request.header("x-riabuild-cli-version"), Some(VERSION));
}

#[tokio::test]
async fn signing_a_server_in_posts_its_label_and_reads_the_session_back() {
    let server = serve_once(reply("200 OK", SERVER_SESSION)).await;
    let api = signed_in_client_for(&server);

    let session = within(crate::auth::for_server(&api, "gpu-box"))
        .await
        .expect("a real /cli/sessions payload decodes");
    assert_eq!(session.token, "riab_server_token");
    assert_eq!(session.session_id, "k9700abcdef");
    // Milliseconds, the server's own reckoning — not a TTL computed here.
    assert_eq!(session.expires_at, 1_762_000_000_000);

    let request = server.request().await;
    assert_eq!(request.request_line(), "POST /api/v1/cli/sessions HTTP/1.1");
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.body(), r#"{"deviceLabel":"gpu-box"}"#);
    assert_eq!(
        request.header("authorization"),
        Some("Bearer riab_live_token")
    );
}

#[tokio::test]
async fn brokering_a_credential_posts_an_empty_object_and_reads_the_environments() {
    let server = serve_once(reply(
        "200 OK",
        r#"{"token":"st.abc","expiresAt":1755000060000,"projectId":"proj_1",
            "environment":"dev","environments":["dev","staging"],"secretPath":"/",
            "siteUrl":"https://app.infisical.com","secretsUpdatedAt":1755000000}"#,
    ))
    .await;
    let api = signed_in_client_for(&server);

    let brokered = within(crate::secrets::broker(&api))
        .await
        .expect("a real /secrets/token payload decodes");
    assert_eq!(brokered.token, "st.abc");
    assert_eq!(brokered.project_id, "proj_1");
    assert_eq!(brokered.environments, ["dev", "staging"]);
    assert_eq!(brokered.secret_path, "/");

    let request = server.request().await;
    assert_eq!(
        request.request_line(),
        "POST /api/v1/secrets/token HTTP/1.1"
    );
    // `post_json` always sends a body, so the empty one still has to be JSON
    // the server's `req.json()` can read rather than nothing at all.
    assert_eq!(request.body(), "{}");
}

/* -------------------------------------------------------------------------- */
/* What the client makes of what comes back                                    */
/* -------------------------------------------------------------------------- */

#[tokio::test]
async fn a_real_org_config_decodes_into_the_fields_provisioning_reads() {
    let server = serve_once(reply("200 OK", ORG_CONFIG)).await;
    let api = client_for(&server);

    let config = within(crate::org::fetch_config(&api))
        .await
        .expect("a real /org/config payload should decode");
    assert_eq!(
        config
            .default_repo()
            .expect("the dashboard's slug is usable")
            .slug(),
        "Clubria/ai-builders-hub"
    );
    assert_eq!(config.min_cli_version, "2026.8.4");
    assert_eq!(config.latest_cli_version, "2026.8.19");
    assert_eq!(config.secret_environments, ["dev", "staging"]);
    assert!(config.has_ngrok_authtoken());
}

#[tokio::test]
async fn the_teams_servers_arrive_as_addresses_and_nothing_else() {
    let server = serve_once(reply(
        "200 OK",
        r#"{"servers":[{"id":"k17abc","name":"gpu","host":"gpu.clubria.dev",
            "port":22,"user":"clubria"}]}"#,
    ))
    .await;
    let api = signed_in_client_for(&server);

    let fetched = within(crate::remotes::fetch_shared(&api))
        .await
        .expect("a real /remotes/shared payload decodes");
    assert!(fetched.refused.is_empty(), "{:?}", fetched.refused);
    assert_eq!(fetched.servers.len(), 1);
    assert_eq!(fetched.servers[0].id, "k17abc");
    assert_eq!(fetched.servers[0].host, "gpu.clubria.dev");
    assert_eq!(fetched.servers[0].port, 22);
}

/* -------------------------------------------------------------------------- */
/* Status mapping                                                              */
/* -------------------------------------------------------------------------- */

#[tokio::test]
async fn only_a_401_is_a_session_that_signing_in_again_would_fix() {
    // `remote::session::usable_token` re-mints a server's session on
    // `needs_login()` and keeps the saved one on anything else. A 409 or a 503
    // read as "sign in again" would mint a second ninety-day session and leave
    // the first one live and unrevocable; a 401 read as anything else would
    // hand the server a token riabuild-web has already thrown away.
    let cases = [
        ("401 Unauthorized", "session_expired", 401, true),
        ("401 Unauthorized", "session_revoked", 401, true),
        ("403 Forbidden", "not_org_member", 403, false),
        ("409 Conflict", "cli_too_old", 409, false),
        (
            "503 Service Unavailable",
            "org_check_unavailable",
            503,
            false,
        ),
    ];

    for (status, code, expected_status, expected_needs_login) in cases {
        let server = serve_once(error_reply(
            status,
            code,
            "riabuild cannot do that.",
            "Do this instead.",
        ))
        .await;
        let api = signed_in_client_for(&server);

        let error = within(api.me()).await.expect_err(status);
        let api_error = as_api_error(&error);
        assert_eq!(api_error.code, code);
        // Filled in from the response rather than the body: `apiError` does not
        // put the status inside the envelope.
        assert_eq!(api_error.status, expected_status, "{status}");
        assert_eq!(api_error.needs_login(), expected_needs_login, "{status}");
        // Printed verbatim — the server knows why, the CLI does not.
        assert_eq!(api_error.message, "riabuild cannot do that.");
        assert_eq!(api_error.action, "Do this instead.");
    }
}

#[tokio::test]
async fn a_gateway_that_sends_no_envelope_is_an_outage_rather_than_a_login_problem() {
    // A 5xx from something in front of Convex — a proxy, a load balancer — has
    // no `error` object to read, so the CLI supplies a shape of its own rather
    // than failing to deserialize the failure.
    let server = serve_once(foreign_reply(
        "502 Bad Gateway",
        "text/html",
        "<html><body>502 Bad Gateway</body></html>",
    ))
    .await;
    let api = signed_in_client_for(&server);

    let error = within(api.me()).await.expect_err("502");
    let api_error = as_api_error(&error);
    assert_eq!(api_error.code, "upstream_error");
    assert_eq!(api_error.status, 502);
    assert!(!api_error.needs_login());
    assert!(api_error.message.contains("502"), "{api_error}");
    // Not the poll loop's business either: only a failure to reach the server
    // at all keeps a fifteen-minute device code alive.
    assert!(!crate::auth::survives_a_failed_poll(&error));
}

#[tokio::test]
async fn a_server_that_is_not_there_is_unreachable_rather_than_a_bug_in_riabuild() {
    // Port 1 on loopback, which nothing is listening on. This is the one
    // failure the device-code poll survives, so the code has to be exactly
    // `unreachable` — `survives_a_failed_poll` matches nothing else.
    let api = ApiClient::pointed_at(VERSION, "http://127.0.0.1:1");

    let error = within(crate::org::fetch_config(&api))
        .await
        .expect_err("nothing is listening there");
    let api_error = as_api_error(&error);
    assert_eq!(api_error.code, "unreachable");
    assert_eq!(api_error.status, 0);
    assert!(!api_error.needs_login());
    assert!(crate::auth::survives_a_failed_poll(&error));
}

#[tokio::test]
async fn a_dashboard_with_no_delegation_route_is_named_rather_than_read_as_an_outage() {
    // A Convex deployment older than this binary answers with its own 404 and
    // no envelope, which `interpret` reports as "replied with HTTP 404" — an
    // outage, to read it. The cause is a deploy that has not happened, and
    // `explain_a_dashboard_that_cannot_delegate` is the one place that is
    // turned into a sentence a developer can act on.
    let server = serve_once(foreign_reply("404 Not Found", "text/plain", "Not Found")).await;
    let api = signed_in_client_for(&server);

    let error = within(crate::auth::for_server(&api, "gpu-box"))
        .await
        .expect_err("no such route");
    let failure = error
        .downcast_ref::<Failure>()
        .unwrap_or_else(|| panic!("a 404 here is a deploy, not an outage: {error:#}"));
    assert!(failure.action.contains("deploy riabuild-web"), "{failure}");
    assert!(
        failure.detail.contains("older than this riabuild"),
        "{failure}"
    );
}

#[tokio::test]
async fn a_404_that_the_server_did_explain_keeps_the_servers_own_words() {
    // The other half of the rewrite above: only a bare 404 is reworded. A 404
    // carrying an envelope is riabuild-web explaining itself, and it explains
    // better than this side could — `/org/ngrok-token` answers this way when
    // no lead has set a token.
    let server = serve_once(error_reply(
        "404 Not Found",
        "not_configured",
        "Your team has not set an ngrok authtoken yet.",
        "Ask your team lead to add one in the riabuild dashboard, under org settings.",
    ))
    .await;
    let api = signed_in_client_for(&server);

    let error = within(crate::ngrok::fetch_authtoken(&api))
        .await
        .expect_err("no token is set");
    let api_error = as_api_error(&error);
    assert_eq!(api_error.code, "not_configured");
    assert_eq!(api_error.status, 404);
    assert!(!api_error.needs_login());
}

/* -------------------------------------------------------------------------- */
/* Bodies that are not what they claim to be                                   */
/* -------------------------------------------------------------------------- */

#[tokio::test]
async fn a_reply_that_stops_part_way_through_names_the_route_rather_than_panicking() {
    // A `content-length` promising far more than arrives, and then a hang-up:
    // a dropped link, or a proxy that cut the connection mid-body. The read
    // has to fail as an error naming where it was reading from, not unwind.
    let server = serve_once(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 4096\r\n\r\n\
         {\"repoSlug\":\"Clubria/ai-"
            .to_string(),
    )
    .await;
    let api = client_for(&server);

    let error = within(crate::org::fetch_config(&api))
        .await
        .expect_err("half a body is not a config");
    let message = format!("{error:#}");
    assert!(
        message.contains("could not read the reply from /api/v1/org/config"),
        "{message}"
    );
}

#[tokio::test]
async fn a_captive_portal_answering_with_html_is_an_error_rather_than_a_panic() {
    // HTTP 200 and not a byte of JSON — the hotel wifi, or a corporate proxy
    // interposing a sign-in page. `interpret` only checks the status, so this
    // reaches `json()` as though it were a real reply.
    let server = serve_once(foreign_reply(
        "200 OK",
        "text/html",
        "<html><head><title>Sign in to the wifi</title></head></html>",
    ))
    .await;
    let api = client_for(&server);

    let error = within(crate::org::fetch_config(&api))
        .await
        .expect_err("HTML is not a config");
    let message = format!("{error:#}");
    assert!(
        message.contains("could not read the reply from /api/v1/org/config"),
        "{message}"
    );
    // A malformed body is not weather: retrying it would hide it.
    assert!(!crate::auth::survives_a_failed_poll(&error));
}

#[tokio::test]
async fn a_dashboard_older_than_this_binary_is_a_stale_deploy_and_not_an_expired_session() {
    // The decode-vs-auth split, over the wire this time rather than through
    // `decode_member` alone: a 200 whose `member` has no `memberId`. Reported
    // as a stale dashboard, and — the part that matters — *not* as something
    // `needs_login()` would send the developer back through a browser for.
    let server = serve_once(reply(
        "200 OK",
        r#"{"member":{"githubLogin":"ada","githubId":"1234567","firstName":"Ada",
            "lastName":"Lovelace","email":"ada@clubria.dev","role":"developer",
            "status":"active","joinedAt":1754000000000}}"#,
    ))
    .await;
    let api = signed_in_client_for(&server);

    let error = within(api.me()).await.expect_err("no memberId");
    let failure = error
        .downcast_ref::<Failure>()
        .unwrap_or_else(|| panic!("a decode failure must be a Failure: {error:#}"));
    assert!(failure.action.contains("deploy the dashboard"), "{failure}");
    assert!(
        error.downcast_ref::<ApiError>().is_none(),
        "a decode failure must never look like a session problem"
    );
}

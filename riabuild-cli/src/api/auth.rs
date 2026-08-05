//! CLI login — loopback OAuth, the same shape `gh` uses.
//!
//! The CLI binds an ephemeral port on 127.0.0.1, sends the developer to the
//! dashboard, and the dashboard redirects the browser back to that port with a
//! one-time code. Chosen over device-code because the target is a desktop with a
//! browser, and because it puts the developer back in their terminal.
//!
//! `state` is generated here and verified here, so a callback riabuild did not
//! start is rejected. The verifier never leaves this process until it is
//! exchanged over TLS.

use crate::api::{ApiClient, Member};
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);

pub struct LoginFlow {
    pub state: String,
    pub verifier: String,
    pub challenge: String,
}

impl Default for LoginFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginFlow {
    pub fn new() -> Self {
        let state = random_b64(32);
        let verifier = random_b64(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            state,
            verifier,
            challenge,
        }
    }

    pub fn authorize_url(&self, web_url: &str, port: u16, label: &str, version: &str) -> String {
        format!(
            "{web_url}/cli/authorize?state={}&challenge={}&port={port}&label={}&version={}",
            urlencode(&self.state),
            urlencode(&self.challenge),
            urlencode(label),
            urlencode(version),
        )
    }
}

fn random_b64(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Pulls `code` and `state` out of a callback request line.
///
/// Pure, so the rejection rules are unit-testable without a socket.
pub fn parse_callback(request_line: &str) -> Option<(String, String)> {
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?')?;
    if path != "/callback" {
        return None;
    }

    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "code" => code = Some(urldecode(value)),
            "state" => state = Some(urldecode(value)),
            _ => {}
        }
    }
    Some((code?, state?))
}

fn urldecode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Errors are swallowed on purpose: a browser that hangs up before reading the
/// courtesy page must not fail a login that has already succeeded.
async fn respond(stream: &mut TcpStream, title: &str, detail: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>riabuild</title>\
         <body style=\"font:16px/1.5 -apple-system,system-ui,sans-serif;padding:3rem\">\
         <h1 style=\"font-size:1.4rem\">{title}</h1><p>{detail}</p></body>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

/// Waits for the browser to come back. Rejects any callback whose `state` is not
/// the one this process generated.
async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    // The timeout wraps the whole accept loop rather than a single accept:
    // favicon requests and stray probes `continue`, and a per-accept timeout
    // would hand each of them a fresh three-minute budget.
    let wait = async {
        loop {
            let (mut stream, _) = listener.accept().await?;

            let mut line = String::new();
            let mut reader = BufReader::new(&mut stream);
            // One connection that opens and then says nothing must not park a
            // real callback behind it. The outer timeout would eventually fire,
            // but this keeps a stalled probe from consuming the whole window.
            if timeout(Duration::from_secs(5), reader.read_line(&mut line))
                .await
                .is_err()
            {
                continue;
            }

            match parse_callback(line.trim_end()) {
                Some((code, state)) if state == expected_state => {
                    respond(
                        &mut stream,
                        "You are signed in.",
                        "riabuild has what it needs. You can close this tab.",
                    )
                    .await;
                    return Ok(code);
                }
                Some(_) => {
                    respond(
                        &mut stream,
                        "That did not come from riabuild.",
                        "The sign-in was not the one this terminal started. Run <code>riabuild login</code> again.",
                    )
                    .await;
                    return Err(anyhow!(
                        "the browser came back with a sign-in riabuild did not start"
                    ));
                }
                None => {
                    // Favicon requests and stray probes land here.
                    respond(&mut stream, "riabuild", "Nothing to do here.").await;
                }
            }
        }
    };

    timeout(LOGIN_TIMEOUT, wait)
        .await
        .map_err(|_| anyhow!("no reply from the browser within three minutes"))?
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

#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
    member: Member,
}

/// Runs the whole flow and returns the session token. The caller stores it in
/// the keychain; it is never written to `~/.riabuild`.
pub async fn login(
    api: &ApiClient,
    runner: &dyn CommandRunner,
    ui: &Ui,
    web_url: &str,
    version: &str,
) -> Result<(String, Member)> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|error| {
        Failure::new(
            "opening a local port for the browser to come back to",
            "Check whether something is blocking loopback connections, then run `riabuild login` again.",
        )
        .detail(error.to_string())
    })?;
    let port = listener.local_addr()?.port();

    let flow = LoginFlow::new();
    let label = device_label(runner).await;
    let url = flow.authorize_url(web_url, port, &label, version);

    ui.heading("Signing this machine in to riabuild");
    if !open_browser(runner, &url).await {
        ui.note("Could not open your browser. Open this link yourself:");
    }
    ui.note(&url);

    let code = wait_for_code(&listener, &flow.state)
        .await
        .map_err(|error| {
            Failure::new(
                "waiting for you to approve this machine in the browser",
                "Run `riabuild login` again and approve the request.",
            )
            .detail(error.to_string())
        })?;

    let response: TokenResponse = api
        .post_json(
            "/api/v1/cli/token",
            serde_json::json!({ "code": code, "verifier": flow.verifier }),
        )
        .await?;

    Ok((response.token, response.member))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_callback() {
        let parsed = parse_callback("GET /callback?code=abc123&state=xyz HTTP/1.1");
        assert_eq!(parsed, Some(("abc123".into(), "xyz".into())));
    }

    #[test]
    fn decodes_percent_escapes() {
        let parsed = parse_callback("GET /callback?code=a%2Bb%3Dc&state=s%2F1 HTTP/1.1").unwrap();
        assert_eq!(parsed.0, "a+b=c");
        assert_eq!(parsed.1, "s/1");
    }

    #[test]
    fn ignores_anything_that_is_not_the_callback() {
        assert!(parse_callback("GET /favicon.ico HTTP/1.1").is_none());
        assert!(parse_callback("POST /callback?code=a&state=b HTTP/1.1").is_none());
        assert!(parse_callback("GET /callback HTTP/1.1").is_none());
        assert!(parse_callback("garbage").is_none());
    }

    #[test]
    fn a_challenge_is_the_sha256_of_the_verifier() {
        let flow = LoginFlow::new();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(flow.verifier.as_bytes()));
        assert_eq!(flow.challenge, expected);
        assert_ne!(flow.state, flow.verifier);
        // Long enough that the dashboard's own length checks pass.
        assert!(flow.state.len() >= 16);
        assert!(flow.challenge.len() >= 32);
    }

    #[test]
    fn each_login_is_unique() {
        let a = LoginFlow::new();
        let b = LoginFlow::new();
        assert_ne!(a.state, b.state);
        assert_ne!(a.verifier, b.verifier);
    }

    #[test]
    fn the_authorize_url_carries_everything_the_dashboard_needs() {
        let flow = LoginFlow::new();
        let url = flow.authorize_url("https://riabuild.clubria.com", 51234, "Ada's MBP", "0.1.0");
        assert!(url.starts_with("https://riabuild.clubria.com/cli/authorize?"));
        assert!(url.contains("port=51234"));
        assert!(url.contains("label=Ada%27s+MBP"));
        assert!(url.contains(&format!("challenge={}", urlencode(&flow.challenge))));
        // The verifier is the one thing that must not appear in a URL.
        assert!(!url.contains(&flow.verifier));
    }
}

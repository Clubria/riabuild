//! The server side: connect, ask once, read once.
//!
//! Everything here is in the paste path, so the contract is that it never
//! hangs. A laptop that has closed its lid must produce a fast, clean failure —
//! the alternative is Claude Code stopping dead on Ctrl+V, which reads as the
//! editor being broken rather than the channel being down.

use crate::protocol::{Request, Response, decode_response, encode_request};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// The socket is local — a forwarded one either answers immediately or is not
/// there at all — so this only has to cover scheduling, not a network.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Covers the round trip to the laptop and the transfer. Generous, because a
/// 15 MB screenshot over a hotel connection is a legitimate slow case.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub struct Reply {
    pub response: Response,
    pub body: Vec<u8>,
}

pub async fn request(socket: &Path, request: &Request) -> Result<Reply> {
    request_with_body(socket, request, &[]).await
}

/// A request that carries a payload — today only `clipboard.write`.
///
/// The body goes out on the same connection, straight after the header line,
/// framed by the length the header announced.
pub async fn request_with_body(socket: &Path, request: &Request, body: &[u8]) -> Result<Reply> {
    let connect = UnixStream::connect(socket);
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .with_context(|| {
            format!(
                "the laptop channel at {} did not accept a connection",
                socket.display()
            )
        })?
        .with_context(|| {
            format!(
                "the laptop channel at {} is not available",
                socket.display()
            )
        })?;

    tokio::time::timeout(REQUEST_TIMEOUT, exchange(stream, request, body))
        .await
        .context("the laptop channel did not answer in time")?
}

async fn exchange(mut stream: UnixStream, request: &Request, body: &[u8]) -> Result<Reply> {
    stream
        .write_all(encode_request(request).as_bytes())
        .await
        .context("could not send the request to the laptop channel")?;
    if !body.is_empty() {
        stream
            .write_all(body)
            .await
            .context("could not send the payload to the laptop channel")?;
    }
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("the laptop channel closed before replying")?;
    if line.trim().is_empty() {
        bail!("the laptop channel replied with nothing");
    }

    let response = decode_response(&line)?;

    let body = match &response {
        Response::Payload { len } => {
            // Exactly the announced length, never "until close": a short read
            // here is a truncated screenshot that Claude Code would accept.
            let mut buffer = vec![0u8; *len];
            reader
                .read_exact(&mut buffer)
                .await
                .context("the laptop channel sent fewer bytes than it announced")?;
            buffer
        }
        _ => Vec::new(),
    };

    Ok(Reply { response, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ErrorCode, encode_response};

    /// A scripted agent: one canned reply per connection.
    fn serve(socket: &Path, header: Response, body: &'static [u8]) {
        let listener = tokio::net::UnixListener::bind(socket).expect("bind");
        let header = encode_response(&header);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                let _ = reader.read_line(&mut line).await;
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.flush().await;
            }
        });
    }

    #[tokio::test]
    async fn a_targets_request_returns_the_list() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(&socket, Response::Targets(vec!["image/png".into()]), b"");

        let reply = request(&socket, &Request::ClipboardTargets)
            .await
            .expect("request");
        assert_eq!(
            reply.response,
            Response::Targets(vec!["image/png".to_string()])
        );
        assert!(reply.body.is_empty());
    }

    /// The length prefix is a contract: read exactly that many bytes, not
    /// "until the peer closes". A short read here is a truncated screenshot.
    #[tokio::test]
    async fn a_payload_reply_reads_exactly_the_announced_bytes() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(
            &socket,
            Response::Payload { len: 4 },
            b"\x89PNGtrailing junk",
        );

        let reply = request(
            &socket,
            &Request::ClipboardRead {
                mime: "image/png".into(),
            },
        )
        .await
        .expect("request");
        assert_eq!(reply.body, b"\x89PNG");
    }

    #[tokio::test]
    async fn an_error_reply_is_returned_rather_than_raised() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(
            &socket,
            Response::Error {
                code: ErrorCode::Unavailable,
                message: "no clipboard content of that type".into(),
            },
            b"",
        );

        let reply = request(&socket, &Request::ClipboardTargets)
            .await
            .expect("request");
        assert!(matches!(reply.response, Response::Error { .. }));
    }

    /// The laptop is gone. This must fail fast and legibly, because the
    /// alternative is Claude Code hanging on Ctrl+V.
    #[tokio::test]
    async fn a_missing_socket_is_an_error_not_a_hang() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let error = request(&dir.path().join("absent.sock"), &Request::ChannelPing)
            .await
            .expect_err("should fail");
        assert!(
            error.to_string().contains("channel"),
            "{error} does not mention the channel"
        );
    }

    /// A truncated body must not be returned as if it were complete: a
    /// half-written PNG that Claude Code accepts is worse than a clean miss.
    #[tokio::test]
    async fn a_body_shorter_than_its_header_promised_is_an_error() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(&socket, Response::Payload { len: 64 }, b"short");

        let result = request(
            &socket,
            &Request::ClipboardRead {
                mime: "image/png".into(),
            },
        )
        .await;
        assert!(result.is_err(), "a short body was accepted");
    }
}

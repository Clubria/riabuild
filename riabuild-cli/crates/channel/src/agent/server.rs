//! Carrying the agent's answers over a unix socket.
//!
//! One connection carries one request and one response. The socket is
//! request-scoped rather than session-scoped so a wedged reader cannot hold the
//! channel, and so the supervisor's ping is a real end-to-end probe rather than
//! a check on a socket that is merely still open.

use super::Agent;
use crate::line;
use crate::protocol::{ErrorCode, Request, Response, decode_request, encode_response};
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

impl Agent {
    /// Accepts connections until cancelled. One request per connection.
    pub async fn serve(self: Arc<Self>, socket: &Path) -> Result<()> {
        if let Some(parent) = socket.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        // A socket left by a killed agent blocks the bind, and the channel comes
        // up permanently dead. This is our own end, on the laptop; the server
        // end is where a socket owned by another uid is refused.
        let _ = tokio::fs::remove_file(socket).await;

        let listener = UnixListener::bind(socket)
            .with_context(|| format!("could not listen on {}", socket.display()))?;

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .context("the channel socket stopped accepting connections")?;
            let agent = Arc::clone(&self);
            // Serving inline would let one slow clipboard read block every other
            // shell into the same server.
            tokio::spawn(async move {
                let _ = agent.serve_one(stream).await;
            });
        }
    }

    async fn serve_one(&self, stream: UnixStream) -> Result<()> {
        let mut reader = BufReader::new(stream);
        // Bounded, the same way `mux` and `client` bound theirs. `decode_request`
        // refuses a body over the cap, and until this existed nothing refused a
        // *header* that never ended — so a connection to this socket could make
        // the laptop's own agent allocate without limit.
        let line = line::read_header_line(&mut reader)
            .await?
            .unwrap_or_default();

        let (header, body) = match decode_request(&line) {
            Ok(request) => {
                // A write is the only request with a body. Read exactly what
                // the header announced — `decode_request` has already refused
                // anything past the cap, so this cannot be told to allocate
                // more than the channel carries.
                let inbound = match &request {
                    Request::ClipboardWrite { len, .. } => {
                        let mut bytes = vec![0u8; *len];
                        match reader.read_exact(&mut bytes).await {
                            Ok(_) => Some(bytes),
                            // A truncated body is a framing failure, not an
                            // empty clipboard: answer rather than write half an
                            // image onto the laptop.
                            Err(_) => None,
                        }
                    }
                    _ => None,
                };
                self.handle(&request, inbound, Instant::now()).await
            }
            // A line the allowlist refuses is answered, not dropped: the next
            // shell into the same server still needs this agent.
            Err(error) => (
                Response::Error {
                    code: ErrorCode::BadRequest,
                    message: error.to_string(),
                },
                None,
            ),
        };

        let stream = reader.get_mut();
        stream
            .write_all(encode_response(&header).as_bytes())
            .await?;
        if let Some(bytes) = body {
            stream.write_all(&bytes).await?;
        }
        stream.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::agent_holding;
    use crate::mime::TEXT;
    use crate::protocol::{Request, decode_response, encode_request};
    use std::time::Duration;
    // The tests read replies as lines because a reply *is* one line; only the
    // production reader has an untrusted peer at the other end of it.
    use tokio::io::AsyncBufReadExt;

    /// Starts an agent and waits for the listener rather than sleeping a fixed
    /// interval.
    async fn started(socket: &Path) -> tokio::task::JoinHandle<Result<()>> {
        let agent = Arc::new(agent_holding(&[TEXT], b"hello"));
        let handle = tokio::spawn({
            let socket = socket.to_path_buf();
            async move { agent.serve(&socket).await }
        });
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle
    }

    /// End to end over a real socket, which is the only way to know the framing
    /// and the socket layer agree.
    #[tokio::test]
    async fn the_agent_answers_over_a_real_unix_socket() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let serving = started(&socket).await;

        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("connect");
        stream
            .write_all(encode_request(&Request::ClipboardTargets).as_bytes())
            .await
            .expect("write");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        assert_eq!(
            decode_response(&line).expect("decode"),
            Response::Targets(vec![TEXT.to_string()])
        );

        serving.abort();
    }

    /// An unparseable line must not take the agent down: the next shell into the
    /// same server still needs it.
    #[tokio::test]
    async fn a_malformed_request_is_answered_rather_than_fatal() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let serving = started(&socket).await;

        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("connect");
        stream.write_all(b"not json\n").await.expect("write");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        assert!(line.contains("bad_request"), "{line}");

        // Still serving.
        let mut second = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("second connect");
        second
            .write_all(encode_request(&Request::ChannelPing).as_bytes())
            .await
            .expect("write");
        let mut reader = BufReader::new(second);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        assert_eq!(decode_response(&line).expect("decode"), Response::Pong);

        serving.abort();
    }

    /// A write, body and all, over a real socket. The framing is the whole risk
    /// here: the body is not newline-delimited, so a reader that guessed at its
    /// length would corrupt every binary copy.
    #[tokio::test]
    async fn a_write_carries_its_body_over_a_real_unix_socket() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let serving = started(&socket).await;

        // A body with an embedded newline and a non-UTF-8 byte: either one
        // would be mangled by a reader that framed on lines or on strings.
        let payload = b"first\nsecond\xFF".to_vec();
        let request = Request::ClipboardWrite {
            mime: TEXT.to_string(),
            len: payload.len(),
        };

        let reply = crate::client::request_with_body(&socket, &request, &payload)
            .await
            .expect("write");
        assert_eq!(reply.response, Response::Written);

        // The agent's clipboard now holds exactly those bytes.
        let read = crate::client::request(
            &socket,
            &Request::ClipboardRead {
                mime: TEXT.to_string(),
            },
        )
        .await
        .expect("read back");
        assert_eq!(read.body, payload);

        serving.abort();
    }

    /// The connection stays usable after a write: the body must be consumed
    /// exactly, or the next request would be read out of the leftovers.
    #[tokio::test]
    async fn a_write_leaves_the_agent_answering() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let serving = started(&socket).await;

        let payload = b"content".to_vec();
        let request = Request::ClipboardWrite {
            mime: TEXT.to_string(),
            len: payload.len(),
        };
        crate::client::request_with_body(&socket, &request, &payload)
            .await
            .expect("write");

        assert!(pings(&socket).await, "the agent stopped answering");
        serving.abort();
    }

    /// One round trip, or `false` if the agent is not answering yet.
    async fn pings(socket: &Path) -> bool {
        let Ok(mut stream) = tokio::net::UnixStream::connect(socket).await else {
            return false;
        };
        if stream
            .write_all(encode_request(&Request::ChannelPing).as_bytes())
            .await
            .is_err()
        {
            return false;
        }
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).await.is_err() {
            return false;
        }
        decode_response(&line).ok() == Some(Response::Pong)
    }

    /// A socket left behind by a killed agent must not make the channel come up
    /// permanently dead — the failure `StreamLocalBindUnlink` exists to prevent
    /// on the server side, and `remove_file` on this one.
    #[tokio::test]
    async fn a_stale_socket_from_a_killed_agent_is_replaced() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");

        let first = started(&socket).await;
        assert!(pings(&socket).await, "the first agent never answered");
        first.abort();

        // The file outlives the abort, so waiting for it to exist proves
        // nothing here: poll for an agent that actually answers.
        let second = started(&socket).await;
        let mut answered = false;
        for _ in 0..100 {
            if pings(&socket).await {
                answered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            answered,
            "the replacement agent never bound the stale socket"
        );

        second.abort();
    }
}

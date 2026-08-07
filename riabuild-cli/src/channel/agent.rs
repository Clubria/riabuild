//! The laptop side: answer requests, decide what to serve.
//!
//! One connection carries one request and one response. The socket is
//! request-scoped rather than session-scoped so a wedged reader cannot hold the
//! channel, and so the supervisor's ping is a real end-to-end probe rather than
//! a check on a socket that is merely still open.

use crate::channel::clipboard::Clipboard;
use crate::channel::protocol::{
    ErrorCode, MAX_PAYLOAD, Request, Response, decode_request, encode_response,
};
use crate::channel::resize;
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

/// How long a `TARGETS` answer stays good for the read that follows it.
///
/// A paste is two round trips. Long enough to cover a slow link, short enough
/// that this is a snapshot for one paste rather than a cache of the clipboard.
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(5);

struct Snapshot {
    taken: Instant,
    types: Vec<String>,
    /// Filled lazily: `TARGETS` records what was advertised, and the read that
    /// follows stores the bytes it fetched under that advertisement.
    content: Vec<(String, Vec<u8>)>,
}

pub struct Agent {
    clipboard: Box<dyn Clipboard>,
    snapshot: Mutex<Option<Snapshot>>,
}

impl Agent {
    pub fn new(clipboard: Box<dyn Clipboard>) -> Self {
        Self {
            clipboard,
            snapshot: Mutex::new(None),
        }
    }

    /// Answers one request. The body is returned beside the header rather than
    /// written, so every dispatch decision is testable without a socket.
    pub async fn handle(&self, request: &Request, now: Instant) -> (Response, Option<Vec<u8>>) {
        match request {
            Request::ChannelPing => (Response::Pong, None),
            Request::ClipboardTargets => self.targets(now).await,
            Request::ClipboardRead { mime } => self.read(mime, now).await,
        }
    }

    async fn targets(&self, now: Instant) -> (Response, Option<Vec<u8>>) {
        let types = match self.clipboard.targets().await {
            Ok(types) => types,
            Err(error) => return (internal(error), None),
        };

        *self.snapshot.lock().await = Some(Snapshot {
            taken: now,
            types: types.clone(),
            content: Vec::new(),
        });

        (Response::Targets(types), None)
    }

    async fn read(&self, mime: &str, now: Instant) -> (Response, Option<Vec<u8>>) {
        let mut snapshot = self.snapshot.lock().await;

        // Expire first, so a stale snapshot never answers.
        if snapshot
            .as_ref()
            .is_some_and(|held| now.duration_since(held.taken) > SNAPSHOT_TTL)
        {
            *snapshot = None;
        }

        if let Some(held) = snapshot.as_ref()
            && let Some((_, bytes)) = held.content.iter().find(|(t, _)| t == mime)
        {
            return payload(mime, bytes.clone());
        }

        let fetched = match self.clipboard.read(mime).await {
            Ok(found) => found,
            Err(error) => return (internal(error), None),
        };

        let Some(bytes) = fetched else {
            // The clipboard moved between the advertisement and the read. The
            // caller is mid-paste, so say what happened rather than reporting
            // an empty clipboard it can do nothing with.
            let advertised = snapshot
                .as_ref()
                .is_some_and(|held| held.types.iter().any(|t| t == mime));
            let message = if advertised {
                format!("the clipboard changed while `{mime}` was being read")
            } else {
                "no clipboard content of that type".to_string()
            };
            return (
                Response::Error {
                    code: ErrorCode::Unavailable,
                    message,
                },
                None,
            );
        };

        let bytes = resize::to_ceiling(mime, bytes);

        if let Some(held) = snapshot.as_mut() {
            held.content.push((mime.to_string(), bytes.clone()));
        }

        payload(mime, bytes)
    }

    /// Accepts connections until cancelled. One request per connection.
    pub async fn serve(self: Arc<Self>, socket: &Path) -> Result<()> {
        if let Some(parent) = socket.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        // A socket left by a killed agent blocks the bind, and the channel
        // comes up permanently dead. This is our own end, on the laptop; the
        // server end is where a socket owned by another uid is refused.
        let _ = tokio::fs::remove_file(socket).await;

        let listener = UnixListener::bind(socket)
            .with_context(|| format!("could not listen on {}", socket.display()))?;

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .context("the channel socket stopped accepting connections")?;
            let agent = Arc::clone(&self);
            // Serving inline would let one slow clipboard read block every
            // other shell into the same server.
            tokio::spawn(async move {
                let _ = agent.serve_one(stream).await;
            });
        }
    }

    async fn serve_one(&self, stream: UnixStream) -> Result<()> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let (header, body) = match decode_request(&line) {
            Ok(request) => self.handle(&request, Instant::now()).await,
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

fn internal(error: anyhow::Error) -> Response {
    Response::Error {
        code: ErrorCode::Internal,
        message: format!("could not read the laptop's clipboard: {error}"),
    }
}

fn payload(mime: &str, bytes: Vec<u8>) -> (Response, Option<Vec<u8>>) {
    if bytes.len() > MAX_PAYLOAD {
        return (
            Response::Error {
                code: ErrorCode::TooLarge,
                message: format!(
                    "`{mime}` is {} bytes, over the {MAX_PAYLOAD} byte channel limit",
                    bytes.len()
                ),
            },
            None,
        );
    }
    (Response::Payload { len: bytes.len() }, Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::mime::{PNG, TEXT};
    // Only the socket test needs these, so they are imported here rather than
    // at module level, where they would be unused in the shipped build.
    use crate::channel::protocol::{decode_response, encode_request};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    /// A clipboard whose contents the test can change between calls, which is
    /// the whole point of the snapshot.
    struct FakeClipboard {
        types: StdMutex<Vec<String>>,
        bytes: StdMutex<Vec<u8>>,
    }

    impl FakeClipboard {
        fn holding(types: &[&str], bytes: &[u8]) -> Arc<Self> {
            Arc::new(Self {
                types: StdMutex::new(types.iter().map(|t| t.to_string()).collect()),
                bytes: StdMutex::new(bytes.to_vec()),
            })
        }

        fn becomes_empty(&self) {
            self.types.lock().expect("lock").clear();
            self.bytes.lock().expect("lock").clear();
        }
    }

    #[async_trait]
    impl Clipboard for FakeClipboard {
        async fn targets(&self) -> Result<Vec<String>> {
            Ok(self.types.lock().expect("lock").clone())
        }
        async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>> {
            let held = self.types.lock().expect("lock").clone();
            if !held.iter().any(|t| t == mime) {
                return Ok(None);
            }
            Ok(Some(self.bytes.lock().expect("lock").clone()))
        }
    }

    /// Lets one fake back both the trait object the agent owns and the handle
    /// the test mutates.
    struct Handle(Arc<FakeClipboard>);

    #[async_trait]
    impl Clipboard for Handle {
        async fn targets(&self) -> Result<Vec<String>> {
            self.0.targets().await
        }
        async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>> {
            self.0.read(mime).await
        }
    }

    fn agent(clipboard: Arc<FakeClipboard>) -> Agent {
        Agent::new(Box::new(Handle(clipboard)))
    }

    #[tokio::test]
    async fn a_ping_is_answered_without_touching_the_clipboard() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let (response, body) = agent.handle(&Request::ChannelPing, Instant::now()).await;
        assert_eq!(response, Response::Pong);
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn targets_are_reported_from_the_clipboard() {
        let agent = agent(FakeClipboard::holding(&[PNG], b"\x89PNG"));
        let (response, _) = agent
            .handle(&Request::ClipboardTargets, Instant::now())
            .await;
        assert_eq!(response, Response::Targets(vec![PNG.to_string()]));
    }

    #[tokio::test]
    async fn a_read_returns_a_length_header_and_the_bytes() {
        let agent = agent(FakeClipboard::holding(&[PNG], b"\x89PNG"));
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, Instant::now()).await;
        assert_eq!(response, Response::Payload { len: 4 });
        assert_eq!(body, Some(b"\x89PNG".to_vec()));
    }

    /// The two-call race. A paste is TARGETS then read; if the clipboard
    /// changes in between, the read must still serve what was advertised or the
    /// paste fails for no visible reason.
    #[tokio::test]
    async fn a_read_is_served_from_the_snapshot_when_the_clipboard_has_moved_on() {
        let clipboard = FakeClipboard::holding(&[PNG], b"\x89PNG");
        let agent = agent(clipboard.clone());
        let now = Instant::now();

        // The advertisement, then the read that fetches and caches the bytes.
        agent.handle(&Request::ClipboardTargets, now).await;
        let request = Request::ClipboardRead { mime: PNG.into() };
        agent.handle(&request, now).await;

        clipboard.becomes_empty();

        let (response, body) = agent.handle(&request, now).await;
        assert_eq!(response, Response::Payload { len: 4 });
        assert_eq!(body, Some(b"\x89PNG".to_vec()));
    }

    /// A type that was advertised but never fetched, whose content then
    /// vanishes, is named as a race rather than reported as an empty clipboard.
    #[tokio::test]
    async fn a_type_that_vanishes_before_its_first_read_says_so() {
        let clipboard = FakeClipboard::holding(&[PNG], b"\x89PNG");
        let agent = agent(clipboard.clone());
        let now = Instant::now();

        agent.handle(&Request::ClipboardTargets, now).await;
        clipboard.becomes_empty();

        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, now).await;
        let Response::Error { code, message } = response else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::Unavailable);
        assert!(message.contains("changed"), "{message}");
        assert!(body.is_none());
    }

    /// The snapshot is for one paste, not a cache. A read long after the
    /// advertisement must see the real clipboard.
    #[tokio::test]
    async fn the_snapshot_expires() {
        let clipboard = FakeClipboard::holding(&[PNG], b"\x89PNG");
        let agent = agent(clipboard.clone());
        let now = Instant::now();

        agent.handle(&Request::ClipboardTargets, now).await;
        let request = Request::ClipboardRead { mime: PNG.into() };
        agent.handle(&request, now).await;

        clipboard.becomes_empty();

        let later = now + SNAPSHOT_TTL + Duration::from_secs(1);
        let (response, body) = agent.handle(&request, later).await;
        assert!(matches!(response, Response::Error { .. }), "{response:?}");
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn a_genuinely_empty_clipboard_is_unavailable_rather_than_a_fault() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, Instant::now()).await;
        assert!(
            matches!(&response, Response::Error { code, .. } if *code == ErrorCode::Unavailable),
            "{response:?}"
        );
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn a_payload_over_the_cap_is_refused_with_the_limit_named() {
        let huge = vec![0u8; MAX_PAYLOAD + 1];
        let agent = agent(FakeClipboard::holding(&[TEXT], &huge));
        let request = Request::ClipboardRead { mime: TEXT.into() };
        let (response, body) = agent.handle(&request, Instant::now()).await;
        let Response::Error { code, message } = response else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::TooLarge);
        assert!(message.contains(TEXT), "{message}");
        assert!(body.is_none());
    }

    /// End to end over a real socket, which is the only way to know the framing
    /// and the socket layer agree.
    #[tokio::test]
    async fn the_agent_answers_over_a_real_unix_socket() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");

        let agent = Arc::new(agent(FakeClipboard::holding(&[TEXT], b"hello")));
        let serving = tokio::spawn({
            let socket = socket.clone();
            async move { agent.serve(&socket).await }
        });

        // Wait for the listener rather than sleeping a fixed interval.
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

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

    /// An unparseable line must not take the agent down: the next shell into
    /// the same server still needs it.
    #[tokio::test]
    async fn a_malformed_request_is_answered_rather_than_fatal() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");

        let agent = Arc::new(agent(FakeClipboard::holding(&[TEXT], b"hello")));
        let serving = tokio::spawn({
            let socket = socket.clone();
            async move { agent.serve(&socket).await }
        });
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

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
}

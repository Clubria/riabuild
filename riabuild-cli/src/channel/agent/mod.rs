//! The laptop side: decide what to serve.
//!
//! This file answers requests; `server` carries the answers over a socket. The
//! split is what lets every dispatch decision — the snapshot, the size cap, the
//! empty-versus-raced distinction — be tested without a socket anywhere.

mod server;

use crate::channel::clipboard::Clipboard;
use crate::channel::protocol::{ErrorCode, MAX_PAYLOAD, Request, Response};
use crate::channel::resize;
use std::time::{Duration, Instant};
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
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Arc;
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

    /// An agent over a fixed clipboard, for the socket tests in `server`, which
    /// only need something that answers.
    pub(super) fn agent_holding(types: &[&str], bytes: &[u8]) -> Agent {
        agent(FakeClipboard::holding(types, bytes))
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
}

//! The laptop side: decide what to serve.
//!
//! This file answers requests; `server` carries the answers over a socket. The
//! split is what lets every dispatch decision — the snapshot, the size cap, the
//! empty-versus-raced distinction — be tested without a socket anywhere.

mod server;

use crate::channel::clipboard::Clipboard;
use crate::channel::opener::Opener;
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
    opener: Box<dyn Opener>,
    snapshot: Mutex<Option<Snapshot>>,
}

impl Agent {
    pub fn new(clipboard: Box<dyn Clipboard>, opener: Box<dyn Opener>) -> Self {
        Self {
            clipboard,
            opener,
            snapshot: Mutex::new(None),
        }
    }

    /// Answers one request. Bodies are passed and returned beside the header
    /// rather than read and written, so every dispatch decision is testable
    /// without a socket.
    ///
    /// `body` is the payload of an inbound write, already framed by `server`
    /// against the length the header announced.
    pub async fn handle(
        &self,
        request: &Request,
        body: Option<Vec<u8>>,
        now: Instant,
    ) -> (Response, Option<Vec<u8>>) {
        match request {
            Request::ChannelPing => (Response::Pong, None),
            Request::ClipboardTargets => self.targets(now).await,
            Request::ClipboardRead { mime } => self.read(mime, now).await,
            Request::ClipboardWrite { mime, len } => self.write(mime, body, *len).await,
            Request::OpenUrl { url } => self.open(url).await,
        }
    }

    /// Opens a link on the laptop.
    ///
    /// No prompt, by decision: `clipboard.read` already hands the server the
    /// contents of this laptop's clipboard without asking, and a confirmation
    /// per URL turns a device-code login into a two-machine dance. The log line
    /// is the audit trail, and it is written *before* the opener runs so a URL
    /// that hangs a browser is still recorded.
    ///
    /// The scheme was settled in `decode_request`; by here the URL is http or
    /// https and nothing else.
    async fn open(&self, url: &str) -> (Response, Option<Vec<u8>>) {
        note(&format!("opening {url}"));
        match self.opener.open(url).await {
            Ok(()) => (Response::Opened, None),
            Err(error) => {
                note(&format!("could not open {url}: {error:#}"));
                (
                    Response::Error {
                        code: ErrorCode::Unavailable,
                        message: format!("this laptop could not open the link: {error}"),
                    },
                    None,
                )
            }
        }
    }

    /// The one operation that changes the laptop rather than reporting on it.
    async fn write(
        &self,
        mime: &str,
        body: Option<Vec<u8>>,
        len: usize,
    ) -> (Response, Option<Vec<u8>>) {
        let bytes = body.unwrap_or_default();
        if bytes.len() != len {
            return (
                Response::Error {
                    code: ErrorCode::BadRequest,
                    message: format!(
                        "the write announced {len} bytes and carried {}",
                        bytes.len()
                    ),
                },
                None,
            );
        }

        // The snapshot describes a clipboard that is about to stop existing.
        // Dropped before the write rather than after, so a write that fails
        // half-way cannot leave a reader being served content the laptop no
        // longer holds.
        *self.snapshot.lock().await = None;

        match self.clipboard.write(mime, &bytes).await {
            Ok(true) => (Response::Written, None),
            Ok(false) => (
                Response::Error {
                    code: ErrorCode::Unsupported,
                    message: format!("the channel does not carry `{mime}`"),
                },
                None,
            ),
            Err(error) => (
                Response::Error {
                    code: ErrorCode::Internal,
                    message: format!("could not write the laptop's clipboard: {error}"),
                },
                None,
            ),
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

/// The laptop's record of what the server asked it to do.
///
/// Only `browser.open` writes here. Clipboard traffic is high-volume and its
/// content is the developer's own, so logging it would be both noisy and a
/// place secrets accumulate; opening a link is rare, consequential, and the
/// operation the developer agreed to have happen without a prompt. That trade
/// is the reason there is no confirmation.
fn note(message: &str) {
    if let Ok(path) = std::env::var(crate::channel::LOG_ENV) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "agent: {message}");
        }
    }
    eprintln!("riabuild: {message}");
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
        async fn write(&self, mime: &str, bytes: &[u8]) -> Result<bool> {
            // Only the types a real backend has a name for. `refuses` stands in
            // for anything outside the table.
            if mime == "application/pdf" {
                return Ok(false);
            }
            if mime == "explode" {
                anyhow::bail!("the clipboard tool fell over");
            }
            *self.types.lock().expect("lock") = vec![mime.to_string()];
            *self.bytes.lock().expect("lock") = bytes.to_vec();
            Ok(true)
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
        async fn write(&self, mime: &str, bytes: &[u8]) -> Result<bool> {
            self.0.write(mime, bytes).await
        }
    }

    /// Records what it was asked to open, and fails for one sentinel URL so the
    /// error path has something to exercise.
    #[derive(Default)]
    pub(super) struct FakeOpener {
        opened: StdMutex<Vec<String>>,
    }

    impl FakeOpener {
        fn opened(&self) -> Vec<String> {
            self.opened.lock().expect("lock").clone()
        }
    }

    #[async_trait]
    impl Opener for Arc<FakeOpener> {
        async fn open(&self, url: &str) -> Result<()> {
            if url.contains("unreachable") {
                anyhow::bail!("no browser answered");
            }
            self.opened.lock().expect("lock").push(url.to_string());
            Ok(())
        }
    }

    fn agent(clipboard: Arc<FakeClipboard>) -> Agent {
        agent_with(clipboard, Arc::new(FakeOpener::default()))
    }

    fn agent_with(clipboard: Arc<FakeClipboard>, opener: Arc<FakeOpener>) -> Agent {
        Agent::new(Box::new(Handle(clipboard)), Box::new(opener))
    }

    /// An agent over a fixed clipboard, for the socket tests in `server`, which
    /// only need something that answers.
    pub(super) fn agent_holding(types: &[&str], bytes: &[u8]) -> Agent {
        agent(FakeClipboard::holding(types, bytes))
    }

    #[tokio::test]
    async fn an_open_request_reaches_the_laptops_opener() {
        let opener = Arc::new(FakeOpener::default());
        let agent = agent_with(FakeClipboard::holding(&[], b""), opener.clone());

        let request = Request::OpenUrl {
            url: "https://github.com/login/device".into(),
        };
        let (response, body) = agent.handle(&request, None, Instant::now()).await;

        assert_eq!(response, Response::Opened);
        assert!(body.is_none());
        assert_eq!(opener.opened(), vec!["https://github.com/login/device"]);
    }

    /// A laptop that cannot open the link says so rather than reporting
    /// success: the server's shim exits non-zero off the back of this, which is
    /// what makes Claude Code print the URL instead of pretending it opened.
    #[tokio::test]
    async fn a_laptop_that_cannot_open_the_link_reports_it() {
        let opener = Arc::new(FakeOpener::default());
        let agent = agent_with(FakeClipboard::holding(&[], b""), opener.clone());

        let request = Request::OpenUrl {
            url: "https://unreachable.example.com".into(),
        };
        let (response, _) = agent.handle(&request, None, Instant::now()).await;

        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Unavailable,
                    ..
                }
            ),
            "{response:?}"
        );
        assert!(opener.opened().is_empty());
    }

    #[tokio::test]
    async fn a_ping_is_answered_without_touching_the_clipboard() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let (response, body) = agent
            .handle(&Request::ChannelPing, None, Instant::now())
            .await;
        assert_eq!(response, Response::Pong);
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn targets_are_reported_from_the_clipboard() {
        let agent = agent(FakeClipboard::holding(&[PNG], b"\x89PNG"));
        let (response, _) = agent
            .handle(&Request::ClipboardTargets, None, Instant::now())
            .await;
        assert_eq!(response, Response::Targets(vec![PNG.to_string()]));
    }

    #[tokio::test]
    async fn a_read_returns_a_length_header_and_the_bytes() {
        let agent = agent(FakeClipboard::holding(&[PNG], b"\x89PNG"));
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, None, Instant::now()).await;
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
        agent.handle(&Request::ClipboardTargets, None, now).await;
        let request = Request::ClipboardRead { mime: PNG.into() };
        agent.handle(&request, None, now).await;

        clipboard.becomes_empty();

        let (response, body) = agent.handle(&request, None, now).await;
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

        agent.handle(&Request::ClipboardTargets, None, now).await;
        clipboard.becomes_empty();

        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, None, now).await;
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

        agent.handle(&Request::ClipboardTargets, None, now).await;
        let request = Request::ClipboardRead { mime: PNG.into() };
        agent.handle(&request, None, now).await;

        clipboard.becomes_empty();

        let later = now + SNAPSHOT_TTL + Duration::from_secs(1);
        let (response, body) = agent.handle(&request, None, later).await;
        assert!(matches!(response, Response::Error { .. }), "{response:?}");
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn a_genuinely_empty_clipboard_is_unavailable_rather_than_a_fault() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, None, Instant::now()).await;
        assert!(
            matches!(&response, Response::Error { code, .. } if *code == ErrorCode::Unavailable),
            "{response:?}"
        );
        assert!(body.is_none());
    }

    fn write_of(mime: &str, bytes: &[u8]) -> Request {
        Request::ClipboardWrite {
            mime: mime.to_string(),
            len: bytes.len(),
        }
    }

    #[tokio::test]
    async fn a_write_puts_the_bytes_on_the_clipboard() {
        let clipboard = FakeClipboard::holding(&[], b"");
        let agent = agent(clipboard.clone());

        let (response, body) = agent
            .handle(
                &write_of(TEXT, b"copied on the server"),
                Some(b"copied on the server".to_vec()),
                Instant::now(),
            )
            .await;

        assert_eq!(response, Response::Written);
        assert!(body.is_none());
        assert_eq!(
            clipboard.read(TEXT).await.unwrap(),
            Some(b"copied on the server".to_vec())
        );
    }

    /// A write replaces the clipboard the snapshot describes. Left in place, the
    /// next read would serve the content that was just overwritten — and would
    /// look like a working paste while doing it.
    #[tokio::test]
    async fn a_write_invalidates_the_snapshot_taken_before_it() {
        let clipboard = FakeClipboard::holding(&[TEXT], b"the old clipboard");
        let agent = agent(clipboard.clone());
        let now = Instant::now();

        // Advertise, then read, so the snapshot holds cached content.
        agent.handle(&Request::ClipboardTargets, None, now).await;
        let read = Request::ClipboardRead { mime: TEXT.into() };
        let (_, cached) = agent.handle(&read, None, now).await;
        assert_eq!(cached, Some(b"the old clipboard".to_vec()));

        agent
            .handle(
                &write_of(TEXT, b"the new one"),
                Some(b"the new one".to_vec()),
                now,
            )
            .await;

        // Same instant, so nothing expired: only the write can have cleared it.
        let (_, after) = agent.handle(&read, None, now).await;
        assert_eq!(after, Some(b"the new one".to_vec()));
    }

    /// The length in the header is the framing. A body that does not match it
    /// means the stream is out of step, and writing the fragment would put half
    /// an image on the laptop.
    #[tokio::test]
    async fn a_body_that_does_not_match_its_announced_length_is_refused() {
        let clipboard = FakeClipboard::holding(&[], b"");
        let agent = agent(clipboard.clone());

        let request = Request::ClipboardWrite {
            mime: TEXT.into(),
            len: 100,
        };
        let (response, _) = agent
            .handle(&request, Some(b"short".to_vec()), Instant::now())
            .await;

        let Response::Error { code, message } = response else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::BadRequest);
        assert!(message.contains("100"), "{message}");
        // And nothing reached the clipboard.
        assert!(clipboard.read(TEXT).await.unwrap().is_none());
    }

    /// A truncated body arrives as `None`, and must be refused rather than
    /// written as an empty clipboard.
    #[tokio::test]
    async fn a_write_whose_body_never_arrived_is_refused() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let request = Request::ClipboardWrite {
            mime: TEXT.into(),
            len: 12,
        };
        let (response, _) = agent.handle(&request, None, Instant::now()).await;
        assert!(
            matches!(&response, Response::Error { code, .. } if *code == ErrorCode::BadRequest),
            "{response:?}"
        );
    }

    /// A type no backend has a name for is refused as unsupported rather than
    /// reported as a broken laptop.
    #[tokio::test]
    async fn a_write_of_a_type_the_channel_does_not_carry_is_unsupported() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let (response, _) = agent
            .handle(
                &write_of("application/pdf", b"%PDF"),
                Some(b"%PDF".to_vec()),
                Instant::now(),
            )
            .await;
        let Response::Error { code, message } = response else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::Unsupported);
        assert!(message.contains("application/pdf"), "{message}");
    }

    #[tokio::test]
    async fn a_clipboard_tool_that_fails_a_write_is_reported_as_a_fault() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let (response, _) = agent
            .handle(
                &write_of("explode", b"x"),
                Some(b"x".to_vec()),
                Instant::now(),
            )
            .await;
        let Response::Error { code, message } = response else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::Internal);
        assert!(message.contains("fell over"), "{message}");
    }

    /// An empty clipboard is a legitimate thing to copy, and zero bytes is not
    /// the same as a missing body.
    #[tokio::test]
    async fn an_empty_write_is_allowed() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let (response, _) = agent
            .handle(&write_of(TEXT, b""), Some(Vec::new()), Instant::now())
            .await;
        assert_eq!(response, Response::Written);
    }

    #[tokio::test]
    async fn a_payload_over_the_cap_is_refused_with_the_limit_named() {
        let huge = vec![0u8; MAX_PAYLOAD + 1];
        let agent = agent(FakeClipboard::holding(&[TEXT], &huge));
        let request = Request::ClipboardRead { mime: TEXT.into() };
        let (response, body) = agent.handle(&request, None, Instant::now()).await;
        let Response::Error { code, message } = response else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::TooLarge);
        assert!(message.contains(TEXT), "{message}");
        assert!(body.is_none());
    }
}

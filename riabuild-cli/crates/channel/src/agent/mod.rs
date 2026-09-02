//! The laptop side: decide what to serve.
//!
//! This file answers requests; `server` carries the answers over a socket. The
//! split is what lets every dispatch decision — the snapshot, the size cap, the
//! empty-versus-raced distinction — be tested without a socket anywhere.
//!
//! Two files hold the answers themselves. `snapshot` owns the three clipboard
//! operations and the snapshot that makes a two-call paste coherent; `browser`
//! owns the one operation that reaches past the laptop. What stays here is the
//! agent, the dispatch, and the deadline every one of those calls runs under.

mod browser;
mod pipe;
mod server;
mod snapshot;

/// What one pipe connection carried, which is the supervisor's evidence about
/// the connection it has just lost. Re-exported because `pipe` is private and
/// the supervisor is the caller it was written for.
pub use pipe::Served;
/// How long a `TARGETS` answer stays good for the read that follows it — see
/// `snapshot`, which owns the mechanism.
pub use snapshot::SNAPSHOT_TTL;

use crate::clipboard::Clipboard;
use crate::opener::Opener;
use crate::protocol::{ErrorCode, Request, Response};
use riabuild_ui::StatusBar;
use snapshot::Snapshot;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// How long one clipboard or browser subprocess may take before the agent
/// answers without it.
///
/// **Nothing else bounds them, and one that wedges takes the whole session
/// with it.** `xclip`, `osascript` and `xdg-open` are all capable of never
/// returning — a compositor that stops answering a selection request, a
/// Privacy dialog nobody is at the laptop to dismiss, a `.desktop` handler
/// waiting on a lock. `serve_pipe` cannot finish while a spawned answer still
/// holds its sender, so one wedged tool stopped the pipe from draining, which
/// stopped the supervisor's teardown, which stopped `remote::channel` — and
/// the developer's `riabuild remote` hung after their shell had already
/// exited, with nothing on screen naming a clipboard.
///
/// Shorter than every deadline downstream of it — `client::REQUEST_TIMEOUT`
/// (20 s) and `pump::REPLY_TIMEOUT` (25 s) — so the shim gets this sentence,
/// which names the laptop's tool, rather than its own timeout, which names
/// nothing. The subprocess itself dies with the dropped future: `RealRunner`
/// sets `kill_on_drop`, so this is a bound on the tool and not merely on
/// riabuild's patience.
pub const DISPATCH_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Agent {
    clipboard: Box<dyn Clipboard>,
    opener: Box<dyn Opener>,
    snapshot: Mutex<Option<Snapshot>>,
    /// Hands out `Snapshot::seq`. Never reset: a wrap at `u64` is not a thing
    /// a laptop reaches.
    snapshots: AtomicU64,
    /// Where the agent says what it is doing — the same line on row two the
    /// supervisor reports a dead channel on.
    ///
    /// **The agent has one for the reason the supervisor does.** Opening a link
    /// is the one thing this laptop does that a developer wants told about, and
    /// it happens *while* a full-screen Claude Code is drawing the screen from
    /// the far end of a mosh session: an `eprintln!` from here arrives through a
    /// terminal an interactive shell has put in raw mode, so it staircases down
    /// the right-hand side and then sits in the middle of somebody else's
    /// output for the rest of the session.
    ///
    /// Disabled by default, which is every run except a remote session — the
    /// developer who ran `riabuild channel agent` by hand owns their terminal
    /// and gets the ordinary printed line. Remote mode is the caller that knows
    /// otherwise, and says so with [`speaking_on`](Self::speaking_on).
    bar: Arc<StatusBar>,
}

impl Agent {
    pub fn new(clipboard: Box<dyn Clipboard>, opener: Box<dyn Opener>) -> Self {
        Self {
            clipboard,
            opener,
            snapshot: Mutex::new(None),
            snapshots: AtomicU64::new(0),
            bar: Arc::new(StatusBar::disabled()),
        }
    }

    /// Hands the agent the line to speak on instead of printing.
    ///
    /// Taken here rather than in [`new`](Self::new) because of the order the
    /// two are made in: what this laptop *can do* is settled by
    /// `laptop_agent`, which can fail and takes the whole channel down with it
    /// when it does, while the bar is started afterwards by `remote::channel` —
    /// last, so that nothing can return early past a task left repainting a
    /// line for a channel that never started. Consuming `self` is what keeps
    /// that a construction rather than a setting: the agent is shared behind an
    /// `Arc` the moment it is finished, and there is no later to change it in.
    #[must_use]
    pub fn speaking_on(mut self, bar: Arc<StatusBar>) -> Self {
        self.bar = bar;
        self
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
}

/// Bounds one clipboard or browser call. `None` means it did not answer inside
/// [`DISPATCH_TIMEOUT`].
async fn within<F: std::future::Future>(work: F) -> Option<F::Output> {
    tokio::time::timeout(DISPATCH_TIMEOUT, work).await.ok()
}

/// What the server is told about a tool on the laptop that stopped answering.
///
/// `Unavailable` rather than `Internal`: nothing is broken, and the state
/// resolves itself the moment the tool comes back or the developer dismisses
/// whatever it is waiting on. It reads the same way an empty clipboard does,
/// which is the right altitude for a paste that did not happen.
fn wedged(what: &str) -> Response {
    Response::Error {
        code: ErrorCode::Unavailable,
        message: format!(
            "the laptop took longer than {} seconds to {what}",
            DISPATCH_TIMEOUT.as_secs()
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    // Kept whole rather than split beside `snapshot` and `browser`: every case
    // below is driven through `Agent::handle`, and they share one set of
    // fixtures — the clipboard a test can change between calls, the opener that
    // records what it was asked for, and the gated pair that never returns.
    use super::*;
    use crate::mime::{PNG, TEXT};
    use crate::protocol::MAX_PAYLOAD;
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
    pub(crate) fn agent_holding(types: &[&str], bytes: &[u8]) -> Agent {
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

    /// Where the agent says what it is doing, when there is a line to say it
    /// on: on the bar, and not printed into a screen mosh and Claude Code are
    /// painting from the other end of the session.
    #[tokio::test]
    async fn opening_a_link_is_said_on_the_bar_where_there_is_one() {
        let bar = Arc::new(StatusBar::recording());
        let agent = agent_with(
            FakeClipboard::holding(&[], b""),
            Arc::new(FakeOpener::default()),
        )
        .speaking_on(Arc::clone(&bar));

        let request = Request::OpenUrl {
            url: "https://github.com/login/device".into(),
        };
        agent.handle(&request, None, Instant::now()).await;

        let said = bar.painted();
        assert!(
            said.iter()
                .any(|line| line.contains("opening https://github.com/login/device")),
            "{said:?}"
        );
    }

    /// …and so is a link that did not open, which is the case a developer most
    /// needs told: the shim on the server exits non-zero and Claude Code prints
    /// the URL, but nothing there says the laptop refused it.
    #[tokio::test]
    async fn a_link_that_would_not_open_is_said_on_the_bar_too() {
        let bar = Arc::new(StatusBar::recording());
        let agent = agent_with(
            FakeClipboard::holding(&[], b""),
            Arc::new(FakeOpener::default()),
        )
        .speaking_on(Arc::clone(&bar));

        let request = Request::OpenUrl {
            url: "https://unreachable.example.com".into(),
        };
        agent.handle(&request, None, Instant::now()).await;

        let said = bar.painted();
        assert!(
            said.iter()
                .any(|line| line.contains("could not open https://unreachable.example.com")),
            "{said:?}"
        );
    }

    /// The clipboard says nothing at all, and that is the whole reason the bar
    /// is usable for the link: paste is high-volume and its content is the
    /// developer's own, so a line per Ctrl+V would be both a flicker on row two
    /// and their clipboard on their screen.
    #[tokio::test]
    async fn pasting_says_nothing_on_the_bar() {
        let bar = Arc::new(StatusBar::recording());
        let agent = agent_with(
            FakeClipboard::holding(&[TEXT], b"a password, most likely"),
            Arc::new(FakeOpener::default()),
        )
        .speaking_on(Arc::clone(&bar));
        let now = Instant::now();

        agent.handle(&Request::ClipboardTargets, None, now).await;
        let read = Request::ClipboardRead { mime: TEXT.into() };
        agent.handle(&read, None, now).await;
        agent
            .handle(
                &write_of(TEXT, b"copied on the server"),
                Some(b"copied on the server".to_vec()),
                now,
            )
            .await;

        assert!(bar.painted().is_empty(), "{:?}", bar.painted());
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

    /// A clipboard tool that never returns for one type and answers instantly
    /// for the other. Both halves are load-bearing: the wedge is what the
    /// deadline is for, and the answer beside it is what proves the lock was
    /// not held across the wedge.
    struct Gated;

    #[async_trait]
    impl Clipboard for Gated {
        async fn targets(&self) -> Result<Vec<String>> {
            Ok(vec![PNG.to_string(), TEXT.to_string()])
        }
        async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>> {
            if mime == PNG {
                // `xclip` against a compositor that stopped answering a
                // selection request: open, running, and never coming back.
                std::future::pending::<()>().await;
            }
            Ok(Some(b"text".to_vec()))
        }
        async fn write(&self, _: &str, _: &[u8]) -> Result<bool> {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    fn gated() -> Agent {
        Agent::new(Box::new(Gated), Box::new(Arc::new(FakeOpener::default())))
    }

    /// I054. A tool that never returns must not become a request that never
    /// gets an answer.
    ///
    /// `serve_pipe` cannot finish while a spawned answer still holds its
    /// sender, so one wedged `xclip` stopped the pipe draining, which stopped
    /// the supervisor's teardown, which stopped `remote::channel` — and
    /// `riabuild remote` hung on the laptop after the developer's shell had
    /// already exited. Nothing on screen named a clipboard.
    #[tokio::test(start_paused = true)]
    async fn a_clipboard_tool_that_never_returns_is_answered_rather_than_waited_out() {
        let agent = gated();
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, None, Instant::now()).await;

        let Response::Error { code, message } = response else {
            panic!("expected an error, got {response:?}");
        };
        assert_eq!(code, ErrorCode::Unavailable);
        assert!(message.contains("read the laptop's clipboard"), "{message}");
        assert!(body.is_none());
    }

    /// …and a write, which is the other subprocess in the paste path.
    #[tokio::test(start_paused = true)]
    async fn a_write_that_never_returns_is_answered_rather_than_waited_out() {
        let agent = gated();
        let request = Request::ClipboardWrite {
            mime: TEXT.into(),
            len: 2,
        };
        let (response, _) = agent
            .handle(&request, Some(b"hi".to_vec()), Instant::now())
            .await;
        assert!(
            matches!(&response, Response::Error { code, .. } if *code == ErrorCode::Unavailable),
            "{response:?}"
        );
    }

    /// I061. The snapshot mutex must not be held across the subprocess.
    ///
    /// Both `serve_pipe` and `agent::server` spawn a task per request on the
    /// stated grounds that "a slow clipboard read must not hold up every other
    /// shell" — and then every one of them queued behind one `xclip`, because
    /// `read` took the lock before the fetch and gave it back after. One
    /// developer pasting a screenshot stalled every other shell into the
    /// server for as long as it took.
    ///
    /// Under the old code this does not fail, it *hangs*, so the wait is
    /// bounded: a hang has to present as a red test rather than a slow one.
    #[tokio::test]
    async fn a_read_waiting_on_its_subprocess_does_not_hold_up_every_other_read() {
        let agent = gated();
        let now = Instant::now();
        let stuck = Request::ClipboardRead { mime: PNG.into() };
        let other = Request::ClipboardRead { mime: TEXT.into() };

        let answered = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                biased;
                _ = agent.handle(&stuck, None, now) => panic!("the wedged read should not finish"),
                answer = agent.handle(&other, None, now) => answer,
            }
        })
        .await
        .expect("a second read must not queue behind the first one's subprocess");

        assert_eq!(answered.0, Response::Payload { len: 4 });
        assert_eq!(answered.1, Some(b"text".to_vec()));
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

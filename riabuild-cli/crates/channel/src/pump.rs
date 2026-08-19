//! The server end of the channel: a socket, and a pipe to the laptop.
//!
//! `riabuild channel pump` is what the laptop runs on the server, over an
//! ordinary `ssh -T`. It binds `<namespace>/channel.sock`, accepts the shims
//! that connect to it, and relays each request to the laptop over its own
//! stdin and stdout.
//!
//! **It binds the socket itself, and that is the point of the whole design.**
//! Under `ssh -R` the socket was created by `sshd`, so whether a stale one from
//! a killed session could be replaced was the *server's* setting to make —
//! `sshd_config`'s `StreamLocalBindUnlink`, which defaults to `no`. The client
//! option riabuild passed could not affect it, so a leftover `channel.sock`
//! disabled paste on that server permanently and no riabuild flag could clear
//! it. Here the socket belongs to a process riabuild started, under the
//! developer's own account, and clearing a dead one is an ordinary `unlink` by
//! its owner.
//!
//! **The pump is a relay and never a parser.** It moves bytes the shim wrote to
//! the laptop, and bytes the laptop wrote back to the shim. It does not decode
//! a `Request`, does not know what operations exist, and cannot invent one. The
//! only place a line becomes an operation is the laptop's compiled-in
//! `protocol::decode_request`, which is the entire security argument for the
//! channel and is unchanged by moving the transport.

use crate::mux::{Frame, KEEPALIVE_ID, read_frame, write_frame};
use crate::protocol::MAX_PAYLOAD;
use anyhow::{Context, Result};
use riabuild_ui::Failure;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
// `tokio`'s instant, not `std`'s, and the difference is not cosmetic: this is
// the clock the keepalive's own sleeps are measured against, and the two only
// agree outside a test. Under a paused clock `std::time::Instant` goes on
// reporting wall-clock, so the deadline below would be measured against a
// clock nothing else in the loop is using — which is a test that cannot fail
// rather than a test that passes.
use tokio::time::Instant;

/// How long a shim waits for the laptop before the pump gives up on its behalf.
///
/// Slightly longer than the shim's own `client::REQUEST_TIMEOUT`, so the shim
/// is the one that reports the timeout and the developer sees a message naming
/// the clipboard rather than one naming the pump. This exists only to stop a
/// pending entry leaking for a reply that is never coming.
const REPLY_TIMEOUT: Duration = Duration::from_secs(25);

/// A request line plus the largest body the protocol carries, and nothing more.
///
/// A shim is riabuild's own code, but it is reached through a socket every
/// co-tenant on a shared account could connect to, so its input is bounded here
/// rather than trusted.
const MAX_REQUEST: u64 = MAX_PAYLOAD as u64 + 4096;

/// How often the pump asks the laptop whether it is still on the other end.
///
/// The same interval as the `ServerAliveInterval` the laptop's own `ssh` is
/// started with, because this is the same measurement taken from the other
/// side. Cheap in a way the health probe this design deleted was not: one empty
/// frame down a pipe that is already open, never a second SSH connection.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// How long the pump goes unanswered before it gives the socket back.
///
/// Three missed keepalives, matching `ServerAliveCountMax=3` on the laptop, so
/// the two ends give up on each other at about the same moment.
///
/// **This is the bound on how long a pump can outlive its laptop, and until it
/// existed there was none.** The laptop notices a dropped connection because
/// `ssh` is measuring it; the server does not, because `sshd` ships with
/// `ClientAliveInterval 0` and a TCP connection whose peer stopped
/// acknowledging is indistinguishable from an idle one for as long as the
/// kernel keeps retransmitting — a quarter of an hour, and longer if the peer
/// is merely wedged rather than gone. For all of it the pump stayed bound to
/// the socket, which cost the developer three things at once: every paste and
/// every `riabuild channel status` blocked for the full reply timeout and then
/// failed, every pump the reconnecting supervisor started found the socket
/// live and refused it, and the supervisor — recognising none of that — said
/// the channel could not reach the server, which was the one thing that was
/// not true.
const KEEPALIVE_DEADLINE: Duration = Duration::from_secs(45);

/// How long [`answers`] gives a socket to say whether anything is serving on it.
///
/// Generous by design. Completing a connection to an `AF_UNIX` socket is the
/// kernel's work and does not wait for the pump to `accept`, so a healthy
/// listener answers in microseconds and any value here is slack. It is a
/// *bound*, not a deadline anyone is expected to approach.
const LIVENESS_PROBE: Duration = Duration::from_secs(2);

/// Binds the socket and relays until the pipe closes.
///
/// Returns `Ok(())` on a clean end of pipe — the laptop's session ended, which
/// is how every normal run finishes.
pub async fn run(socket: &Path) -> Result<()> {
    serve(socket, tokio::io::stdin(), tokio::io::stdout()).await
}

/// The same, over any pipe, so the relay can be tested without an ssh or a
/// laptop.
pub async fn serve<R, W>(socket: &Path, input: R, output: W) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let listener = bind(socket).await?;

    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Vec<u8>>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // One task owns the pipe to the laptop. Frames from many connections
    // interleave on it, and two tasks writing a header and a body without
    // coordination would splice one shell's screenshot into another's paste.
    let (outbound, mut queued) = mpsc::channel::<Frame>(32);
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(frame) = queued.recv().await {
            if write_frame(&mut output, &frame).await.is_err() {
                break;
            }
        }
    });

    // Set once the laptop's end of the pipe closes. Every shim — the ones
    // already waiting and the ones that connect a moment later — has to learn
    // that immediately: a paste that blocks for the reply timeout reads as
    // Claude Code being broken, while one that fails at once reads as paste
    // being unavailable, which is the truth and is what `client` reports.
    let (gone, watching) = watch::channel(false);

    // When the laptop was last heard from, as milliseconds since `since`.
    // An instant is not atomic and this is read by one task and written by
    // another on every frame, which is the whole of why it is stored this way.
    let since = Instant::now();
    let heard = Arc::new(AtomicU64::new(0));

    let router = tokio::spawn({
        let pending = Arc::clone(&pending);
        let heard = Arc::clone(&heard);
        async move {
            let mut input = BufReader::new(input);
            // Ends on a clean close, a closed lid, or a frame this end cannot
            // read — all three mean the same thing here, which is that no
            // further reply is coming.
            while let Ok(Some(frame)) = read_frame(&mut input).await {
                // Any frame at all is proof the laptop is there, which is why
                // the keepalive below needs nothing of its own to read: a reply
                // to a paste counts exactly as much as a reply to a keepalive,
                // and a busy channel never pays for one.
                heard.store(since.elapsed().as_millis() as u64, Ordering::Relaxed);
                // A reply for a connection that has given up is dropped, not an
                // error: the shim timed out and closed, and the laptop had no
                // way to know before answering. The keepalive's own reply lands
                // here too, and being dropped is the right answer for it: it
                // was never asking a question, only proving there is somebody
                // to answer one.
                if let Some(waiting) = pending.lock().await.remove(&frame.id) {
                    let _ = waiting.send(frame.payload);
                }
            }
            // Dropping every sender resolves each waiting `answered` at once,
            // and the flag catches the shims that have not registered yet.
            let _ = gone.send(true);
            pending.lock().await.clear();
        }
    });

    let next = AtomicU64::new(1);
    let accepting = async {
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .context("the channel socket stopped accepting connections")?;
            let id = next.fetch_add(1, Ordering::Relaxed);
            let pending = Arc::clone(&pending);
            let outbound = outbound.clone();
            let watching = watching.clone();
            // Serving inline would let one slow clipboard read block every
            // other shell into this server.
            tokio::spawn(async move {
                let _ = relay(stream, id, pending, outbound, watching).await;
            });
        }
    };

    let result: Result<()> = tokio::select! {
        outcome = accepting => outcome,
        // The laptop closing its end is the ordinary end of a session, not a
        // failure: `riabuild remote` finished, or the lid closed.
        _ = router => Ok(()),
        _ = writer => Ok(()),
        // A laptop that stopped answering without closing anything. Also not a
        // failure, and it must not be reported as one: the supervisor on the
        // other end reads this pump's exit as an ordinary disconnect and
        // rebuilds, which is exactly what should happen.
        () = keepalive(since, Arc::clone(&heard), outbound.clone()) => Ok(()),
    };

    // Ours, and nothing answers on it now. Leaving it behind is precisely the
    // stale socket this design exists to stop being fatal — harmless here since
    // the next pump would clear it, and still worth not creating.
    let _ = tokio::fs::remove_file(socket).await;
    result
}

/// One shim connection, start to finish.
async fn relay(
    mut stream: UnixStream,
    id: u64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Vec<u8>>>>>,
    outbound: mpsc::Sender<Frame>,
    mut watching: watch::Receiver<bool>,
) -> Result<()> {
    // The shim half-closes after writing, so end of input is the end of the
    // request — header line, body and all. Reading to EOF rather than parsing a
    // length is what keeps the pump from having to know the protocol at all.
    let mut payload = Vec::new();
    (&mut stream)
        .take(MAX_REQUEST)
        .read_to_end(&mut payload)
        .await
        .context("could not read the request from the shim")?;

    // A connection that closed without writing is not a request, and must not
    // become an empty frame the laptop is asked to answer. Something does this
    // on purpose: `bind` below connects to decide whether a socket is live, and
    // a channel that reported that probe to the laptop as a paste would be
    // answering a question nobody asked.
    if payload.is_empty() {
        return Ok(());
    }

    // Checked before anything is registered: this connection may have arrived
    // in the gap between the laptop closing its pipe and the listener being
    // dropped, and a shim that waited out the reply timeout there would be
    // waiting on a peer that is already gone.
    if *watching.borrow_and_update() {
        return Ok(());
    }

    let (reply, answered) = oneshot::channel();
    pending.lock().await.insert(id, reply);

    if outbound.send(Frame { id, payload }).await.is_err() {
        pending.lock().await.remove(&id);
        return Ok(());
    }

    // Closing without a reply is the right answer to every arm below: `client`
    // reads it as a channel that is not there, which is exactly what it is.
    let response = tokio::select! {
        biased;
        answer = answered => match answer {
            Ok(bytes) => bytes,
            Err(_) => return Ok(()),
        },
        _ = watching.changed() => {
            pending.lock().await.remove(&id);
            return Ok(());
        }
        _ = tokio::time::sleep(REPLY_TIMEOUT) => {
            pending.lock().await.remove(&id);
            return Ok(());
        }
    };

    stream.write_all(&response).await?;
    stream.flush().await?;
    Ok(())
}

/// Returns when the laptop has gone [`KEEPALIVE_DEADLINE`] without a word.
///
/// The point of it is the *return*. Ending the pump unbinds the socket, which
/// is what lets the reconnecting supervisor's own pump take the path, and what
/// turns a paste into an immediate "the channel is not running" instead of a
/// twenty-second wait for a laptop that is not there.
///
/// Sent rather than merely waited for, because silence on this pipe is not
/// evidence of anything on its own: a developer who is not pasting produces
/// exactly as little traffic as a laptop that has gone. The frame obliges an
/// answer — see [`KEEPALIVE_ID`] for why an empty one is enough — and it is the
/// answer that is the measurement.
async fn keepalive(since: Instant, heard: Arc<AtomicU64>, outbound: mpsc::Sender<Frame>) {
    loop {
        tokio::time::sleep(KEEPALIVE_INTERVAL).await;

        let last = Duration::from_millis(heard.load(Ordering::Relaxed));
        if since.elapsed().saturating_sub(last) >= KEEPALIVE_DEADLINE {
            return;
        }

        // `try_send`, never `send`. The queue drains into a pipe that is
        // precisely what may have stopped moving, and a keepalive that blocked
        // on the wedged connection it exists to detect would be the one thing
        // this function must not do. A full queue is not treated as a failure
        // on its own — the deadline above is the only thing that decides — but
        // a closed one means the writer has already ended, and there is nothing
        // left to keep alive.
        if outbound
            .try_send(Frame {
                id: KEEPALIVE_ID,
                payload: Vec::new(),
            })
            .is_err()
            && outbound.is_closed()
        {
            return;
        }
    }
}

/// Binds the channel socket, clearing a dead one and refusing a live one.
///
/// The distinction is the whole of it. A socket file that nothing answers on is
/// a leftover from a killed session and is this account's to remove; a socket
/// that *does* answer belongs to a pump that is still serving, and taking it
/// would silently cut that session's paste. Connecting is the only way to tell
/// them apart — the file looks identical either way.
async fn bind(socket: &Path) -> Result<UnixListener> {
    if socket.exists() {
        if answers(socket).await {
            return Err(Failure::new(
                format!(
                    "another riabuild is already serving the clipboard channel at {}",
                    socket.display()
                ),
                "Close the other riabuild session on this server, or wait for it to finish.",
            )
            .into());
        }
        // Nothing answered: a socket left by a session that was killed. Under
        // `ssh -R` this was fatal and unfixable, because sshd owned the bind.
        tokio::fs::remove_file(socket)
            .await
            .with_context(|| format!("could not clear the stale socket at {}", socket.display()))?;
    }

    UnixListener::bind(socket).with_context(|| format!("could not listen on {}", socket.display()))
}

/// Whether anything is serving on this socket — and never a question that hangs.
///
/// The connect is bounded because the answer is only ever used to choose
/// between clearing a leftover and refusing to take a live one, and neither
/// choice is worth blocking a session on. An unbounded probe is not a probe: it
/// puts the pump's startup at the mercy of the kernel's willingness to refuse a
/// connection, which is exactly where it was found — a stale `channel.sock` on
/// macOS left `riabuild channel pump` waiting on a connect that never completed
/// and never failed, so the channel came up neither working nor broken. The
/// same stall inside `pump::tests` held a release's macOS job open for as long
/// as GitHub allows a job to run.
///
/// A timeout counts as **not** serving, and that is the deliberate half. It is
/// the direction that keeps the leftover socket this whole design exists to
/// recover from recoverable: a pump that cannot answer a connect inside
/// [`LIVENESS_PROBE`] is not one a shim could have used either, so treating it
/// as live would trade a rare stolen channel for a permanently dead one. The
/// case the refusal protects — a colleague's *working* pump — answers in
/// microseconds and is never the one that times out.
async fn answers(socket: &Path) -> bool {
    matches!(
        tokio::time::timeout(LIVENESS_PROBE, UnixStream::connect(socket)).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::read_frame;
    use tokio::io::duplex;

    /// How long any one step of a pump test may take before it is a failure.
    ///
    /// Every await below is wrapped in [`within`], and that is not belt and
    /// braces. These tests talk to a real socket, and `cargo test` on macOS runs
    /// in `release.yml` — *after* the tag is pushed — so a test that stalls
    /// there does not report a failure anybody can read: it holds the release's
    /// macOS job open for the six hours GitHub allows, publishes nothing, and
    /// says nothing about which test stopped. A stalled test has to be a red
    /// one. Generous enough that a loaded three-core runner never reaches it.
    const STEP: Duration = Duration::from_secs(20);

    /// The one place a pump test is allowed to wait.
    ///
    /// `what` is the sentence the developer reads when it does not arrive, so it
    /// names the thing that failed to happen rather than the call that was made.
    async fn within<F: std::future::Future>(what: &str, future: F) -> F::Output {
        match tokio::time::timeout(STEP, future).await {
            Ok(value) => value,
            Err(_) => panic!("{what} — nothing happened for {STEP:?}"),
        }
    }

    /// Drives a pump over an in-memory pipe, standing in for the laptop.
    struct Laptop {
        reader: BufReader<tokio::io::DuplexStream>,
        writer: tokio::io::DuplexStream,
    }

    impl Laptop {
        /// The next frame the pump sends up, or a failure naming its absence.
        async fn frame(&mut self, what: &str) -> Frame {
            within(what, read_frame(&mut self.reader))
                .await
                .expect("the pipe carried something that is not a frame")
                .expect(what)
        }
    }

    async fn pump(socket: &Path) -> (Laptop, tokio::task::JoinHandle<Result<()>>) {
        let (ours_in, theirs_in) = duplex(64 * 1024);
        let (ours_out, theirs_out) = duplex(64 * 1024);
        let path = socket.to_path_buf();
        let serving = tokio::spawn(async move { serve(&path, theirs_in, theirs_out).await });

        // Waits for a socket that *answers*, never for one that merely exists.
        // Half these tests start a pump over a stale socket file, so existence
        // is true before this pump has bound anything and a test that waited on
        // it would race the very replacement it is checking. Connecting is
        // free of side effects because `relay` drops a request with no bytes in
        // it, and it goes through `answers` so one attempt cannot hang the wait.
        let mut ready = false;
        for _ in 0..400 {
            if answers(socket).await {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Running on regardless is what made a stalled pump look like a stalled
        // *laptop*: the test went on to send a request nothing was listening for
        // and then waited for a reply that could never come. The pump not coming
        // up is its own failure and says so here.
        assert!(ready, "the pump never bound {}", socket.display());

        (
            Laptop {
                reader: BufReader::new(ours_out),
                writer: ours_in,
            },
            serving,
        )
    }

    /// A shim: write a request, half-close, read the whole reply.
    async fn shim(socket: &Path, request: &[u8]) -> Vec<u8> {
        try_shim(socket, request).await.expect("shim")
    }

    /// The same, for the tests about a channel that is going away — where a
    /// refused connect or a reset read is the expected outcome rather than a
    /// failure, and the only thing being asserted is that it *returns*.
    async fn try_shim(socket: &Path, request: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(request).await?;
        stream.shutdown().await?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await?;
        Ok(reply)
    }

    /// The relay, end to end: bytes in at the socket, a frame out at the pipe,
    /// a frame back in, the same bytes out at the socket.
    #[tokio::test]
    async fn a_request_reaches_the_laptop_and_its_reply_reaches_the_shim() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let (mut laptop, serving) = pump(&socket).await;

        let asking = tokio::spawn({
            let socket = socket.clone();
            async move { shim(&socket, b"{\"v\":1,\"op\":\"channel.ping\"}\n").await }
        });

        let frame = laptop
            .frame("the shim's request never reached the laptop")
            .await;
        assert_eq!(frame.payload, b"{\"v\":1,\"op\":\"channel.ping\"}\n");

        write_frame(
            &mut laptop.writer,
            &Frame {
                id: frame.id,
                payload: b"{\"ok\":true}\n".to_vec(),
            },
        )
        .await
        .expect("write");

        let answered = within("the reply never reached the shim", asking).await;
        assert_eq!(answered.expect("shim"), b"{\"ok\":true}\n");
        serving.abort();
    }

    /// Two shells pasting at once. Replies come back out of order on purpose:
    /// the ids are the only thing keeping one shell's answer out of the other's.
    #[tokio::test]
    async fn two_connections_do_not_get_each_others_replies() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let (mut laptop, serving) = pump(&socket).await;

        let first = tokio::spawn({
            let socket = socket.clone();
            async move { shim(&socket, b"first\n").await }
        });
        let frame_one = laptop.frame("the first shim's request never arrived").await;

        let second = tokio::spawn({
            let socket = socket.clone();
            async move { shim(&socket, b"second\n").await }
        });
        let frame_two = laptop
            .frame("the second shim's request never arrived")
            .await;

        assert_ne!(frame_one.id, frame_two.id, "ids must be distinct");
        assert_eq!(frame_one.payload, b"first\n");
        assert_eq!(frame_two.payload, b"second\n");

        // Answered in reverse, which a pump keying on anything but the id
        // would get wrong.
        for (id, body) in [(frame_two.id, b"two".as_slice()), (frame_one.id, b"one")] {
            write_frame(
                &mut laptop.writer,
                &Frame {
                    id,
                    payload: body.to_vec(),
                },
            )
            .await
            .expect("write");
        }

        let one = within("the first shim was never answered", first).await;
        let two = within("the second shim was never answered", second).await;
        assert_eq!(one.expect("first"), b"one");
        assert_eq!(two.expect("second"), b"two");
        serving.abort();
    }

    /// A body with a newline and a non-UTF-8 byte, over the socket and the pipe
    /// together. Each layer is tested for this separately; this is the one that
    /// proves they agree.
    #[tokio::test]
    async fn a_binary_body_survives_the_socket_and_the_pipe() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let (mut laptop, serving) = pump(&socket).await;

        let payload = b"header\n\x89PNG\r\n\x1a\n\xFF\x00trailing".to_vec();
        let sent = payload.clone();
        let asking = tokio::spawn({
            let socket = socket.clone();
            async move { shim(&socket, &sent).await }
        });

        let frame = laptop
            .frame("the binary request never reached the laptop")
            .await;
        assert_eq!(frame.payload, payload);

        write_frame(
            &mut laptop.writer,
            &Frame {
                id: frame.id,
                payload: payload.clone(),
            },
        )
        .await
        .expect("write");
        let echoed = within("the binary reply never reached the shim", asking).await;
        assert_eq!(echoed.expect("shim"), payload);
        serving.abort();
    }

    /// The failure this design exists to end. Under `ssh -R` a socket left by a
    /// killed session was fatal for every later session, because sshd owned the
    /// bind and its `StreamLocalBindUnlink` defaults to `no`.
    #[tokio::test]
    async fn a_stale_socket_from_a_killed_pump_is_cleared_rather_than_fatal() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");

        let (_first_laptop, first) = pump(&socket).await;
        first.abort();
        // Awaited, not just asked for. `abort` schedules a cancellation and the
        // listener's descriptor closes when the task is dropped, so a test that
        // moved straight on would be racing a pump that is still answering —
        // and the replacement would then refuse the socket as live rather than
        // clear it, which is the opposite of what this test is here to pin.
        let _ = within("the killed pump never went away", first).await;
        // The file outlives the abort — exactly the leftover a killed session
        // leaves on a real server.
        assert!(socket.exists(), "the stale socket should still be there");

        let (mut laptop, second) = pump(&socket).await;
        let asking = tokio::spawn({
            let socket = socket.clone();
            async move { shim(&socket, b"after\n").await }
        });
        let frame = laptop
            .frame("the replacement pump never bound the stale socket")
            .await;
        assert_eq!(frame.payload, b"after\n");

        write_frame(
            &mut laptop.writer,
            &Frame {
                id: frame.id,
                payload: b"ok".to_vec(),
            },
        )
        .await
        .expect("write");
        let answered = within("the replacement pump never answered the shim", asking).await;
        assert_eq!(answered.expect("shim"), b"ok");
        second.abort();
    }

    /// A *live* pump's socket is not stolen. Clearing a dead socket and taking
    /// a working one look identical on disk, and only one of them is recovery.
    #[tokio::test]
    async fn a_socket_a_live_pump_is_serving_is_refused_rather_than_taken() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let (_laptop, serving) = pump(&socket).await;

        let error = bind(&socket).await.expect_err("should refuse");
        assert!(
            error.to_string().contains("already serving"),
            "{error} should name the other session"
        );
        serving.abort();
    }

    /// The failure this keepalive exists for, and the one that cost a developer
    /// two bug reports at once.
    ///
    /// A laptop that *closes* the pipe is easy — every test above covers it.
    /// This is a laptop that says nothing and closes nothing, which is what a
    /// dropped wifi link leaves behind: the laptop's own `ssh` gives up on its
    /// `ServerAliveInterval` and reconnects, while on the server `sshd` — whose
    /// `ClientAliveInterval` is off by default — sits on a TCP connection the
    /// kernel will go on retransmitting into for a quarter of an hour. For all
    /// of it the pump used to hold the socket, and holding the socket is what
    /// made both symptoms: every paste and every `riabuild channel status`
    /// waited out the full reply timeout, and every pump the reconnecting
    /// laptop started was refused with `already serving`.
    #[tokio::test(start_paused = true)]
    async fn a_pump_whose_laptop_goes_silent_gives_the_socket_back() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        // Held, never read from and never written to: a pipe that is open and
        // dead, which is the whole point. Dropping it would be the easy case.
        let (_laptop, serving) = pump(&socket).await;

        let ended = tokio::time::timeout(Duration::from_secs(600), serving)
            .await
            .expect("the pump never gave up on a laptop that stopped answering");

        assert!(ended.expect("join").is_ok(), "this is not a failure");
        // The half that matters to the next session: the path is free, so the
        // reconnecting laptop's own pump binds it instead of being refused.
        assert!(
            !socket.exists(),
            "the socket must be given back, not left for the next pump to be refused by"
        );
    }

    /// …and a laptop that is answering keeps its pump, however little the
    /// developer is pasting.
    ///
    /// The mirror of the test above, and the one that stops the fix from being
    /// worse than the bug: an idle channel produces no shim traffic at all, so
    /// a pump that took silence for absence would end a working session's paste
    /// every forty-five seconds.
    #[tokio::test(start_paused = true)]
    async fn a_laptop_that_answers_keepalives_keeps_its_pump() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let (mut laptop, serving) = pump(&socket).await;

        // Four rounds is past the deadline twice over, with no shim connecting
        // at any point.
        for round in 0..4 {
            let frame = laptop.frame("the pump stopped sending keepalives").await;
            assert_eq!(frame.id, KEEPALIVE_ID, "round {round}");
            assert!(
                frame.payload.is_empty(),
                "a keepalive asks for nothing: {:?}",
                frame.payload
            );
            write_frame(
                &mut laptop.writer,
                &Frame {
                    id: frame.id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("write");
        }

        assert!(!serving.is_finished(), "an answered pump must stay up");
        // And it is still a pump, not merely a live process.
        let asking = tokio::spawn({
            let socket = socket.clone();
            async move { shim(&socket, b"after\n").await }
        });
        let frame = laptop.frame("the pump stopped relaying").await;
        assert_eq!(frame.payload, b"after\n");
        write_frame(
            &mut laptop.writer,
            &Frame {
                id: frame.id,
                payload: b"ok".to_vec(),
            },
        )
        .await
        .expect("write");
        assert_eq!(
            within("the shim was never answered", asking)
                .await
                .expect("shim"),
            b"ok"
        );
        serving.abort();
    }

    /// The laptop going away must fail the shim quickly and cleanly, never hang
    /// it: a paste that blocks reads as Claude Code being broken.
    #[tokio::test]
    async fn a_laptop_that_disappears_closes_the_shim_rather_than_hanging_it() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let (laptop, serving) = pump(&socket).await;

        // The pipe closes, which is a closed lid or an ended session.
        drop(laptop);

        // Every outcome here is acceptable except one. A refused connect, a
        // reset read and an empty reply all mean "there is no channel", which
        // `client` turns into a clean miss the developer can read. Hanging is
        // the single answer that is not allowed: a paste that blocks reads as
        // Claude Code being broken rather than as paste being unavailable.
        let outcome =
            tokio::time::timeout(Duration::from_secs(5), try_shim(&socket, b"ping\n")).await;
        match outcome {
            Ok(Ok(reply)) => assert!(reply.is_empty(), "expected no reply, got {reply:?}"),
            Ok(Err(_)) => {}
            Err(_) => panic!("the shim hung after the laptop went away"),
        }
        serving.abort();
    }
}

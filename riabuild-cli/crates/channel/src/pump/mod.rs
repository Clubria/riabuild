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
//!
//! Four files. `bind` is taking the socket — clearing a dead one, refusing a
//! live one; `relay` is one shim connection start to finish; `keepalive` is how
//! the pump learns its laptop has gone. What is left here is the accept loop
//! that drives them and the single pipe every frame is funnelled onto.

mod bind;
mod keepalive;
mod relay;

use bind::bind;
use keepalive::keepalive;
use relay::relay;

use crate::mux::{Frame, read_frame, write_frame};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;

/// How many shim connections the pump serves at once.
///
/// The permit is taken *before* `accept`, so at the cap the kernel's backlog
/// holds the next connection rather than this process holding a task and a
/// descriptor for it. That is the difference between a bound on memory and a
/// bound on both: `MAX_REQUEST` × an unbounded number of connections is an
/// unbounded number.
///
/// Generous for what this is. One shim per paste, on a server with a handful of
/// shells open, and every one of them is gone inside a round trip; anything
/// approaching this is a runaway rather than a busy developer.
const MAX_CONNECTIONS: usize = 64;

/// How long the accept loop pauses after a failure it intends to retry.
///
/// Without it, `EMFILE` — the process is out of descriptors — is a loop that
/// fails and retries as fast as the CPU allows, which is a busy wait on a
/// server other developers are sharing. Long enough that the condition has a
/// chance to clear, short enough that nobody notices a paste waiting it out.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

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
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let accepting = async {
        loop {
            // Taken before the accept rather than after it. At the cap the
            // kernel's backlog holds the next connection, so this process is
            // never holding a task and a descriptor for a shim it has not
            // begun to serve — which is what made `MAX_REQUEST` a bound on one
            // connection's memory and on nothing else.
            let Ok(permit) = Arc::clone(&connections).acquire_owned().await else {
                // Only reachable if something closed the semaphore, which
                // nothing here does.
                return Ok(());
            };

            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                // A failure that is about this one connection, or about a
                // resource that will come back. Ending the pump on it would
                // give up the socket — and with it every shell's paste on this
                // server — because one `accept` happened to land while the
                // process was briefly out of descriptors.
                Err(error) if transient(&error) => {
                    drop(permit);
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                    continue;
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error)
                        .context("the channel socket stopped accepting connections"));
                }
            };

            let id = next.fetch_add(1, Ordering::Relaxed);
            let pending = Arc::clone(&pending);
            let outbound = outbound.clone();
            let watching = watching.clone();
            // Serving inline would let one slow clipboard read block every
            // other shell into this server.
            tokio::spawn(async move {
                let _ = relay(stream, id, pending, outbound, watching).await;
                drop(permit);
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

/// Whether an `accept` failure is one to try again rather than one to end the
/// pump on.
///
/// The three `ErrorKind`s are about a single connection that went away between
/// the kernel queueing it and this process taking it, or about a syscall a
/// signal interrupted; none of them says anything about the listener. The
/// `errno`s are the resource exhaustions — this process or the machine out of
/// descriptors, the kernel out of buffers — which clear on their own as soon as
/// something else closes one.
///
/// Ending the pump on any of them costs the whole server's paste: the socket is
/// unbound, every shell that names it starts failing, and the reconnecting
/// laptop's supervisor reports it as a server it cannot reach. Retrying costs
/// [`ACCEPT_RETRY_DELAY`].
fn transient(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::Interrupted | ErrorKind::WouldBlock
    ) {
        return true;
    }
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

#[cfg(test)]
mod tests {
    use super::bind::answers;
    use super::relay::REQUEST_DEADLINE;
    use super::*;
    use crate::mux::{KEEPALIVE_ID, read_frame};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
    use tokio::net::UnixStream;

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

    /// I058. A shim that connects and never writes must be shed, not held.
    ///
    /// `take(MAX_REQUEST).read_to_end()` bounded the buffer at 32 MB and put no
    /// clock on it, so one such connection cost a task, a descriptor and that
    /// buffer for the length of the session, and enough of them cost the
    /// server. Nothing on the box distinguishes it from a shim whose developer
    /// is simply slow, which is why the deadline is generous rather than tight.
    #[tokio::test(start_paused = true)]
    async fn a_shim_that_connects_and_never_writes_is_let_go_of() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");
        let (_laptop, serving) = pump(&socket).await;

        // Connected, never written to, never half-closed: the read below has
        // nothing to return and no end of input coming.
        let idle = UnixStream::connect(&socket).await.expect("connect");

        // The pump lets go on its own, which the shim sees as its connection
        // closing. Under the old code this waits for ever.
        let mut idle = idle;
        let mut reply = Vec::new();
        let outcome = tokio::time::timeout(
            REQUEST_DEADLINE + Duration::from_secs(30),
            idle.read_to_end(&mut reply),
        )
        .await;
        assert!(
            outcome.is_ok(),
            "the pump held a connection that never wrote"
        );
        assert!(reply.is_empty(), "no reply was earned: {reply:?}");

        // And it is still a pump: shedding one connection is not ending the
        // session.
        assert!(!serving.is_finished(), "the pump must stay up");
        serving.abort();
    }

    /// The accept loop must survive a failure that is about one connection or
    /// about a resource that comes back.
    ///
    /// Ending the pump unbinds the socket, so every shell on the server loses
    /// paste and the reconnecting laptop reports a server it cannot reach —
    /// paid for one `accept` that landed while the process was briefly out of
    /// descriptors.
    #[test]
    fn a_transient_accept_failure_is_retried_rather_than_ending_the_pump() {
        use std::io::{Error, ErrorKind};

        for kind in [
            ErrorKind::ConnectionAborted,
            ErrorKind::Interrupted,
            ErrorKind::WouldBlock,
        ] {
            assert!(transient(&Error::from(kind)), "{kind:?}");
        }
        for errno in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
            assert!(transient(&Error::from_raw_os_error(errno)), "errno {errno}");
        }

        // …and a listener that is genuinely gone still ends it. Retrying that
        // is a busy loop against a socket nobody will ever connect to.
        assert!(!transient(&Error::from_raw_os_error(libc::EBADF)));
        assert!(!transient(&Error::from(ErrorKind::PermissionDenied)));
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

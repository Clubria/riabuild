//! The server side: connect, ask once, read once.
//!
//! Everything here is in the paste path, so the contract is that it never
//! hangs. A laptop that has closed its lid must produce a fast, clean failure —
//! the alternative is Claude Code stopping dead on Ctrl+V, which reads as the
//! editor being broken rather than the channel being down.

use crate::protocol::{Request, Response, decode_response, encode_request};
use anyhow::{Context, Result, bail};
use riabuild_ui::Failure;
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

/// What a request carrying nothing waits, which is every `channel.ping`.
///
/// The generous deadline above is for a transfer, and a ping has nothing to
/// transfer: it is one short line each way, so anything past a few seconds is a
/// laptop that is not going to answer rather than one that is still sending.
/// Twenty seconds of silence is what `riabuild channel status` — a command
/// whose entire job is to answer "is this thing working?" — used to spend
/// before saying anything at all, which reads as riabuild hanging and is how a
/// dead channel got reported as a stuck command.
pub const PING_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct Reply {
    pub response: Response,
    pub body: Vec<u8>,
}

pub async fn request(socket: &Path, request: &Request) -> Result<Reply> {
    request_with_body(socket, request, &[]).await
}

/// A request with a deadline of the caller's choosing.
///
/// One knob, because there is one thing a caller knows that this file does not:
/// how much it is asking for. See [`PING_TIMEOUT`].
pub async fn request_within(socket: &Path, request: &Request, waiting: Duration) -> Result<Reply> {
    exchange_within(socket, request, &[], waiting).await
}

/// A request that carries a payload — today only `clipboard.write`.
///
/// The body goes out on the same connection, straight after the header line,
/// framed by the length the header announced.
pub async fn request_with_body(socket: &Path, request: &Request, body: &[u8]) -> Result<Reply> {
    exchange_within(socket, request, body, REQUEST_TIMEOUT).await
}

async fn exchange_within(
    socket: &Path,
    request: &Request,
    body: &[u8],
    waiting: Duration,
) -> Result<Reply> {
    let connect = UnixStream::connect(socket);
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .with_context(|| {
            format!(
                "the laptop channel at {} did not accept a connection",
                socket.display()
            )
        })?
        .map_err(|error| unavailable(socket, &error))?;

    tokio::time::timeout(waiting, exchange(stream, request, body))
        .await
        .map_err(|_| unanswered(socket, waiting))?
}

/// What a shim says when the socket is there, something is serving it, and the
/// laptop behind it never replies.
///
/// A `Failure` rather than a bare context line, which is what it was, and the
/// difference is the whole of what a developer gets: `channel status` renders
/// `action` as the one thing to do, and a plain `anyhow` context has no such
/// field — so the command that exists to explain a dead channel timed out and
/// then explained nothing.
///
/// The remedy says to wait, because this is the state that resolves itself. A
/// pump whose connection to the laptop dropped without the server noticing goes
/// on holding the socket and swallowing every request into a pipe nobody reads;
/// its own keepalive is what ends that, and it ends within the minute.
fn unanswered(socket: &Path, waited: Duration) -> anyhow::Error {
    Failure::new(
        format!(
            "the clipboard channel did not answer within {} seconds",
            waited.as_secs()
        ),
        "Wait a minute and try again — a session whose connection to the laptop dropped keeps \
         the channel until it notices, and gives it up on its own. If it is still silent after \
         that, run `riabuild remote` again from your laptop.",
    )
    .detail(format!(
        "Something is serving {}, so the channel is bound here; it is the laptop on the other \
         end that is not replying.",
        socket.display()
    ))
    .into()
}

/// What a shim says when there is no channel to talk to.
///
/// Reported at the altitude the developer can act at, rather than the one the
/// kernel answered at. `RIABUILD_CHANNEL_SOCKET` is a promise written once into
/// the shell's environment when the session opened; the channel behind it is a
/// live resource a laptop-side process owns and can end at any moment, and
/// nothing reconciles the two. A shell that outlives its session — a second
/// terminal whose owner exited first, a tmux window still there tomorrow, a
/// laptop that slept and never came back — goes on naming a path that is
/// perfectly correct and completely unbound.
///
/// `No such file or directory (os error 2)` is a true answer to a question the
/// developer did not ask. It reads as riabuild being broken, when what happened
/// is that a session ended. Worse, it is invisible in the one place it matters
/// most: Claude Code's copy falls back to an OSC 52 escape, so copying still
/// appears to work while paste and `xdg-open` do not, and no two symptoms ever
/// point at one cause.
///
/// The remedy is the same in every case and is worth stating rather than
/// implying: the socket path is per-developer and stable, so a new
/// `riabuild remote` to that server binds this very path again and the shells
/// already open start working, without anybody restarting them.
/// A `Failure` rather than a bare string, which is what `supervisor::diagnose`
/// already returns for the other half of the same story: it keeps the diagnosis
/// and the one concrete next action in separate fields, so a caller can render
/// them apart — `channel status` shows the action as folded prose — while
/// `Display` still puts both on one line for a shim with only stderr to write
/// to. It deliberately does **not** name `riabuild channel status`: this text is
/// most of what that command prints, and advising a developer to run the
/// command they are already reading the output of is how advice stops being
/// read at all.
fn unavailable(socket: &Path, error: &std::io::Error) -> anyhow::Error {
    use std::io::ErrorKind;
    let path = socket.display();
    // One remedy, and a real one rather than a shrug: the socket path is per
    // developer and per server, so a new session binds this very path and the
    // shells already open start working again without being restarted.
    let reconnect = "Run `riabuild remote` again from your laptop. It binds this same socket, \
                     so the shells you already have open start working again — nothing needs \
                     restarting here.";
    match error.kind() {
        ErrorKind::NotFound => Failure::new(
            format!("the clipboard channel is not running — nothing is bound at {path}"),
            reconnect,
        )
        // Not "the session that opened this shell", which is what this said
        // while a channel belonged to whichever session started it. A sibling
        // session standing by takes the channel over within seconds of the one
        // serving it ending, so nothing bound here now means there is no
        // session left to do that — which is a different fact and the one that
        // makes the remedy above the only remedy.
        .detail(
            "Every `riabuild remote` session this laptop had open to this server has ended. \
             While one is open it takes the channel over on its own."
                .to_string(),
        )
        .into(),
        // A socket file with nobody accepting: a pump killed hard enough that it
        // never unlinked. Told apart because "nothing is bound at this path"
        // reads as plainly wrong when the path is sitting right there, and a
        // developer who checks trusts the next message less.
        ErrorKind::ConnectionRefused => Failure::new(
            format!("the clipboard channel is not answering — {path} is there, but nothing is serving it"),
            reconnect,
        )
        .detail("The session that created the socket ended without removing it.".to_string())
        .into(),
        _ => Failure::new(
            format!("the clipboard channel at {path} is not available"),
            reconnect,
        )
        .detail(error.to_string())
        .into(),
    }
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
    // Half-close: the write half only, so the reply still comes back down the
    // read half. This is what tells the pump the request is complete, and it is
    // why the pump can relay bytes without parsing them — end of input is the
    // end of the request, so nothing on the server has to know that
    // `clipboard.write` is the one operation with a body. Harmless on the
    // direct-socket path, where `agent::serve_one` frames on the announced
    // length and never waits for EOF.
    stream
        .shutdown()
        .await
        .context("could not finish sending the request to the laptop channel")?;

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

    /// The message a developer actually meets, and the one this exists to fix.
    ///
    /// `No such file or directory (os error 2)` is a true answer to a question
    /// nobody asked: the path is right and a session ended. Left at that
    /// altitude it reads as riabuild being broken, and it is met at the worst
    /// possible moment — beside a Claude Code whose copying still works,
    /// because that falls back to an OSC 52 escape needing no channel at all.
    #[tokio::test]
    async fn a_channel_that_is_not_running_says_so_and_says_what_to_do() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let error = request(&dir.path().join("absent.sock"), &Request::ChannelPing)
            .await
            .expect_err("should fail")
            .to_string();

        assert!(error.contains("not running"), "{error}");
        // The remedy, and it is a real one: the socket path is per-developer
        // and stable, so a new session binds this same path and the shells
        // already open start working without being restarted.
        assert!(error.contains("riabuild remote"), "{error}");
        // …and never an instruction to run the command that prints this text.
        assert!(!error.contains("channel status"), "{error}");
        // The kernel's wording must not be what leads.
        assert!(!error.starts_with("No such file"), "{error}");
    }

    /// A laptop that never answers must produce something a developer can act
    /// on, and this is the case that produced the worst of both.
    ///
    /// It was an `anyhow` context line, which has no `action` field — so
    /// `riabuild channel status`, the one command whose whole job is to explain
    /// a dead channel, sat silent for twenty seconds and then printed a
    /// diagnosis with no remedy under it. The silence read as riabuild hanging,
    /// which is how one dead channel was reported as a stuck command.
    #[tokio::test(start_paused = true)]
    async fn a_laptop_that_never_answers_says_what_to_do_about_it() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("silent.sock");
        // Accepts and then says nothing: a pump whose own connection to the
        // laptop has dropped without the server noticing.
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let held = tokio::spawn(async move {
            let accepted = listener.accept().await;
            std::future::pending::<()>().await;
            drop(accepted);
        });

        let error = request_within(&socket, &Request::ChannelPing, PING_TIMEOUT)
            .await
            .expect_err("should time out");
        let failure = error
            .downcast_ref::<Failure>()
            .expect("a timeout must carry a remedy, not only a sentence");

        assert!(failure.attempting.contains("did not answer"), "{failure}");
        assert!(failure.action.contains("riabuild remote"), "{failure}");
        // The distinction the developer needs: the channel is bound *here*, so
        // this is not the "nothing is running" case and the remedy is not the
        // same one.
        assert!(
            !failure.to_string().contains("not running"),
            "{failure} confuses a silent laptop with an absent one"
        );
        held.abort();
    }

    /// A socket file with nobody accepting is a different sentence: "nothing is
    /// bound at this path" reads as wrong when the path is plainly there, and
    /// a developer who checks will trust the next message less.
    #[tokio::test]
    async fn a_socket_nobody_is_serving_is_told_apart_from_one_that_is_absent() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("dead.sock");
        // A plain file where a socket should be: connecting to it is refused
        // rather than reported as missing, which is the case this separates.
        tokio::fs::write(&socket, b"not a socket")
            .await
            .expect("write");

        let error = request(&socket, &Request::ChannelPing)
            .await
            .expect_err("should fail")
            .to_string();
        assert!(error.contains("channel"), "{error}");
        assert!(!error.contains("nothing is bound"), "{error}");
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

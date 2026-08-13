//! Answering the server over one pipe instead of one socket per request.
//!
//! The laptop half of the exec transport. `supervisor` starts `ssh -T <host>
//! riabuild channel pump` and hands the child's stdout and stdin here; every
//! frame that arrives is one shim's request, and every frame written back is
//! its reply.
//!
//! The dispatch is `agent::server`'s, unchanged: a request line is narrowed by
//! the compiled-in `decode_request`, a body is framed against the length the
//! header announced, and a line the allowlist refuses is *answered* rather than
//! dropped. Only the carrier is different — which is the whole claim of this
//! design, and the reason the security argument did not have to be re-made.

use super::Agent;
use crate::mux::{Frame, read_frame, write_frame};
use crate::protocol::{ErrorCode, Request, Response, decode_request, encode_response};
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::mpsc;

impl Agent {
    /// Serves until the pipe closes, answering with how many requests it
    /// carried.
    ///
    /// The count is what the supervisor's backoff schedule is reset by. "The
    /// connection stayed up" is not the same claim: a channel that came up,
    /// carried nothing and sat there for an hour has not earned a fast retry,
    /// and under the old transport that distinction needed a health probe over
    /// a second SSH connection to make. Here it is a number this loop already
    /// has.
    pub async fn serve_pipe<R, W>(self: Arc<Self>, input: R, output: W) -> Result<u64>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        // One task owns the pipe. Replies are produced concurrently — a slow
        // clipboard read must not hold up every other shell on that server —
        // and two of them writing a header and a body without coordination
        // would splice one answer into another.
        let (replies, mut queued) = mpsc::channel::<Frame>(32);
        let writing = tokio::spawn(async move {
            let mut output = output;
            while let Some(frame) = queued.recv().await {
                if write_frame(&mut output, &frame).await.is_err() {
                    break;
                }
            }
        });

        let mut carried = 0u64;
        let mut input = BufReader::new(input);
        loop {
            let frame = match read_frame(&mut input).await {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                // A frame this end cannot read is not recoverable: the stream
                // position is unknown, so every later frame would be read out
                // of the middle of this one. Ending the connection lets the
                // supervisor rebuild it, which is the only clean repair.
                Err(_) => break,
            };

            carried = carried.saturating_add(1);
            let agent = Arc::clone(&self);
            let replies = replies.clone();
            tokio::spawn(async move {
                let payload = answer(&agent, &frame.payload).await;
                let _ = replies
                    .send(Frame {
                        id: frame.id,
                        payload,
                    })
                    .await;
            });
        }

        drop(replies);
        let _ = writing.await;
        Ok(carried)
    }
}

/// One request's bytes in, one reply's bytes out.
///
/// Always produces a reply, including for a request it cannot parse. A shim
/// left without an answer would wait out its whole timeout, and the developer
/// would read a hang where the truth is "that is not an operation".
async fn answer(agent: &Agent, payload: &[u8]) -> Vec<u8> {
    let (response, body) = match split(payload) {
        Ok((request, inbound)) => agent.handle(&request, inbound, Instant::now()).await,
        Err(message) => (
            Response::Error {
                code: ErrorCode::BadRequest,
                message,
            },
            None,
        ),
    };

    let mut bytes = encode_response(&response).into_bytes();
    if let Some(body) = body {
        bytes.extend_from_slice(&body);
    }
    bytes
}

/// Splits a frame into its request line and the body the header announced.
///
/// The body is framed by the announced length rather than by "the rest of the
/// frame", for the same reason `server` reads exactly `len` from the socket: a
/// write whose header and payload disagree is a corrupt clipboard, and
/// accepting it silently is worse than refusing it.
fn split(payload: &[u8]) -> Result<(Request, Option<Vec<u8>>), String> {
    let end = payload
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| "the request has no header line".to_string())?;

    let line = std::str::from_utf8(&payload[..end])
        .map_err(|_| "the request header is not valid UTF-8".to_string())?;
    let request = decode_request(line).map_err(|error| error.to_string())?;

    let rest = &payload[end + 1..];
    let body = match &request {
        Request::ClipboardWrite { len, .. } => {
            if rest.len() < *len {
                return Err(format!(
                    "the write announced {len} bytes and carried {}",
                    rest.len()
                ));
            }
            Some(rest[..*len].to_vec())
        }
        _ => None,
    };

    Ok((request, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::agent_holding;
    use crate::mime::TEXT;
    use crate::protocol::{decode_response, encode_request};
    use tokio::io::duplex;

    /// The server's end of a pipe an agent is serving.
    struct Server {
        reader: BufReader<tokio::io::DuplexStream>,
        writer: tokio::io::DuplexStream,
    }

    fn served(agent: Arc<Agent>) -> (Server, tokio::task::JoinHandle<Result<u64>>) {
        let (ours_out, theirs_out) = duplex(64 * 1024);
        let (ours_in, theirs_in) = duplex(64 * 1024);
        let serving = tokio::spawn(agent.serve_pipe(theirs_out, theirs_in));
        (
            Server {
                reader: BufReader::new(ours_in),
                writer: ours_out,
            },
            serving,
        )
    }

    async fn ask(server: &mut Server, id: u64, payload: Vec<u8>) -> Vec<u8> {
        write_frame(&mut server.writer, &Frame { id, payload })
            .await
            .expect("write");
        let frame = read_frame(&mut server.reader)
            .await
            .expect("read")
            .expect("a reply");
        assert_eq!(frame.id, id, "a reply must carry its request's id");
        frame.payload
    }

    #[tokio::test]
    async fn a_request_over_the_pipe_is_answered_by_the_agent() {
        let (mut server, serving) = served(Arc::new(agent_holding(&[TEXT], b"hello")));

        let reply = ask(
            &mut server,
            1,
            encode_request(&Request::ClipboardTargets).into_bytes(),
        )
        .await;
        let line = String::from_utf8(reply).expect("utf8");
        assert_eq!(
            decode_response(&line).expect("decode"),
            Response::Targets(vec![TEXT.to_string()])
        );

        serving.abort();
    }

    /// A read's payload follows its header inside the same frame, and must come
    /// back byte for byte — this is the screenshot path.
    #[tokio::test]
    async fn a_read_returns_its_payload_after_the_header() {
        let (mut server, serving) = served(Arc::new(agent_holding(&[TEXT], b"first\nsecond\xFF")));

        let reply = ask(
            &mut server,
            2,
            encode_request(&Request::ClipboardRead {
                mime: TEXT.to_string(),
            })
            .into_bytes(),
        )
        .await;

        let end = reply.iter().position(|b| *b == b'\n').expect("a header");
        let header = std::str::from_utf8(&reply[..end]).expect("utf8");
        assert_eq!(
            decode_response(header).expect("decode"),
            Response::Payload { len: 13 }
        );
        assert_eq!(&reply[end + 1..], b"first\nsecond\xFF");

        serving.abort();
    }

    /// A write carries its body in the same frame. The framing is the risk: a
    /// body with an embedded newline is what a reader splitting on lines would
    /// corrupt.
    #[tokio::test]
    async fn a_write_carries_its_body_in_the_same_frame() {
        let (mut server, serving) = served(Arc::new(agent_holding(&[TEXT], b"")));

        let body = b"first\nsecond\xFF".to_vec();
        let mut payload = encode_request(&Request::ClipboardWrite {
            mime: TEXT.to_string(),
            len: body.len(),
        })
        .into_bytes();
        payload.extend_from_slice(&body);

        let reply = ask(&mut server, 3, payload).await;
        let line = String::from_utf8(reply).expect("utf8");
        assert_eq!(decode_response(&line).expect("decode"), Response::Written);

        serving.abort();
    }

    /// A line the allowlist refuses is answered, not dropped: a shim with no
    /// reply waits out its whole timeout and reads as a hang.
    #[tokio::test]
    async fn a_malformed_request_is_answered_rather_than_dropped() {
        let (mut server, serving) = served(Arc::new(agent_holding(&[TEXT], b"hello")));

        let reply = ask(&mut server, 4, b"not json\n".to_vec()).await;
        assert!(
            String::from_utf8_lossy(&reply).contains("bad_request"),
            "{:?}",
            String::from_utf8_lossy(&reply)
        );

        // Still serving: the next shell into that server needs this agent.
        let reply = ask(
            &mut server,
            5,
            encode_request(&Request::ChannelPing).into_bytes(),
        )
        .await;
        let line = String::from_utf8(reply).expect("utf8");
        assert_eq!(decode_response(&line).expect("decode"), Response::Pong);

        serving.abort();
    }

    /// A write whose header and body disagree is refused rather than truncated
    /// onto the laptop's clipboard.
    #[tokio::test]
    async fn a_write_shorter_than_its_header_promised_is_refused() {
        let (mut server, serving) = served(Arc::new(agent_holding(&[TEXT], b"")));

        let mut payload = encode_request(&Request::ClipboardWrite {
            mime: TEXT.to_string(),
            len: 64,
        })
        .into_bytes();
        payload.extend_from_slice(b"short");

        let reply = ask(&mut server, 6, payload).await;
        assert!(
            String::from_utf8_lossy(&reply).contains("bad_request"),
            "{:?}",
            String::from_utf8_lossy(&reply)
        );

        serving.abort();
    }

    /// Two shells pasting at once, answered on one pipe. Ids are the only thing
    /// keeping one shell's answer out of the other's.
    #[tokio::test]
    async fn concurrent_requests_are_answered_against_their_own_ids() {
        let (mut server, serving) = served(Arc::new(agent_holding(&[TEXT], b"hello")));

        for id in [11u64, 12, 13] {
            write_frame(
                &mut server.writer,
                &Frame {
                    id,
                    payload: encode_request(&Request::ChannelPing).into_bytes(),
                },
            )
            .await
            .expect("write");
        }

        let mut seen = Vec::new();
        for _ in 0..3 {
            let frame = read_frame(&mut server.reader)
                .await
                .expect("read")
                .expect("a reply");
            let line = String::from_utf8(frame.payload).expect("utf8");
            assert_eq!(decode_response(&line).expect("decode"), Response::Pong);
            seen.push(frame.id);
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![11, 12, 13]);

        serving.abort();
    }
}

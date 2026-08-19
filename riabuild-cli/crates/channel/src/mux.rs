//! Many shim connections, one pipe.
//!
//! `ssh -R` gave the channel one socket connection per request for free, and
//! that is the only thing an exec session does not hand back: a single pair of
//! pipes carries every shell on the server at once. So each request and each
//! reply travels as a **frame** — a JSON header line naming a connection, then
//! exactly the announced number of raw bytes.
//!
//! ```text
//! {"id":7,"len":1234}\n<1234 bytes>
//! ```
//!
//! The payload is the wire form `protocol` already defines: a request line and
//! its optional body, or a response header line and its optional body. Nothing
//! here parses it. That separation is deliberate — the pump is a relay and must
//! never be the thing that decides what an operation *is*, because the whole
//! security argument for the channel is that the laptop's compiled-in
//! `decode_request` is the only such decision.
//!
//! Length-prefixed rather than newline-delimited for the same reason `protocol`
//! is: a screenshot is 2–15 MB of arbitrary bytes, and roughly one PNG in
//! sixteen thousand would otherwise be cut short by a byte that happened to be
//! `\n`.

use crate::protocol::MAX_PAYLOAD;
use serde::{Deserialize, Serialize};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

/// Which connection a frame belongs to, and how many bytes it carries.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    id: u64,
    len: usize,
}

/// The id no shim connection is ever given.
///
/// `pump::serve` hands out ids from one, so zero is free — and the pump uses it
/// for the one frame that belongs to no connection: the keepalive it sends to
/// find out whether the laptop is still on the other end of the pipe. Reserved
/// here, beside the ids it has to stay clear of, rather than in either end that
/// depends on it.
///
/// It carries no payload and asks for nothing. The pump has no business naming
/// an operation — it is a relay, and the laptop's compiled-in `decode_request`
/// is the only thing that decides what an operation *is* — so all this frame
/// does is oblige the far end to send a frame back. Any answer will do,
/// including the error an older laptop returns for a request it cannot parse.
pub const KEEPALIVE_ID: u64 = 0;

/// One request or one reply, tagged with the connection it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum FrameError {
    Malformed(String),
    TooLarge(usize),
    Io(std::io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Malformed(detail) => {
                write!(f, "the channel received a frame it cannot read: {detail}")
            }
            FrameError::TooLarge(len) => write!(
                f,
                "a frame announced {len} bytes, over the {MAX_PAYLOAD} byte channel limit"
            ),
            FrameError::Io(error) => write!(f, "the channel pipe failed: {error}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<std::io::Error> for FrameError {
    fn from(error: std::io::Error) -> Self {
        FrameError::Io(error)
    }
}

/// Writes one frame, header and body, and flushes it.
///
/// The flush is not optional and not a tidiness measure. Both ends of this pipe
/// block waiting for the other, so a reply sitting in a `BufWriter` is a paste
/// that hangs until some later frame happens to push it out.
pub async fn write_frame<W>(writer: &mut W, frame: &Frame) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let header = Header {
        id: frame.id,
        len: frame.payload.len(),
    };
    let mut line =
        serde_json::to_string(&header).map_err(|error| FrameError::Malformed(error.to_string()))?;
    line.push('\n');

    writer.write_all(line.as_bytes()).await?;
    writer.write_all(&frame.payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one frame, or `None` at a clean end of pipe.
///
/// `None` means the peer closed between frames, which is an ordinary
/// disconnect — the laptop's lid, the shell exiting. A pipe that ends *inside*
/// a frame is an error instead: a half-read body is a truncated screenshot, and
/// returning it as if it were complete is the one failure worse than no paste.
pub async fn read_frame<R>(reader: &mut R) -> Result<Option<Frame>, FrameError>
where
    R: AsyncBufRead + AsyncRead + Unpin,
{
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    if line.trim().is_empty() {
        return Err(FrameError::Malformed("an empty header line".into()));
    }

    let header: Header = serde_json::from_str(line.trim())
        .map_err(|error| FrameError::Malformed(format!("{error}: {}", line.trim())))?;

    // Checked before anything is sized by it, so a broken or hostile peer
    // cannot make this reader reserve 4 GB — the same rule, for the same
    // reason, that `protocol::decode_request` applies to its own bodies.
    if header.len > MAX_PAYLOAD {
        return Err(FrameError::TooLarge(header.len));
    }

    let mut payload = vec![0u8; header.len];
    reader.read_exact(&mut payload).await?;

    Ok(Some(Frame {
        id: header.id,
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    async fn round_trip(frames: &[Frame]) -> Vec<Frame> {
        let mut buffer = Vec::new();
        for frame in frames {
            write_frame(&mut buffer, frame).await.expect("write");
        }

        let mut reader = BufReader::new(buffer.as_slice());
        let mut read = Vec::new();
        while let Some(frame) = read_frame(&mut reader).await.expect("read") {
            read.push(frame);
        }
        read
    }

    /// The framing is the whole risk this module carries: a body holding a
    /// newline and a byte that is not valid UTF-8 is exactly what a reader
    /// framing on lines or on strings would corrupt, and exactly what a PNG is.
    #[tokio::test]
    async fn a_body_with_newlines_and_invalid_utf8_survives_a_round_trip() {
        let frame = Frame {
            id: 7,
            payload: b"first\nsecond\xFF\x00\n".to_vec(),
        };
        assert_eq!(
            round_trip(std::slice::from_ref(&frame)).await,
            vec![frame.clone()]
        );
    }

    /// Two connections in flight at once. The ids are the only thing keeping
    /// one shell's screenshot out of another shell's paste.
    #[tokio::test]
    async fn frames_for_different_connections_keep_their_ids_and_order() {
        let frames = vec![
            Frame {
                id: 1,
                payload: b"one".to_vec(),
            },
            Frame {
                id: 2,
                payload: b"two".to_vec(),
            },
            Frame {
                id: 1,
                payload: b"one again".to_vec(),
            },
        ];
        assert_eq!(round_trip(&frames).await, frames);
    }

    /// An empty payload is a legitimate frame — every response with no body is
    /// one — and must not read as end of pipe.
    #[tokio::test]
    async fn an_empty_payload_is_a_frame_rather_than_the_end() {
        let frames = vec![
            Frame {
                id: 3,
                payload: Vec::new(),
            },
            Frame {
                id: 3,
                payload: b"after".to_vec(),
            },
        ];
        assert_eq!(round_trip(&frames).await, frames);
    }

    #[tokio::test]
    async fn a_clean_end_of_pipe_is_none_rather_than_an_error() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_frame(&mut reader).await.expect("read").is_none());
    }

    /// Refused from the header alone, before the body is sized by it.
    #[tokio::test]
    async fn a_length_over_the_cap_is_refused_before_it_is_allocated() {
        let line = format!("{{\"id\":1,\"len\":{}}}\n", MAX_PAYLOAD + 1);
        let mut reader = BufReader::new(line.as_bytes());
        let error = read_frame(&mut reader).await.expect_err("over the cap");
        assert!(
            matches!(error, FrameError::TooLarge(_)),
            "{error} should name the cap"
        );
    }

    /// A pipe that ends mid-body must not hand back a short payload: that is a
    /// truncated screenshot Claude Code would accept as complete.
    #[tokio::test]
    async fn a_body_shorter_than_its_header_promised_is_an_error() {
        let mut reader = BufReader::new(&b"{\"id\":1,\"len\":64}\nshort"[..]);
        assert!(read_frame(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn a_header_that_is_not_json_is_an_error_rather_than_a_panic() {
        let mut reader = BufReader::new(&b"not json\n"[..]);
        let error = read_frame(&mut reader).await.expect_err("malformed");
        assert!(matches!(error, FrameError::Malformed(_)), "{error}");
    }
}

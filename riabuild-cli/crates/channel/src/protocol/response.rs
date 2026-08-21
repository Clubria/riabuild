//! What the laptop sends back.
//!
//! A JSON header line, followed for a binary payload by exactly the announced
//! number of raw bytes. Length-prefixed and streamed rather than base64: a
//! screenshot is routinely 2–15 MB, and base64 would inflate it by a third for
//! no benefit.
//!
//! Every bodiless success carries its own discriminator rather than sharing the
//! bare `ok`, because `decode_response` reads a header with no other field as
//! `Pong` — and a reply that decoded as a ping answer would report work the
//! laptop never did.

use super::{MAX_PAYLOAD, ProtocolError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Targets(Vec<String>),
    /// A header announcing `len` raw bytes to follow on the same stream.
    Payload {
        len: usize,
    },
    /// A write reached the laptop's clipboard.
    ///
    /// Distinct from `Pong` on the wire rather than sharing the bare `ok`,
    /// because these two are the only replies with no body and the channel log
    /// is the only place a developer can see which one came back.
    Written,
    /// A URL reached the laptop's browser.
    ///
    /// Carries its own `opened` discriminator on the wire rather than sharing
    /// the bare `ok`, because `decode_response` reads a header with no other
    /// field as `Pong` — without it, "opened" would come back as a ping answer.
    Opened,
    Pong,
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadRequest,
    Unsupported,
    Unavailable,
    TooLarge,
    Internal,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::Internal => "internal",
        }
    }

    fn parse(code: &str) -> ErrorCode {
        match code {
            "bad_request" => ErrorCode::BadRequest,
            "unsupported" => ErrorCode::Unsupported,
            "unavailable" => ErrorCode::Unavailable,
            "too_large" => ErrorCode::TooLarge,
            // An unrecognised code from a newer peer is still an error, and
            // treating it as one is more useful than failing to parse the very
            // reply that says so.
            _ => ErrorCode::Internal,
        }
    }
}

/// The JSON shape of a response header line.
#[derive(Debug, Serialize, Deserialize)]
struct ResponseLine {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    targets: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    written: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opened: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

pub fn encode_response(response: &Response) -> String {
    let line = match response {
        Response::Targets(targets) => ResponseLine {
            ok: true,
            targets: Some(targets.clone()),
            written: None,
            opened: None,
            len: None,
            code: None,
            message: None,
        },
        Response::Payload { len } => ResponseLine {
            ok: true,
            targets: None,
            written: None,
            opened: None,
            len: Some(*len),
            code: None,
            message: None,
        },
        Response::Written => ResponseLine {
            ok: true,
            targets: None,
            written: Some(true),
            opened: None,
            len: None,
            code: None,
            message: None,
        },
        Response::Opened => ResponseLine {
            ok: true,
            targets: None,
            written: None,
            opened: Some(true),
            len: None,
            code: None,
            message: None,
        },
        Response::Pong => ResponseLine {
            ok: true,
            targets: None,
            written: None,
            opened: None,
            len: None,
            code: None,
            message: None,
        },
        Response::Error { code, message } => ResponseLine {
            ok: false,
            targets: None,
            written: None,
            opened: None,
            len: None,
            code: Some(code.as_str().to_string()),
            message: Some(message.clone()),
        },
    };
    let json = serde_json::to_string(&line).unwrap_or_default();
    format!("{json}\n")
}

pub fn decode_response(line: &str) -> Result<Response, ProtocolError> {
    let parsed: ResponseLine = serde_json::from_str(line.trim())
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;

    if !parsed.ok {
        return Ok(Response::Error {
            code: ErrorCode::parse(parsed.code.as_deref().unwrap_or("internal")),
            message: parsed
                .message
                .unwrap_or_else(|| "the laptop refused the request".into()),
        });
    }

    if let Some(targets) = parsed.targets {
        return Ok(Response::Targets(targets));
    }

    if parsed.written == Some(true) {
        return Ok(Response::Written);
    }

    // Before the `len` match below, whose `None` arm is `Pong`: an `Opened`
    // that fell through to it would come back as a ping answer, and the shim
    // would report a link opened on a laptop that never saw it.
    if parsed.opened == Some(true) {
        return Ok(Response::Opened);
    }

    match parsed.len {
        // Checked here so the cap is enforced before a reader allocates by it.
        Some(len) if len > MAX_PAYLOAD => Err(ProtocolError::TooLarge(len)),
        Some(len) => Ok(Response::Payload { len }),
        None => Ok(Response::Pong),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Opened` and `Pong` are both bodiless successes. Without its own
    /// discriminator `Opened` decodes as `Pong`, and the shim reports a link
    /// opened on a laptop that never saw it.
    #[test]
    fn opened_does_not_come_back_as_a_ping_answer() {
        let line = encode_response(&Response::Opened);
        assert_eq!(decode_response(&line).unwrap(), Response::Opened);
        assert_eq!(
            decode_response(&encode_response(&Response::Pong)).unwrap(),
            Response::Pong
        );
    }

    /// A write ack and a pong are the only two replies with no body, and the
    /// channel log is the only place a developer sees which came back.
    #[test]
    fn a_write_acknowledgement_is_distinguishable_from_a_pong() {
        let line = encode_response(&Response::Written);
        assert_eq!(line, "{\"ok\":true,\"written\":true}\n");
        assert_eq!(decode_response(&line).unwrap(), Response::Written);
        assert_ne!(
            decode_response(&encode_response(&Response::Pong)).unwrap(),
            Response::Written
        );
    }

    #[test]
    fn a_targets_response_round_trips() {
        let response = Response::Targets(vec!["image/png".into(), "text/html".into()]);
        let line = encode_response(&response);
        assert!(line.contains("\"ok\":true"), "{line}");
        assert_eq!(decode_response(&line).unwrap(), response);
    }

    #[test]
    fn a_payload_response_announces_its_length_before_the_bytes() {
        let line = encode_response(&Response::Payload { len: 184_320 });
        assert_eq!(line, "{\"ok\":true,\"len\":184320}\n");
        assert_eq!(
            decode_response(&line).unwrap(),
            Response::Payload { len: 184_320 }
        );
    }

    #[test]
    fn an_error_response_round_trips_with_its_code() {
        let response = Response::Error {
            code: ErrorCode::Unavailable,
            message: "no clipboard content of that type".into(),
        };
        let line = encode_response(&response);
        assert!(line.contains("\"ok\":false"), "{line}");
        assert!(line.contains("\"code\":\"unavailable\""), "{line}");
        assert_eq!(decode_response(&line).unwrap(), response);
    }

    #[test]
    fn a_ping_is_answered_with_a_bare_ok() {
        let line = encode_response(&Response::Pong);
        assert_eq!(line, "{\"ok\":true}\n");
        assert_eq!(decode_response(&line).unwrap(), Response::Pong);
    }

    /// A length past the cap is refused at decode time, before anything sized
    /// by it is allocated. A malicious or broken peer must not be able to make
    /// the reader reserve 4 GB.
    #[test]
    fn a_payload_over_the_cap_is_refused_before_it_is_allocated() {
        let line = format!("{{\"ok\":true,\"len\":{}}}\n", MAX_PAYLOAD + 1);
        assert!(matches!(
            decode_response(&line),
            Err(ProtocolError::TooLarge(_))
        ));

        // Exactly the cap is allowed: the boundary belongs on the legal side.
        let line = format!("{{\"ok\":true,\"len\":{MAX_PAYLOAD}}}\n");
        assert_eq!(
            decode_response(&line).unwrap(),
            Response::Payload { len: MAX_PAYLOAD }
        );
    }
}

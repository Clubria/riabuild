//! The wire format, and the operation allowlist.
//!
//! Requests are newline-delimited JSON. Responses are a JSON header line
//! followed, for binary payloads, by exactly the announced number of raw
//! bytes. Length-prefixed and streamed rather than base64: a screenshot is
//! routinely 2–15 MB, and base64 would inflate it by a third for no benefit.
//!
//! `decode_request` is deliberately an explicit `match` over operation names
//! rather than a serde-derived enum. The property it enforces — a server can
//! request only what the laptop's binary already implements — is the reason
//! the whole design is defensible, and it should be one readable function
//! rather than an emergent consequence of derive attributes.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u8 = 1;

/// The largest payload the channel will move, in either direction.
///
/// Refused at decode time, before anything sized by the announced length is
/// allocated, so a broken or hostile peer cannot make the reader reserve 4 GB.
pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    ClipboardTargets,
    ClipboardRead { mime: String },
    ChannelPing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Targets(Vec<String>),
    /// A header announcing `len` raw bytes to follow on the same stream.
    Payload {
        len: usize,
    },
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

#[derive(Debug)]
pub enum ProtocolError {
    Malformed(String),
    UnknownOp(String),
    UnsupportedVersion(u8),
    MissingField(&'static str),
    TooLarge(usize),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Malformed(detail) => write!(
                f,
                "the channel received a line that is not valid JSON: {detail}"
            ),
            ProtocolError::UnknownOp(op) => write!(
                f,
                "`{op}` is not an operation this riabuild implements; the operation set is compiled in"
            ),
            ProtocolError::UnsupportedVersion(v) => write!(
                f,
                "the channel speaks protocol version {PROTOCOL_VERSION}, and the peer asked for {v}"
            ),
            ProtocolError::MissingField(field) => {
                write!(f, "the request is missing its `{field}` field")
            }
            ProtocolError::TooLarge(len) => write!(
                f,
                "the payload is {len} bytes, over the {MAX_PAYLOAD} byte channel limit"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// The JSON shape of a request line. Parsed permissively, then narrowed by
/// `decode_request` into the compiled-in operation set.
#[derive(Debug, Serialize, Deserialize)]
struct RequestLine {
    v: u8,
    op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
}

pub fn encode_request(request: &Request) -> String {
    let line = match request {
        Request::ClipboardTargets => RequestLine {
            v: PROTOCOL_VERSION,
            op: "clipboard.targets".into(),
            mime: None,
        },
        Request::ClipboardRead { mime } => RequestLine {
            v: PROTOCOL_VERSION,
            op: "clipboard.read".into(),
            mime: Some(mime.clone()),
        },
        Request::ChannelPing => RequestLine {
            v: PROTOCOL_VERSION,
            op: "channel.ping".into(),
            mime: None,
        },
    };
    // Serialising a struct of owned scalars cannot fail; the fallback keeps the
    // deny-by-default `unwrap_used` lint satisfied without ceremony.
    let json = serde_json::to_string(&line).unwrap_or_default();
    format!("{json}\n")
}

/// The allowlist.
///
/// Everything a server may ask for is named here. Anything else is refused by
/// name and never attempted.
pub fn decode_request(line: &str) -> Result<Request, ProtocolError> {
    let parsed: RequestLine = serde_json::from_str(line.trim())
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;

    if parsed.v != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(parsed.v));
    }

    match parsed.op.as_str() {
        "clipboard.targets" => Ok(Request::ClipboardTargets),
        "channel.ping" => Ok(Request::ChannelPing),
        "clipboard.read" => match parsed.mime {
            Some(mime) => Ok(Request::ClipboardRead { mime }),
            None => Err(ProtocolError::MissingField("mime")),
        },
        other => Err(ProtocolError::UnknownOp(other.to_string())),
    }
}

/// The JSON shape of a response header line.
#[derive(Debug, Serialize, Deserialize)]
struct ResponseLine {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    targets: Option<Vec<String>>,
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
            len: None,
            code: None,
            message: None,
        },
        Response::Payload { len } => ResponseLine {
            ok: true,
            targets: None,
            len: Some(*len),
            code: None,
            message: None,
        },
        Response::Pong => ResponseLine {
            ok: true,
            targets: None,
            len: None,
            code: None,
            message: None,
        },
        Response::Error { code, message } => ResponseLine {
            ok: false,
            targets: None,
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

    #[test]
    fn a_targets_request_is_one_json_line() {
        let line = encode_request(&Request::ClipboardTargets);
        assert_eq!(line, "{\"v\":1,\"op\":\"clipboard.targets\"}\n");
        assert_eq!(decode_request(&line).unwrap(), Request::ClipboardTargets);
    }

    #[test]
    fn a_read_request_carries_its_mime_type() {
        let request = Request::ClipboardRead {
            mime: "image/png".into(),
        };
        let line = encode_request(&request);
        assert!(line.contains("\"op\":\"clipboard.read\""), "{line}");
        assert!(line.contains("\"mime\":\"image/png\""), "{line}");
        assert_eq!(decode_request(&line).unwrap(), request);
    }

    #[test]
    fn every_request_ends_in_a_newline_so_the_reader_knows_where_it_stops() {
        for request in [
            Request::ClipboardTargets,
            Request::ChannelPing,
            Request::ClipboardRead {
                mime: "text/html".into(),
            },
        ] {
            assert!(encode_request(&request).ends_with('\n'));
        }
    }

    /// The allowlist is the security property of the whole design: the server
    /// asks and the laptop decides. An op the binary does not implement is
    /// refused by name, not attempted.
    #[test]
    fn an_operation_outside_the_allowlist_is_refused() {
        let line = r#"{"v":1,"op":"clipboard.write","data":"x"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::UnknownOp(op)) if op == "clipboard.write"
        ));
    }

    #[test]
    fn shell_shaped_operations_are_refused_like_any_other_unknown_op() {
        for op in ["exec", "channel.exec", "clipboard.targets;rm -rf /"] {
            let line = format!(r#"{{"v":1,"op":"{op}"}}"#);
            assert!(
                matches!(decode_request(&line), Err(ProtocolError::UnknownOp(_))),
                "{op} was not refused"
            );
        }
    }

    #[test]
    fn a_future_protocol_version_is_refused_rather_than_guessed_at() {
        let line = r#"{"v":2,"op":"clipboard.targets"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn a_read_without_a_mime_type_is_a_missing_field_not_a_panic() {
        let line = r#"{"v":1,"op":"clipboard.read"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::MissingField("mime"))
        ));
    }

    #[test]
    fn malformed_json_is_an_error_rather_than_a_crash() {
        for line in ["", "not json", "{\"v\":1", "[]", "null"] {
            assert!(
                matches!(decode_request(line), Err(ProtocolError::Malformed(_))),
                "{line:?} was not rejected"
            );
        }
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

    #[test]
    fn the_cap_is_thirty_two_megabytes() {
        assert_eq!(MAX_PAYLOAD, 32 * 1024 * 1024);
    }

    /// Every error a caller can trigger must produce a message worth reading:
    /// these strings end up in the channel log, which is the only place a
    /// developer can find out why paste stopped working.
    #[test]
    fn every_protocol_error_describes_itself() {
        let errors = [
            ProtocolError::Malformed("bad".into()),
            ProtocolError::UnknownOp("clipboard.write".into()),
            ProtocolError::UnsupportedVersion(2),
            ProtocolError::MissingField("mime"),
            ProtocolError::TooLarge(99),
        ];
        for error in errors {
            let text = error.to_string();
            assert!(text.len() > 10, "{text:?} is not a useful message");
        }
    }
}

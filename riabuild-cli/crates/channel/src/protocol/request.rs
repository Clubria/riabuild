//! The allowlist, and the line a server sends to reach it.
//!
//! `decode_request` is deliberately an explicit `match` over operation names
//! rather than a serde-derived enum. The property it enforces — a server can
//! request only what the laptop's binary already implements — is the reason
//! the whole design is defensible, and it should be one readable function
//! rather than an emergent consequence of derive attributes.
//!
//! Both refusals that are not merely "unknown operation" live here too: a
//! payload past the cap, refused before anything sized by it is allocated, and
//! a URL scheme this channel does not carry.

use super::{MAX_PAYLOAD, PROTOCOL_VERSION, ProtocolError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    ClipboardTargets,
    ClipboardRead {
        mime: String,
    },
    /// A header announcing `len` raw bytes to follow on the same stream — the
    /// mirror of `Response::Payload`, in the other direction.
    ///
    /// The only operation that carries a body, and the only one that changes
    /// the laptop rather than reporting on it.
    ClipboardWrite {
        mime: String,
        len: usize,
    },
    /// Open a URL in the laptop's own browser.
    ///
    /// The second operation that changes the laptop rather than reporting on
    /// it, and the first that reaches past the laptop into whatever application
    /// claims the scheme. `is_openable` is why that is defensible: this variant
    /// cannot be constructed by `decode_request` for anything but http and
    /// https.
    OpenUrl {
        url: String,
    },
    ChannelPing,
}

/// The scheme rule, and the whole security boundary of `browser.open`.
///
/// `open(1)` on macOS dispatches by scheme to whichever application registered
/// it, so `file://`, `vscode://` and `slack://` are not links — they are calls
/// into a local application with a payload the server chose. The URL that
/// reaches this operation was picked by a model reading a repository, so
/// "the server would not send that" is not an assumption available to us.
///
/// Checked on the laptop, before the request becomes a `Request` at all. The
/// server checks too, but only so a mistake produces a local message instead of
/// a round trip; this copy is the one that decides.
pub fn is_openable(url: &str) -> bool {
    let lowered = url.to_ascii_lowercase();
    let rest = match () {
        _ if lowered.starts_with("https://") => &url["https://".len()..],
        _ if lowered.starts_with("http://") => &url["http://".len()..],
        // Anything else, `javascript:` and `file:` included, is not a link this
        // channel carries.
        _ => return false,
    };

    // A scheme with nothing after it is not a URL, and `xdg-open ""` is an
    // error rather than a no-op.
    if rest.is_empty() {
        return false;
    }

    // Whitespace and control characters would either break the newline framing
    // this protocol is built on or arrive at the opener as a second argument.
    !url.chars().any(|c| c.is_control() || c == ' ')
}

/// The JSON shape of a request line. Parsed permissively, then narrowed by
/// `decode_request` into the compiled-in operation set.
#[derive(Debug, Serialize, Deserialize)]
struct RequestLine {
    v: u8,
    op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
    /// Present only on `clipboard.write`: how many raw bytes follow the line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    len: Option<usize>,
    /// Present only on `browser.open`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

pub fn encode_request(request: &Request) -> String {
    let line = match request {
        Request::ClipboardTargets => RequestLine {
            v: PROTOCOL_VERSION,
            op: "clipboard.targets".into(),
            mime: None,
            len: None,
            url: None,
        },
        Request::ClipboardRead { mime } => RequestLine {
            v: PROTOCOL_VERSION,
            op: "clipboard.read".into(),
            mime: Some(mime.clone()),
            len: None,
            url: None,
        },
        Request::ClipboardWrite { mime, len } => RequestLine {
            v: PROTOCOL_VERSION,
            op: "clipboard.write".into(),
            mime: Some(mime.clone()),
            len: Some(*len),
            url: None,
        },
        Request::OpenUrl { url } => RequestLine {
            v: PROTOCOL_VERSION,
            op: "browser.open".into(),
            mime: None,
            len: None,
            url: Some(url.clone()),
        },
        Request::ChannelPing => RequestLine {
            v: PROTOCOL_VERSION,
            op: "channel.ping".into(),
            mime: None,
            len: None,
            url: None,
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
        "clipboard.write" => {
            let mime = parsed.mime.ok_or(ProtocolError::MissingField("mime"))?;
            let len = parsed.len.ok_or(ProtocolError::MissingField("len"))?;
            // Refused here, before the reader allocates or reads by it. This is
            // the only inbound length a peer chooses, so it is the only one that
            // could make the laptop reserve 4 GB on request.
            if len > MAX_PAYLOAD {
                return Err(ProtocolError::TooLarge(len));
            }
            Ok(Request::ClipboardWrite { mime, len })
        }
        "browser.open" => {
            let url = parsed.url.ok_or(ProtocolError::MissingField("url"))?;
            // Refused here, so no caller downstream is ever handed a `OpenUrl`
            // holding a scheme the laptop should not dispatch. The check is not
            // repeated in the agent because it cannot be reached without one.
            if !is_openable(&url) {
                return Err(ProtocolError::UnsupportedScheme(url));
            }
            Ok(Request::OpenUrl { url })
        }
        other => Err(ProtocolError::UnknownOp(other.to_string())),
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
    fn an_open_request_round_trips_its_url() {
        let request = Request::OpenUrl {
            url: "https://github.com/login/device".into(),
        };
        let line = encode_request(&request);
        assert!(line.contains("\"op\":\"browser.open\""), "{line}");
        assert_eq!(decode_request(&line).unwrap(), request);
    }

    /// The security boundary. `open(1)` dispatches by scheme to whatever
    /// application claims it, so these are not links — they are calls into a
    /// local application with a payload the server chose.
    #[test]
    fn only_http_and_https_survive_decoding() {
        for url in [
            "file:///etc/passwd",
            "vscode://ms-vscode.remote/x",
            "javascript:alert(1)",
            "slack://open?team=T1",
            "HtTpS:/evil",
            "ftp://example.com",
            // A scheme with nothing after it.
            "https://",
            // Whitespace would arrive at the opener as a second argument.
            "https://example.com/a b",
        ] {
            assert!(!is_openable(url), "{url} should not be openable");

            let line = format!(
                "{{\"v\":1,\"op\":\"browser.open\",\"url\":{}}}\n",
                serde_json::to_string(url).unwrap()
            );
            assert!(
                matches!(
                    decode_request(&line),
                    Err(ProtocolError::UnsupportedScheme(_))
                ),
                "{url} decoded into a request"
            );
        }
    }

    #[test]
    fn ordinary_links_are_openable_whatever_the_case_of_the_scheme() {
        for url in [
            "http://example.com",
            "https://example.com",
            "HTTPS://EXAMPLE.COM/Path?q=1#frag",
            "https://localhost:3000/callback?code=abc",
        ] {
            assert!(is_openable(url), "{url} should be openable");
        }
    }

    /// A control character would break the newline framing the whole protocol
    /// rests on.
    #[test]
    fn a_url_carrying_a_newline_is_refused() {
        assert!(!is_openable("https://example.com/\nclipboard.read"));
        assert!(!is_openable("https://example.com/\u{7}"));
    }

    #[test]
    fn an_open_request_without_a_url_names_the_missing_field() {
        let line = "{\"v\":1,\"op\":\"browser.open\"}\n";
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::MissingField("url"))
        ));
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
        let line = r#"{"v":1,"op":"clipboard.clear"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::UnknownOp(op)) if op == "clipboard.clear"
        ));
    }

    #[test]
    fn a_write_request_carries_its_type_and_its_length() {
        let request = Request::ClipboardWrite {
            mime: "image/png".into(),
            len: 4,
        };
        let line = encode_request(&request);
        assert!(line.contains("\"op\":\"clipboard.write\""), "{line}");
        assert!(line.contains("\"len\":4"), "{line}");
        assert_eq!(decode_request(&line).unwrap(), request);
    }

    /// A write with no length cannot be framed: the reader would not know where
    /// the body stops and the next request begins.
    #[test]
    fn a_write_without_a_length_is_a_missing_field() {
        let line = r#"{"v":1,"op":"clipboard.write","mime":"text/plain;charset=utf-8"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::MissingField("len"))
        ));
    }

    #[test]
    fn a_write_without_a_type_is_a_missing_field() {
        let line = r#"{"v":1,"op":"clipboard.write","len":4}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::MissingField("mime"))
        ));
    }

    /// The inbound direction is the only one where a peer chooses how much the
    /// laptop allocates, so the cap is enforced before anything is read.
    #[test]
    fn an_oversized_write_is_refused_before_its_body_is_read() {
        let line = format!(
            r#"{{"v":1,"op":"clipboard.write","mime":"image/png","len":{}}}"#,
            MAX_PAYLOAD + 1
        );
        assert!(matches!(
            decode_request(&line),
            Err(ProtocolError::TooLarge(_))
        ));

        // Exactly the cap is legal: the boundary belongs on the legal side.
        let line =
            format!(r#"{{"v":1,"op":"clipboard.write","mime":"image/png","len":{MAX_PAYLOAD}}}"#);
        assert!(matches!(
            decode_request(&line),
            Ok(Request::ClipboardWrite { len, .. }) if len == MAX_PAYLOAD
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
}

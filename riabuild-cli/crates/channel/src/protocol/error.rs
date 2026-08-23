//! Every way a line can be refused, and the sentence each one reads as.
//!
//! These strings end up in the channel log, which is the only place a developer
//! can find out why paste stopped working — so each variant has to describe
//! itself rather than name a code somebody else has to look up.

use super::{MAX_PAYLOAD, PROTOCOL_VERSION};

#[derive(Debug)]
pub enum ProtocolError {
    Malformed(String),
    UnknownOp(String),
    UnsupportedVersion(u8),
    MissingField(&'static str),
    TooLarge(usize),
    UnsupportedScheme(String),
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
            ProtocolError::UnsupportedScheme(url) => write!(
                f,
                "the channel opens http and https links only, and `{url}` is neither"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

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

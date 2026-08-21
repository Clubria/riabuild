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
//!
//! Three files, in the two directions and the vocabulary they share. `request`
//! is the allowlist and the line a server sends; `response` is what the laptop
//! sends back; `error` is every way either of them can be refused. What stays
//! here is what both halves are measured against — the version they agree on,
//! and the cap neither may exceed.

mod error;
mod request;
mod response;

// Re-exported so every caller keeps naming `protocol::…`. Which file each half
// lives in is this module's business, and a caller that had to know would have
// to be edited the next time one moves.
pub use error::ProtocolError;
pub use request::{Request, decode_request, encode_request, is_openable};
pub use response::{ErrorCode, Response, decode_response, encode_response};

pub const PROTOCOL_VERSION: u8 = 1;

/// The largest payload the channel will move, in either direction.
///
/// Refused at decode time, before anything sized by the announced length is
/// allocated, so a broken or hostile peer cannot make the reader reserve 4 GB.
pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_is_thirty_two_megabytes() {
        assert_eq!(MAX_PAYLOAD, 32 * 1024 * 1024);
    }
}

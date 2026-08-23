//! Reading one header line, with a bound on it.
//!
//! Three readers in this crate frame on a header line before they know how many
//! bytes follow: `mux::read_frame` over the pipe to the laptop, `client` over
//! the shim's socket, and `agent::server` over the laptop's own socket. All
//! three used `read_line`, which grows its buffer until it meets a newline or
//! runs out of memory — so [`protocol::MAX_PAYLOAD`](crate::protocol::MAX_PAYLOAD)
//! bounded every *body* on the channel and nothing bounded the thing that
//! announces one. A peer streaming bytes with no `\n` in them made the reader
//! allocate without limit, on either end, and the comment above the cap in
//! `mux` claimed the opposite.
//!
//! The bound is deliberately generous rather than tight. A header is a short
//! JSON object, but one field of it is an error *message* — a clipboard tool's
//! stderr, an `anyhow` chain — and truncating a legitimate reply is a broken
//! paste rather than a refused attack. [`MAX_HEADER`] is three orders of
//! magnitude under the payload cap and still far past anything riabuild writes.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// The most one header line may run to before the reader refuses it.
pub const MAX_HEADER: usize = 64 * 1024;

/// One `\n`-terminated header line, or `None` at a clean end of stream.
///
/// The newline is kept, because every caller hands the line to a decoder that
/// trims it and one that did not would be reading a different string than the
/// `read_line` this replaces returned.
///
/// A stream that ends *without* a newline returns what it carried, exactly as
/// before: that is a peer that closed mid-header, and the decoder above says so
/// in the caller's own words. What is new is the refusal — [`std::io::ErrorKind::InvalidData`]
/// once the line passes [`MAX_HEADER`] with no newline in it, which is the one
/// case that used to have no end.
pub async fn read_header_line<R>(reader: &mut R) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut buffer = Vec::new();
    // One byte past the cap, so a line of exactly `MAX_HEADER` bytes plus its
    // newline still reads as a line rather than as an overrun.
    let read = (&mut *reader)
        .take(MAX_HEADER as u64 + 1)
        .read_until(b'\n', &mut buffer)
        .await?;

    if read == 0 {
        return Ok(None);
    }
    if !buffer.ends_with(b"\n") && read > MAX_HEADER {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a header line ran past {MAX_HEADER} bytes without a newline"),
        ));
    }

    String::from_utf8(buffer).map(Some).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a header line is not valid UTF-8",
        )
    })
}

/// Whether this is the refusal above rather than a pipe that broke.
///
/// The two want different words from a caller — one is a peer sending
/// something it should not, the other is a connection going away — and
/// `ErrorKind` is the only thing that separates them once the error has been
/// boxed.
pub fn is_overrun(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::InvalidData
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn a_line_is_returned_with_its_newline() {
        let mut reader = BufReader::new(&b"{\"id\":1}\nrest"[..]);
        assert_eq!(
            read_header_line(&mut reader).await.expect("read"),
            Some("{\"id\":1}\n".to_string())
        );
    }

    #[tokio::test]
    async fn a_clean_end_of_stream_is_none() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_header_line(&mut reader).await.expect("read").is_none());
    }

    /// A peer that closed mid-header is not the attack this bounds, and the
    /// decoder above says what is wrong with the fragment in its own words.
    #[tokio::test]
    async fn a_stream_that_ends_without_a_newline_returns_what_it_carried() {
        let mut reader = BufReader::new(&b"not json"[..]);
        assert_eq!(
            read_header_line(&mut reader).await.expect("read"),
            Some("not json".to_string())
        );
    }

    /// The bug. A peer that never sends a newline used to make the reader grow
    /// its buffer for as long as bytes kept arriving, on whichever end of the
    /// channel it was pointed at.
    #[tokio::test]
    async fn a_header_that_never_ends_is_refused_rather_than_allocated() {
        let flood = vec![b'a'; MAX_HEADER * 2];
        let mut reader = BufReader::new(flood.as_slice());
        let error = read_header_line(&mut reader)
            .await
            .expect_err("an endless header must be refused");
        assert!(is_overrun(&error), "{error}");
        assert!(error.to_string().contains(&MAX_HEADER.to_string()));
    }

    /// The boundary, both sides of it: a header that fits exactly is still a
    /// header, and the byte past it is not.
    #[tokio::test]
    async fn the_cap_admits_a_line_of_exactly_its_length() {
        let mut line = vec![b'x'; MAX_HEADER - 1];
        line.push(b'\n');
        let mut reader = BufReader::new(line.as_slice());
        let read = read_header_line(&mut reader)
            .await
            .expect("read")
            .expect("a line");
        assert_eq!(read.len(), MAX_HEADER);
    }
}

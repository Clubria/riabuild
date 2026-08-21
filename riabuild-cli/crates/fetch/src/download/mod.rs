//! Fetching and verifying the distributions riabuild owns: where each release
//! lives, what its asset is called on this platform, and the sha256 that says
//! the bytes are the ones upstream published.
//!
//! Where the bytes come from and whether they are the right bytes. Unpacking
//! them is `archive.rs`, which only ever sees a buffer that has already matched
//! a published digest; landing a verified tree at its final path is `staging`'s.
//!
//! riabuild owns its Node rather than driving nvm: nvm is a bash function, not a
//! binary, so Rust cannot drive it without spawning a login shell, it does not
//! work in fish, and sourcing it costs every shell start 200 ms to 1 s. corepack
//! is not an option either — it was removed from Node.js 25+ distributions.
//! Owning the tarball is a few dozen lines and removes a class of
//! works-in-my-shell failures.
//!
//! pnpm comes from the **npm registry** rather than from pnpm's GitHub
//! releases, which publish no checksum file: `dist.integrity` is a digest the
//! publisher recorded, served with no API budget to run out of. See
//! `NPM_REGISTRY` in `assets`.
//!
//! The same reasoning extends to `gh` and `infisical` — see `tools.rs`, which
//! describes where their releases live and what the assets are called.
//!
//! Three files, split where the questions are different. `assets` is where
//! each release lives and what its asset is called on this platform; `digest`
//! is what the bytes have to hash to; and this file is the transfer itself —
//! the one place bytes cross the network, and every failure a developer can be
//! told about on the way.

mod assets;
mod digest;

// Re-exported so every caller keeps naming `download::…`. Which file each
// answer lives in is this module's business, and a caller that had to know
// would have to be edited the next time one moves.
pub use assets::{
    PNPM_ENTRY, PNPM_PACKAGE, node_platform, node_shasums_url, node_tarball_name, node_tarball_url,
    npm_metadata_url, npm_tarball_url, riabuild_asset, riabuild_asset_url, riabuild_checksums_url,
    rust_target,
};
pub use digest::{
    digest_for, digest_from_any, npm_integrity, npm_integrity_digest, sha256_hex, sha512,
};

use crate::{CHECK_THE_NETWORK, Failure, TELL_YOUR_LEAD, UPSTREAM_MOVED};
use anyhow::Result;
use std::time::Duration;

/// The ceiling on a body riabuild will hold in memory.
///
/// Consulted twice, because neither answer is sufficient on its own: against
/// `Content-Length` before a byte is read, which is what lets an absurd asset
/// be refused without allocating for it, and against what has actually arrived
/// as each chunk lands, because the header is a claim and a chunked response
/// carries none at all. It used to be neither — `response.bytes()` buffered the
/// whole body and the cap was compared against the allocation it exists to
/// prevent.
const MAX_DOWNLOAD: u64 = 400 * 1024 * 1024;

/// Reaching the host, not reading from it.
///
/// One deadline used to cover both, and the body is the half that legitimately
/// takes minutes — see [`READ_TIMEOUT`]. Split apart, this one can be short
/// enough that a host riabuild cannot reach says so rather than looking like a
/// hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The gap riabuild will tolerate between two chunks — **not** a deadline on
/// the whole body.
///
/// A single 300 s timeout over the complete response is a bandwidth floor in
/// disguise: Node is around 130 MB, so every link below roughly 450 KB/s failed
/// a download that was arriving perfectly steadily, and the developer was told
/// it was a bug in riabuild. Measured per read, a transfer still making
/// progress is never cut off, and one that has genuinely stalled still ends
/// inside a minute.
///
/// There is deliberately **no** deadline over the whole request beside it. A
/// hang has to present as a failure rather than as a slow run, and this is what
/// covers that: the way a download hangs is that the bytes stop, which is what
/// this measures. Restoring a total would only reintroduce the bandwidth floor
/// at a different number, and [`MAX_DOWNLOAD`] is what bounds how long a
/// transfer that never stalls can go on for.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Reads a whole distribution into memory.
///
/// Deliberately not streamed *to disk*: the sha256 in `verify` is checked
/// against the complete buffer **before** anything is extracted. Streaming to a
/// file would mean writing unverified bytes into a developer's toolchain
/// directory and checking them afterwards, which is a weaker property for a
/// tool that installs executables.
///
/// It is streamed into memory all the same, chunk by chunk, and that is a
/// different question: `response.bytes()` allocates the entire body and only
/// then hands it over, so [`MAX_DOWNLOAD`] was consulted *after* the allocation
/// it exists to prevent. Accumulating gives the cap somewhere to be enforced
/// while there is still something to refuse.
pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(|error| unreachable(url, &error))?;

    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| unreachable(url, &error))?
        .error_for_status()
        .map_err(|error| refused(url, &error))?;

    // A first chance to refuse, not the check itself: the header is upstream's
    // claim about a body riabuild has not read, and a chunked response carries
    // none at all.
    if let Some(claimed) = response.content_length()
        && claimed > MAX_DOWNLOAD
    {
        return Err(too_large(url, claimed));
    }

    // Reserving the claimed length turns a 130 MB download into one allocation
    // rather than two dozen, and the cap above is what keeps a hostile claim
    // from being a way to ask riabuild for memory.
    let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(0) as usize);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| cut_short(url, &error))?
    {
        let arrived = bytes.len() as u64 + chunk.len() as u64;
        if arrived > MAX_DOWNLOAD {
            return Err(too_large(url, arrived));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// riabuild never got an answer out of the host.
///
/// The three causes are always the same and naming them is the whole value: a
/// developer told to "check your network" on a laptop whose browser works fine
/// has been told nothing.
fn unreachable(url: &str, error: &reqwest::Error) -> anyhow::Error {
    Failure::new(format!("downloading {url}"), CHECK_THE_NETWORK)
        .detail(format!("{error}"))
        .into()
}

/// The host answered, and the answer was not the file. A 404 here is an
/// upstream release that has moved out from under a pin in this repository,
/// which is nothing the developer can fix and nothing a re-run will change.
fn refused(url: &str, error: &reqwest::Error) -> anyhow::Error {
    Failure::new(format!("downloading {url}"), UPSTREAM_MOVED)
        .detail(format!("{error}"))
        .into()
}

/// The body stopped arriving part way through — a dropped link, a proxy that
/// cut the connection, or a transfer that stalled past [`READ_TIMEOUT`].
fn cut_short(url: &str, error: &reqwest::Error) -> anyhow::Error {
    Failure::new(
        format!("downloading {url} — the transfer stopped part way through"),
        CHECK_THE_NETWORK,
    )
    .detail(format!("{error}"))
    .into()
}

fn too_large(url: &str, size: u64) -> anyhow::Error {
    Failure::new(
        format!(
            "downloading {url} — it is {size} bytes, more than the {MAX_DOWNLOAD} riabuild will \
             hold in memory"
        ),
        TELL_YOUR_LEAD,
    )
    .into()
}

pub async fn fetch_text(url: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&fetch_bytes(url).await?).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A loopback HTTP server that answers exactly one request with `head`
    /// followed by each of `body`, and then waits for the client to hang up.
    ///
    /// Written by hand rather than through a canned-response crate because
    /// what these tests need to control is the shape of the *response* — a
    /// `Content-Length` describing a body that is never sent, a body arriving
    /// in pieces — which is the layer such a crate hides.
    async fn serve_once(head: &'static str, body: &'static [&'static [u8]]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            if socket.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            for piece in body {
                if socket.write_all(piece).await.is_err() {
                    return;
                }
            }
            // Hold the connection open until the client is done with it, so
            // that a response the client refuses to read is not also a
            // connection that closed underneath it.
            let _ = socket.read(&mut request).await;
        });
        format!("http://{address}/node-v22.23.1-linux-x64.tar.gz")
    }

    #[tokio::test]
    async fn a_body_the_header_says_is_too_large_is_refused_before_it_is_read() {
        // The reported bug: `response.bytes()` buffered the whole body and
        // `MAX_DOWNLOAD` was compared against the allocation it exists to
        // prevent. This server sends a `Content-Length` of 8 GB and then no
        // body at all — the only way the call can return is by refusing on the
        // header, because there is nothing to read.
        let url = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 8589934592\r\n\r\n", &[]).await;
        let error = fetch_bytes(&url).await.expect_err("8 GB is too much");
        let failure = error
            .downcast_ref::<Failure>()
            .expect("a size refusal is something the developer can be told about");
        assert!(failure.attempting.contains("8589934592"), "{failure}");
        assert!(failure.attempting.contains("more than"), "{failure}");
    }

    #[tokio::test]
    async fn a_body_that_arrives_in_pieces_is_reassembled_whole() {
        // The other half of not calling `bytes()`: the cap is now enforced
        // while the body accumulates, so the accumulation itself has to be
        // right. A tarball arrives in as many chunks as the network feels like.
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n",
            &[b"node", b"-v22", b"1234"],
        )
        .await;
        assert_eq!(fetch_bytes(&url).await.unwrap(), b"node-v221234");
    }

    #[tokio::test]
    async fn a_host_that_answers_with_a_404_names_the_pin_rather_than_the_network() {
        // An upstream release that moved is not a connectivity problem, and
        // telling the developer to check their network would send them to look
        // at the one thing that is working.
        let url = serve_once("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n", &[]).await;
        let error = fetch_bytes(&url).await.expect_err("404");
        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(failure.action.contains("pinned to has moved"), "{failure}");
    }

    #[tokio::test]
    async fn a_connection_riabuild_cannot_make_is_not_reported_as_a_bug_in_riabuild() {
        // Port 1 on loopback, which nothing is listening on. Before this crate
        // could produce a `Failure` every one of these reached `main`'s
        // unknown-error branch and printed "it is a bug in riabuild" at a
        // developer whose VPN was down.
        let error = fetch_bytes("http://127.0.0.1:1/node.tar.gz")
            .await
            .expect_err("nothing is listening there");
        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(failure.action.contains("VPN"), "{failure}");
        assert!(!failure.detail.is_empty(), "the cause is still carried");
    }

    /// Proves this build can resolve a name, complete a TLS handshake, and
    /// read a real body.
    ///
    /// Ignored by default because it needs the network. CI runs it against the
    /// musl artefact, where it is the only thing standing between us and a
    /// static binary that builds, links, reports its version, and then cannot
    /// reach anything on a developer's machine — the two ways that happens are
    /// `rustls-tls-native-roots` finding no certificate store and musl's
    /// resolver behaving differently from glibc's, and neither is visible
    /// without actually making a request.
    #[tokio::test]
    #[ignore = "requires network; pins TLS and DNS for this build"]
    async fn tls_and_dns_work_on_this_build() {
        let shasums = fetch_text(&node_shasums_url("22.23.1"))
            .await
            .expect("fetch");
        assert!(
            digest_for(&shasums, "node-v22.23.1-linux-x64.tar.gz").is_some(),
            "reached nodejs.org but the body was not SHASUMS256.txt"
        );
    }
}

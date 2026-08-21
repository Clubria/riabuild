//! One shim connection, start to finish.
//!
//! Read the request whole, hand it up the pipe as a frame, wait for the frame
//! that answers it, write it back. Every bound a single connection has lives
//! here — how much it may buffer, how long it has to write, how long it may
//! wait for the laptop — because each of them is about one connection and
//! none of them is about the listener.

use crate::mux::Frame;
use crate::protocol::MAX_PAYLOAD;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc, oneshot, watch};

/// How long a shim waits for the laptop before the pump gives up on its behalf.
///
/// Slightly longer than the shim's own `client::REQUEST_TIMEOUT`, so the shim
/// is the one that reports the timeout and the developer sees a message naming
/// the clipboard rather than one naming the pump. This exists only to stop a
/// pending entry leaking for a reply that is never coming.
const REPLY_TIMEOUT: Duration = Duration::from_secs(25);

/// A request line plus the largest body the protocol carries, and nothing more.
///
/// A shim is riabuild's own code, but it is reached through a socket every
/// co-tenant on a shared account could connect to, so its input is bounded here
/// rather than trusted.
const MAX_REQUEST: u64 = MAX_PAYLOAD as u64 + 4096;

/// How long a shim has to finish writing its request and half-close.
///
/// [`MAX_REQUEST`] bounds how much one connection can buffer and said nothing
/// about how long it could take: a process that connected and then never wrote
/// held a task, a file descriptor and a `Vec` for the length of the session,
/// and N of them held N × 32 MB. The socket is local and every shim writes its
/// whole request in one go, so this is slack rather than a deadline anyone
/// approaches — a 32 MB clipboard write over `AF_UNIX` is milliseconds.
///
/// Shorter than [`REPLY_TIMEOUT`], so a connection cannot be shed twice over
/// and the pump's own bookkeeping stays the shorter-lived half.
pub(super) const REQUEST_DEADLINE: Duration = Duration::from_secs(20);

/// One shim connection, start to finish.
pub(super) async fn relay(
    mut stream: UnixStream,
    id: u64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Vec<u8>>>>>,
    outbound: mpsc::Sender<Frame>,
    mut watching: watch::Receiver<bool>,
) -> Result<()> {
    // The shim half-closes after writing, so end of input is the end of the
    // request — header line, body and all. Reading to EOF rather than parsing a
    // length is what keeps the pump from having to know the protocol at all.
    //
    // Bounded in both directions. `take` caps how much one connection can
    // buffer; the deadline caps how long it may take about it, which nothing
    // did — a process that connected and never wrote held this task, its
    // descriptor and its buffer for the length of the developer's session.
    let mut payload = Vec::new();
    let mut bounded = (&mut stream).take(MAX_REQUEST);
    tokio::time::timeout(REQUEST_DEADLINE, bounded.read_to_end(&mut payload))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "a shim connected and did not finish its request within {} seconds",
                REQUEST_DEADLINE.as_secs()
            )
        })?
        .context("could not read the request from the shim")?;

    // A connection that closed without writing is not a request, and must not
    // become an empty frame the laptop is asked to answer. Something does this
    // on purpose: `bind` below connects to decide whether a socket is live, and
    // a channel that reported that probe to the laptop as a paste would be
    // answering a question nobody asked.
    if payload.is_empty() {
        return Ok(());
    }

    // Checked before anything is registered: this connection may have arrived
    // in the gap between the laptop closing its pipe and the listener being
    // dropped, and a shim that waited out the reply timeout there would be
    // waiting on a peer that is already gone.
    if *watching.borrow_and_update() {
        return Ok(());
    }

    let (reply, answered) = oneshot::channel();
    pending.lock().await.insert(id, reply);

    if outbound.send(Frame { id, payload }).await.is_err() {
        pending.lock().await.remove(&id);
        return Ok(());
    }

    // Closing without a reply is the right answer to every arm below: `client`
    // reads it as a channel that is not there, which is exactly what it is.
    let response = tokio::select! {
        biased;
        answer = answered => match answer {
            Ok(bytes) => bytes,
            Err(_) => return Ok(()),
        },
        _ = watching.changed() => {
            pending.lock().await.remove(&id);
            return Ok(());
        }
        _ = tokio::time::sleep(REPLY_TIMEOUT) => {
            pending.lock().await.remove(&id);
            return Ok(());
        }
    };

    stream.write_all(&response).await?;
    stream.flush().await?;
    Ok(())
}

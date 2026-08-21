//! How the pump learns its laptop has gone.
//!
//! `ServerAliveInterval` only measures the end that started the connection, and
//! `sshd` ships with `ClientAliveInterval 0` — so the server end has nothing
//! measuring it at all, and a TCP connection whose peer stopped acknowledging
//! looks exactly like an idle one for as long as the kernel retransmits. This
//! is the same measurement taken from this side, and its whole product is a
//! *return*: the pump ends, and the socket goes back.

use crate::mux::{Frame, KEEPALIVE_ID};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
// `tokio`'s instant, not `std`'s, and the difference is not cosmetic: this is
// the clock the keepalive's own sleeps are measured against, and the two only
// agree outside a test. Under a paused clock `std::time::Instant` goes on
// reporting wall-clock, so the deadline below would be measured against a
// clock nothing else in the loop is using — which is a test that cannot fail
// rather than a test that passes.
use tokio::time::Instant;

/// How often the pump asks the laptop whether it is still on the other end.
///
/// The same interval as the `ServerAliveInterval` the laptop's own `ssh` is
/// started with, because this is the same measurement taken from the other
/// side. Cheap in a way the health probe this design deleted was not: one empty
/// frame down a pipe that is already open, never a second SSH connection.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// How long the pump goes unanswered before it gives the socket back.
///
/// Three missed keepalives, matching `ServerAliveCountMax=3` on the laptop, so
/// the two ends give up on each other at about the same moment.
///
/// **This is the bound on how long a pump can outlive its laptop, and until it
/// existed there was none.** The laptop notices a dropped connection because
/// `ssh` is measuring it; the server does not, because `sshd` ships with
/// `ClientAliveInterval 0` and a TCP connection whose peer stopped
/// acknowledging is indistinguishable from an idle one for as long as the
/// kernel keeps retransmitting — a quarter of an hour, and longer if the peer
/// is merely wedged rather than gone. For all of it the pump stayed bound to
/// the socket, which cost the developer three things at once: every paste and
/// every `riabuild channel status` blocked for the full reply timeout and then
/// failed, every pump the reconnecting supervisor started found the socket
/// live and refused it, and the supervisor — recognising none of that — said
/// the channel could not reach the server, which was the one thing that was
/// not true.
const KEEPALIVE_DEADLINE: Duration = Duration::from_secs(45);

/// Returns when the laptop has gone [`KEEPALIVE_DEADLINE`] without a word.
///
/// The point of it is the *return*. Ending the pump unbinds the socket, which
/// is what lets the reconnecting supervisor's own pump take the path, and what
/// turns a paste into an immediate "the channel is not running" instead of a
/// twenty-second wait for a laptop that is not there.
///
/// Sent rather than merely waited for, because silence on this pipe is not
/// evidence of anything on its own: a developer who is not pasting produces
/// exactly as little traffic as a laptop that has gone. The frame obliges an
/// answer — see [`KEEPALIVE_ID`] for why an empty one is enough — and it is the
/// answer that is the measurement.
pub(super) async fn keepalive(
    since: Instant,
    heard: Arc<AtomicU64>,
    outbound: mpsc::Sender<Frame>,
) {
    loop {
        tokio::time::sleep(KEEPALIVE_INTERVAL).await;

        let last = Duration::from_millis(heard.load(Ordering::Relaxed));
        if since.elapsed().saturating_sub(last) >= KEEPALIVE_DEADLINE {
            return;
        }

        // `try_send`, never `send`. The queue drains into a pipe that is
        // precisely what may have stopped moving, and a keepalive that blocked
        // on the wedged connection it exists to detect would be the one thing
        // this function must not do. A full queue is not treated as a failure
        // on its own — the deadline above is the only thing that decides — but
        // a closed one means the writer has already ended, and there is nothing
        // left to keep alive.
        if outbound
            .try_send(Frame {
                id: KEEPALIVE_ID,
                payload: Vec::new(),
            })
            .is_err()
            && outbound.is_closed()
        {
            return;
        }
    }
}

//! Taking the channel socket, and telling a leftover apart from a colleague.
//!
//! The whole of what this module decides is one question that cannot be
//! answered by looking at the file: a socket nothing answers on is a leftover
//! from a killed session and is this account's to remove, and one that *does*
//! answer belongs to a pump that is still serving. Connecting is the only way
//! to tell them apart.

use anyhow::{Context, Result};
use riabuild_ui::Failure;
use std::path::Path;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

/// How long [`answers`] gives a socket to say whether anything is serving on it.
///
/// Generous by design. Completing a connection to an `AF_UNIX` socket is the
/// kernel's work and does not wait for the pump to `accept`, so a healthy
/// listener answers in microseconds and any value here is slack. It is a
/// *bound*, not a deadline anyone is expected to approach.
const LIVENESS_PROBE: Duration = Duration::from_secs(2);

/// Binds the channel socket, clearing a dead one and refusing a live one.
///
/// The distinction is the whole of it. A socket file that nothing answers on is
/// a leftover from a killed session and is this account's to remove; a socket
/// that *does* answer belongs to a pump that is still serving, and taking it
/// would silently cut that session's paste. Connecting is the only way to tell
/// them apart — the file looks identical either way.
///
/// **Refusing is the ordinary outcome, not the exceptional one.** One developer
/// with three terminals into one server is the case remote mode is *for*, and
/// all three of them run this. Exactly one binds; the other two are told the
/// channel is already up and stand by for it. The message says so in those
/// words — it used to tell the developer to close their own other window, which
/// is advice to break a working session in order to fix one that was never
/// broken.
pub(super) async fn bind(socket: &Path) -> Result<UnixListener> {
    if socket.exists() {
        if answers(socket).await {
            return Err(Failure::new(
                format!(
                    "another riabuild session is {} at {}",
                    super::ALREADY_SERVED,
                    socket.display()
                ),
                "Nothing to do — paste in this shell already goes through it. This session \
                 stands by, and takes the channel over if the one serving it ends.",
            )
            .into());
        }
        // Nothing answered: a socket left by a session that was killed. Under
        // `ssh -R` this was fatal and unfixable, because sshd owned the bind.
        tokio::fs::remove_file(socket)
            .await
            .with_context(|| format!("could not clear the stale socket at {}", socket.display()))?;
    }

    UnixListener::bind(socket).with_context(|| format!("could not listen on {}", socket.display()))
}

/// Whether anything is serving on this socket — and never a question that hangs.
///
/// The connect is bounded because the answer is only ever used to choose
/// between clearing a leftover and refusing to take a live one, and neither
/// choice is worth blocking a session on. An unbounded probe is not a probe: it
/// puts the pump's startup at the mercy of the kernel's willingness to refuse a
/// connection, which is exactly where it was found — a stale `channel.sock` on
/// macOS left `riabuild channel pump` waiting on a connect that never completed
/// and never failed, so the channel came up neither working nor broken. The
/// same stall inside `pump::tests` held a release's macOS job open for as long
/// as GitHub allows a job to run.
///
/// A timeout counts as **not** serving, and that is the deliberate half. It is
/// the direction that keeps the leftover socket this whole design exists to
/// recover from recoverable: a pump that cannot answer a connect inside
/// [`LIVENESS_PROBE`] is not one a shim could have used either, so treating it
/// as live would trade a rare stolen channel for a permanently dead one. The
/// case the refusal protects — a colleague's *working* pump — answers in
/// microseconds and is never the one that times out.
pub(super) async fn answers(socket: &Path) -> bool {
    matches!(
        tokio::time::timeout(LIVENESS_PROBE, UnixStream::connect(socket)).await,
        Ok(Ok(_))
    )
}

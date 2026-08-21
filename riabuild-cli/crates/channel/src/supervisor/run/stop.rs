//! Asking the channel to end, and waiting until somebody has.
//!
//! One `watch` channel, reached two ways. [`Stop`] is the caller's handle:
//! cloneable, inert, and safe to use before the supervisor has started or after
//! it has already returned. [`stopped`] is the wait the loop next door and the
//! caller between connections both need, and it is a loop around `changed()`
//! rather than `changed()` itself because a stop that landed first would
//! otherwise never wake anyone.

use std::sync::Arc;
use tokio::sync::watch;

/// The caller's end of a running supervisor.
///
/// Cloneable and inert: holding one keeps nothing alive, so a caller that drops
/// it without stopping the supervisor gets a channel that shuts itself down
/// rather than one that outlives the shell it belongs to.
#[derive(Clone)]
pub struct Stop(Arc<watch::Sender<bool>>);

impl Default for Stop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop {
    pub fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    /// Asks the supervisor to close the connection and return.
    ///
    /// Idempotent, and safe both before the supervisor has started and after it
    /// has already returned. `send_replace` rather than `send` for the first of
    /// those: `send` fails when nobody is subscribed *and leaves the value
    /// unchanged*, so a stop that arrived first would be silently forgotten and
    /// the supervisor would come up already-stale.
    pub fn stop(&self) {
        self.0.send_replace(true);
    }

    /// Whether a stop has already been asked for.
    ///
    /// For a caller that has work of its own to skip — `remote::channel`'s
    /// holder loop asks before it goes back to waiting for the channel's lease,
    /// so a session whose shell has just exited does not take a lease it is
    /// about to give back.
    pub fn asked(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once a stop has been asked for — immediately if it already has.
    ///
    /// The same guarantee [`stopped`] gives this file's own loop, offered to the
    /// caller that waits *between* supervised connections. Without it a session
    /// standing by would have to poll `asked` and could sleep through its own
    /// shell exiting, which is a task left running against a terminal riabuild
    /// has finished with.
    pub async fn stopped(&self) {
        let mut signal = self.signal();
        stopped(&mut signal).await;
    }

    pub(super) fn signal(&self) -> watch::Receiver<bool> {
        self.0.subscribe()
    }
}

/// Resolves once a stop has been asked for — immediately if it already has.
///
/// `changed()` on its own reports only transitions this receiver has not seen,
/// so a stop that landed before the supervisor reached this point would never
/// wake it: the shell would exit and the connection would stay up behind it.
pub(super) async fn stopped(signal: &mut watch::Receiver<bool>) {
    loop {
        let asked = *signal.borrow_and_update();
        if asked {
            return;
        }
        if signal.changed().await.is_err() {
            // Every `Stop` handle is gone, so nothing can ever ask again. An
            // ssh nobody holds a stop for is the leak `kill_on_drop` exists to
            // prevent, and shutting down is the honest reading of "the caller
            // is finished with us".
            return;
        }
    }
}

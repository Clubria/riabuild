//! A child the fake started: something a test can wait on, kill, and — for the
//! clipboard channel — write frames at.

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use crate::child::{ChildHandle, ChildReader, ChildWriter, PipedChildHandle};
use crate::output::CommandOutput;

/// How a scripted child ends.
pub(super) enum Ending {
    /// It exits on its own, with this status and stderr.
    Alone(CommandOutput),
    /// It stays up until something kills it — the tunnel that is *working*.
    /// Without this a ping-timeout test could never reach the teardown it is
    /// about: the child would exit first and the supervisor would be rebuilding
    /// after a clean exit rather than tearing down a wedged forward.
    OnlyWhenKilled,
}

/// One child the fake started.
pub(super) struct FakeChild {
    pub(super) invocation: String,
    /// `None` for a child scripted to stay up: `wait` then resolves only once
    /// `kill` has been called.
    pub(super) exit: Option<CommandOutput>,
    /// The halves riabuild talks over, present only for a `spawn_piped` child.
    /// The other ends live in `FakeRunner::pipes`, so a test drives this child
    /// the way a real `riabuild channel pump` would — by writing frames at it
    /// and reading the frames that come back.
    pub(super) stdin: std::sync::Mutex<Option<ChildWriter>>,
    pub(super) stdout: std::sync::Mutex<Option<ChildReader>>,
    /// The far ends, held here rather than on the runner so that killing this
    /// child *closes* them — which is what a real `ssh` dying does, and what
    /// anything reading the pipe is waiting for. Parked on the runner instead,
    /// they outlived every child, no read ever saw an end of pipe, and a
    /// supervisor that waits for its reader to finish waited for ever.
    pub(super) far: std::sync::Mutex<Option<FakePipes>>,
    pub(super) killed: std::sync::Mutex<bool>,
    /// Wakes a pending `wait`. `notify_one` rather than `notify_waiters`
    /// because it stores a permit when nobody is waiting yet — a kill that
    /// lands between `wait` reading the flag and registering itself would
    /// otherwise leave the waiter parked on a child that is already dead.
    pub(super) stopped: tokio::sync::Notify,
}

#[async_trait]
impl ChildHandle for Arc<FakeChild> {
    async fn wait(&self) -> Result<CommandOutput> {
        if let Some(output) = &self.exit {
            return Ok(output.clone());
        }
        while !*self.killed.lock().unwrap() {
            self.stopped.notified().await;
        }
        // No code, the way a real process killed by a signal reports itself.
        Ok(CommandOutput {
            code: None,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    async fn kill(&self) -> Result<()> {
        *self.killed.lock().unwrap() = true;
        // Dropping the far ends is the fake's stand-in for the kernel closing a
        // dead process's pipes: without it a reader waits for an end of pipe
        // that never comes.
        *self.far.lock().unwrap() = None;
        self.stopped.notify_one();
        Ok(())
    }
}

impl PipedChildHandle for Arc<FakeChild> {
    fn take_stdin(&self) -> Option<ChildWriter> {
        self.stdin.lock().ok()?.take()
    }

    fn take_stdout(&self) -> Option<ChildReader> {
        self.stdout.lock().ok()?.take()
    }
}

/// The far end of a scripted child's stdio — the test's side of the pipe.
///
/// `to_riabuild` is what the pump would write: frames riabuild reads as the
/// child's stdout. `from_riabuild` is what riabuild writes to the child's
/// stdin. Named for direction rather than for stream, because "the child's
/// stdout" and "the end a test writes to" are opposite ends of one pipe and
/// naming them after the stream is how they get connected backwards.
pub struct FakePipes {
    pub to_riabuild: tokio::io::DuplexStream,
    pub from_riabuild: tokio::io::DuplexStream,
}

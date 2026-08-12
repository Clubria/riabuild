//! A subprocess riabuild holds rather than waits for.
//!
//! Every other method on `CommandRunner` owns a child's whole lifetime: start
//! it, wait, return what it said. The clipboard channel's `ssh -N -R` is the
//! one process that has to be *held* — the supervisor pings through the forward
//! while the forward is still up, and a tunnel run through `run` would only
//! return once it had already died.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::CommandOutput;

/// A child that outlives the call that started it.
///
/// Both methods take `&self`, not `self` or `&mut self`, and that is the whole
/// point of the type. The supervisor `select!`s on the child exiting *while*
/// keeping the ability to kill it when a ping goes unanswered; with `&mut self`
/// the borrow checker refuses the second half, and the only way left to stop a
/// wedged tunnel is to drop the handle — which is a teardown, not the rebuild
/// the supervisor is trying to perform.
#[async_trait]
pub trait ChildHandle: Send + Sync {
    /// Resolves when the child exits, with its status and whatever it wrote to
    /// stderr.
    async fn wait(&self) -> Result<CommandOutput>;

    /// Asks the child to stop. Idempotent: killing an already-exited child is
    /// `Ok`.
    async fn kill(&self) -> Result<()>;
}

/// The real thing: a `tokio::process::Child` reachable from both entry points.
pub(super) struct RealChild {
    program: String,
    /// `wait` releases this whenever the future it returned is dropped — which
    /// is exactly what a losing `select!` branch does — so a `kill` racing a
    /// `wait` finds the lock free. Held across the await it must be, since a
    /// `std::sync::Mutex` guard is not `Send` and the future would not compile.
    child: Mutex<Child>,
    /// Drained by its own task rather than inside `wait`, for two reasons a
    /// wedged tunnel would hit in order: a child whose stderr nobody reads
    /// blocks once the 64 KB pipe fills, and `wait` is dropped and recreated on
    /// every ping tick, which would throw away everything read so far each
    /// time. `supervisor::diagnose` needs the whole of it to tell a refused
    /// forward from an ordinary disconnect.
    stderr: Mutex<Option<tokio::task::JoinHandle<String>>>,
}

impl RealChild {
    pub(super) fn spawn(mut command: Command, program: &str) -> Result<Self> {
        // stdout is null because `ssh -N` produces none; stderr is piped
        // because it is the only place a server that refuses the forward says
        // so, and that text is all `supervisor::diagnose` has to turn into
        // advice a developer can act on.
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::piped());
        // Without this, a handle dropped on any error path — a `?` above the
        // supervisor, a cancelled task, a panic — leaves an ssh alive holding
        // the remote socket. The next attempt cannot bind it, so the channel
        // comes up permanently dead and nothing on the laptop names the
        // process still holding it.
        command.kill_on_drop(true);

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))?;

        let stderr = child.stderr.take().map(|mut pipe| {
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                // A read that fails part-way still yields what arrived before
                // it: stderr here is diagnostics, and losing the lot because
                // the tail was unreadable is worse than reporting the head.
                let _ = pipe.read_to_end(&mut bytes).await;
                String::from_utf8_lossy(&bytes).into_owned()
            })
        });

        Ok(Self {
            program: program.to_string(),
            child: Mutex::new(child),
            stderr: Mutex::new(stderr),
        })
    }
}

#[async_trait]
impl ChildHandle for RealChild {
    async fn wait(&self) -> Result<CommandOutput> {
        let status = {
            let mut child = self.child.lock().await;
            child
                .wait()
                .await
                .with_context(|| format!("`{}` did not finish", self.program))?
        };

        let drained = self.stderr.lock().await.take();
        let stderr = match drained {
            Some(handle) => handle.await.unwrap_or_default(),
            None => String::new(),
        };

        Ok(CommandOutput {
            code: status.code(),
            stdout: String::new(),
            stderr,
        })
    }

    async fn kill(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        // The supervisor kills on a ping timeout without knowing whether ssh
        // has meanwhile exited on its own, and signalling a process that has
        // already been reaped is an error rather than a no-op. Checking first
        // is what makes the timeout path safe to take at any moment.
        if child
            .try_wait()
            .with_context(|| format!("could not check on `{}`", self.program))?
            .is_some()
        {
            return Ok(());
        }

        // `start_kill` rather than `kill`: the latter waits for the exit it
        // just caused, and would hold this lock while it did — blocking the
        // `wait` that is going to reap the child and turning a teardown into a
        // hang. The reap belongs to whoever waits next, or to `kill_on_drop`.
        match child.start_kill() {
            Ok(()) => Ok(()),
            // Lost the race with an exit between the check above and here.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error).with_context(|| format!("could not stop `{}`", self.program)),
        }
    }
}

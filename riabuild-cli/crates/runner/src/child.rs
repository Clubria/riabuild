//! A subprocess riabuild holds rather than waits for.
//!
//! Every other method on `CommandRunner` owns a child's whole lifetime: start
//! it, wait, return what it said. The clipboard channel's `ssh` is the one
//! process that has to be *held* — it runs for the length of a session and the
//! supervisor talks over its stdio the whole time, so a connection run through
//! `run` would only return once it had already died.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::CommandOutput;

/// The reading half of a child riabuild speaks to over its stdio.
pub type ChildReader = Box<dyn AsyncRead + Send + Unpin>;
/// The writing half of the same.
pub type ChildWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// A child that outlives the call that started it.
///
/// Both methods take `&self`, not `self` or `&mut self`, and that is the whole
/// point of the type. The supervisor `select!`s on the child exiting *while*
/// keeping the ability to kill it when the caller asks it to stop; with
/// `&mut self` the borrow checker refuses the second half, and the only way
/// left to stop a live connection is to drop the handle — which is a teardown,
/// not the rebuild the supervisor is trying to perform.
#[async_trait]
pub trait ChildHandle: Send + Sync {
    /// Resolves when the child exits, with its status and whatever it wrote to
    /// stderr.
    async fn wait(&self) -> Result<CommandOutput>;

    /// Asks the child to stop. Idempotent: killing an already-exited child is
    /// `Ok`.
    async fn kill(&self) -> Result<()>;
}

/// A held child whose stdin and stdout riabuild talks over.
///
/// Separate from [`ChildHandle`] rather than folded into it because the two
/// answer different questions, and the default really is "no stdio": every
/// other held child riabuild starts is `ssh -N`, which has none, and a trait
/// that offered pipes for it would offer `None` forever. The clipboard channel
/// is the only caller, and it needs all four methods at once — hence the
/// supertrait rather than a parallel type.
///
/// Both takes are once-only and return `None` on a second call. The halves are
/// owned, not borrowed, because the supervisor gives them to two independent
/// tasks: one pumping frames out, one reading frames in. A borrowing API would
/// tie both to a stack frame that has to stay alive for the whole session.
pub trait PipedChildHandle: ChildHandle {
    fn take_stdin(&self) -> Option<ChildWriter>;
    fn take_stdout(&self) -> Option<ChildReader>;
}

/// The real thing: a `tokio::process::Child` reachable from both entry points.
pub(super) struct RealChild {
    program: String,
    /// `wait` releases this whenever the future it returned is dropped — which
    /// is exactly what a losing `select!` branch does — so a `kill` racing a
    /// `wait` finds the lock free. Held across the await it must be, since a
    /// `std::sync::Mutex` guard is not `Send` and the future would not compile.
    child: Mutex<Child>,
    /// Drained by its own task rather than inside `wait`: a child whose stderr
    /// nobody reads blocks once the 64 KB pipe fills, and a `wait` future that
    /// is dropped and recreated would throw away everything read so far each
    /// time. `supervisor::diagnose` needs the whole of it to tell a server that
    /// cannot run the pump from an ordinary disconnect.
    stderr: Mutex<Option<tokio::task::JoinHandle<String>>>,
    /// The channel's pipes, empty unless this child came from
    /// [`RealChild::spawn_piped`]. A `std::sync::Mutex` rather than a tokio one
    /// because taking a half neither blocks nor awaits — it is a move out of an
    /// `Option` — and an async take would force `#[async_trait]` onto
    /// `PipedChildHandle` for no gain.
    stdin: std::sync::Mutex<Option<ChildWriter>>,
    stdout: std::sync::Mutex<Option<ChildReader>>,
}

impl RealChild {
    pub(super) fn spawn(command: Command, program: &str) -> Result<Self> {
        Self::start(command, program, false)
    }

    /// The same child, with stdin and stdout kept rather than nulled.
    ///
    /// The clipboard channel's transport: `ssh -T <host> riabuild channel pump`
    /// carries every request and every reply over this one pipe, so nulling
    /// either half — right for every other held child riabuild starts — would
    /// close the channel at the moment it opened.
    pub(super) fn spawn_piped(command: Command, program: &str) -> Result<Self> {
        Self::start(command, program, true)
    }

    fn start(mut command: Command, program: &str, piped: bool) -> Result<Self> {
        // Nulled for `ssh -N`, which produces no stdout and reads no stdin.
        // stderr is piped either way: it is the only place a server that
        // refuses the connection says so, and that text is all
        // `supervisor::diagnose` has to turn into advice a developer can act on.
        let stdio = || if piped { Stdio::piped() } else { Stdio::null() };
        command.stdin(stdio());
        command.stdout(stdio());
        command.stderr(Stdio::piped());
        // Without this, a handle dropped on any error path — a `?` above the
        // supervisor, a cancelled task, a panic — leaves an ssh alive, and with
        // it a pump holding the server's socket. The next attempt finds that
        // socket live and refuses it, so the channel comes up dead and nothing
        // on the laptop names the process still holding it.
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

        // Taken before the handle is built, so a caller that never asks for
        // them still gets the pipes closed when the child is dropped rather
        // than leaving `ssh` holding a stdin nobody will ever write to.
        let stdin = child.stdin.take().map(|pipe| Box::new(pipe) as ChildWriter);
        let stdout = child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as ChildReader);

        Ok(Self {
            program: program.to_string(),
            child: Mutex::new(child),
            stderr: Mutex::new(stderr),
            stdin: std::sync::Mutex::new(stdin),
            stdout: std::sync::Mutex::new(stdout),
        })
    }
}

impl PipedChildHandle for RealChild {
    fn take_stdin(&self) -> Option<ChildWriter> {
        self.stdin.lock().ok()?.take()
    }

    fn take_stdout(&self) -> Option<ChildReader> {
        self.stdout.lock().ok()?.take()
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

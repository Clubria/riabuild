//! Every external process riabuild starts goes through here.
//!
//! This is the single decision that makes the rest of the crate testable: with
//! it, each `check()` is a pure unit test against canned `gh`, `git`, `node` and
//! `claude` output. Without it, every test needs a real machine in a real state,
//! and the suite gets abandoned.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct. The `feature = "testing"` half matters as much as the `test` half:
// when a downstream crate turns the feature on, this crate is compiled as a
// dependency and `cfg(test)` is false, so the exemption would not apply.
#![cfg_attr(
    any(test, feature = "testing"),
    allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)
)]

use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;

mod child;
mod delegate;
#[cfg(any(test, feature = "testing"))]
mod fake;
mod options;
mod output;
#[cfg(unix)]
mod pty;
mod real;
mod subdue;

pub use child::{ChildHandle, ChildReader, ChildWriter, PipedChildHandle};
pub use delegate::{Decoration, Delegating, EnvScope, ScopedRunner};
#[cfg(any(test, feature = "testing"))]
pub use fake::{FakePipes, FakeRunner, Recorded};
pub use options::{DEFAULT_TIMEOUT, RunOptions, directory_for_riabuild, should_subdue};
pub use output::{BytesOutput, CommandOutput};
pub use real::RealRunner;

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput>;

    /// Like `run`, but stdout is returned as raw bytes.
    ///
    /// Used by the clipboard channel, where stdout is a PNG rather than a
    /// version string, and `from_utf8_lossy` would replace every byte that is
    /// not valid UTF-8 with U+FFFD.
    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput>;

    /// Feeds stdin to a command that forks a child to outlive it, and waits
    /// only for the process actually started.
    ///
    /// `xclip -i` and `wl-copy` fork into the background to *serve* the
    /// selection they were given, and that fork inherits whatever stdout it was
    /// handed. `run` and `run_bytes` finish by reading stdout to EOF, which for
    /// these two arrives only when the selection is replaced — so a clipboard
    /// write through either would hang for as long as the copy stayed current.
    ///
    /// The cost is that stderr is unavailable: the fork holds that pipe too, so
    /// the only diagnostic a write can carry is its exit status.
    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32>;

    /// Starts a child and hands back a handle to it instead of waiting for it.
    ///
    /// Every other method here finishes by waiting for exit, which is the one
    /// thing a long-lived child must not do: the clipboard channel's `ssh` runs
    /// for the length of a session, and one run through `run` would only return
    /// once it had already failed.
    ///
    /// `options.stdin` is ignored, and both of the child's own halves are
    /// nulled. Use `spawn_piped` for a child riabuild talks to; a pipe left
    /// open for nobody is one more handle keeping a dead connection from being
    /// noticed.
    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>>;

    /// The same, keeping the child's stdin and stdout so riabuild can talk to
    /// it.
    ///
    /// One caller: the clipboard channel, whose transport *is* a child's stdio.
    /// `ssh -T <host> riabuild channel pump` carries every request and reply
    /// over this pipe, which is what lets the channel work on a server that
    /// grants nothing beyond running a command — no port forwarding, no unix
    /// socket forwarding, no `sshd_config` anyone has to edit.
    ///
    /// Separate from `spawn` rather than a flag on it because the two differ in
    /// their return type, and because the nulled stdio above is the right
    /// default: a pipe opened for a child nobody reads is a handle keeping a
    /// dead tunnel from being noticed.
    /// Defaulted so the test doubles scattered across the workspace — which
    /// stub `CommandRunner` for a task and will never open a channel — do not
    /// each need an identical copy of a method they do not use. The default
    /// refuses rather than returning an empty pipe: a channel handed a stdio
    /// that silently carries nothing is the failure this whole design exists
    /// to remove, and it must not be reintroduced as a stub's convenience.
    async fn spawn_piped(
        &self,
        program: &str,
        _args: &[&str],
        _options: &RunOptions,
    ) -> Result<Box<dyn PipedChildHandle>> {
        anyhow::bail!("this CommandRunner cannot pipe `{program}`'s stdio")
    }

    /// Replaces this process's stdio with the child's — used for the
    /// environment shell and for anything that prompts the developer.
    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32>;

    /// Resolves a program on `PATH`, so `check()` can distinguish "not
    /// installed" from "installed but wrong version".
    ///
    /// Stays synchronous: it reads `PATH` and stats the candidates, which is
    /// cheap enough that making it async would infect every `check()` for no
    /// gain.
    fn which(&self, program: &str) -> Option<PathBuf>;
}

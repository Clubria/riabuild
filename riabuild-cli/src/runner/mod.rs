//! Every external process riabuild starts goes through here.
//!
//! This is the single decision that makes the rest of the crate testable: with
//! it, each `check()` is a pure unit test against canned `gh`, `git`, `node` and
//! `claude` output. Without it, every test needs a real machine in a real state,
//! and the suite gets abandoned.

use anyhow::{Context, Result};
use async_trait::async_trait;
#[cfg(test)]
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

use crate::theme::Theme;

mod child;
#[cfg(unix)]
mod pty;
mod subdue;

pub use child::ChildHandle;

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout with trailing newline removed — what a `--version` check wants.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim()
    }
}

/// Subprocess output whose stdout is not assumed to be text.
///
/// `CommandOutput` exists for the `--version` and status checks that make up
/// most of riabuild, and its lossy `String` conversion is right for those. The
/// clipboard channel moves PNGs, where a single replacement character is a
/// corrupt image, so it reads through here instead. stderr stays a `String`:
/// it is diagnostics, it is always text, and every caller puts it in a message.
#[derive(Debug, Clone)]
pub struct BytesOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl BytesOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Fed to the child's stdin. Used to pipe brokered secrets without them ever
    /// appearing in a process argument list, where `ps` would show them — and on
    /// a shared server `ps` shows other developers' processes — and to hand a
    /// clipboard write to `xclip -i`.
    ///
    /// Bytes rather than a `String`, for the same reason `run_bytes` exists:
    /// nothing piped through here (a tarball, a token, a PNG) is guaranteed to
    /// be UTF-8, and a `String` cannot represent a PNG at all — so an image
    /// write would not be merely lossy, it would be unconstructible.
    pub stdin: Option<Vec<u8>>,
    /// Run this child under a pty riabuild owns, discard everything it draws
    /// *with*, and print what is left one dimmed line at a time.
    ///
    /// Honoured by `run_interactive` only. The capturing methods never reach a
    /// terminal, so there is nothing there to subdue.
    ///
    /// A `Theme` rather than a `bool` for the reason `CLAUDE.md` gives for the
    /// text a generated rcfile prints: the palette is resolved on the side that
    /// has a `Ui` and passed to the side that does not. `runner/` has no `Ui`
    /// and must not grow one. `Theme::plain()` is a legitimate value — line
    /// discipline with no dim — which is what a `NO_COLOR` run produces without
    /// a special case anywhere.
    pub subdued: Option<Theme>,
}

/// Whether this call actually gets a pty.
///
/// Split out from `run_interactive` so the rule is testable without a terminal,
/// the same reason `theme::depth_for` is split out from `Theme::detect`.
///
/// With no terminal the flag is ignored outright. That is not a convenience: an
/// unattended run must not take a different code path from an attended one for
/// a *cosmetic* reason, and a pty allocated where no terminal exists would be
/// riabuild inventing a tty for a child that correctly concluded there wasn't
/// one.
pub fn should_subdue(is_terminal: bool, subdued: Option<Theme>) -> Option<Theme> {
    is_terminal.then_some(subdued).flatten()
}

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
    /// thing the clipboard channel's `ssh -N -R` must not do: the supervisor
    /// has to ping *through* the forward while the forward is up, and a tunnel
    /// run through `run` would only return once it had already failed.
    ///
    /// `options.stdin` is ignored. A held child is not something anything
    /// writes to — `ssh -N` reads none — and a pipe left open for nobody is one
    /// more handle keeping a dead tunnel from being noticed.
    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>>;

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

pub struct RealRunner;

impl RealRunner {
    fn build(program: &str, args: &[&str], options: &RunOptions) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        if let Some(cwd) = &options.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &options.env {
            command.env(key, value);
        }
        command
    }
}

#[async_trait]
impl CommandRunner for RealRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
        let mut command = RealRunner::build(program, args, options);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        command.stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))?;

        if let Some(input) = &options.stdin {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().context("stdin was piped")?;
            stdin.write_all(input).await?;
            // Closing the pipe is what tells the child there is no more input.
            // The blocking version got this free when the `if let` block ended;
            // here the handle would otherwise live until the end of the
            // function, and a child reading to EOF — `infisical export` does —
            // would wait forever.
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .with_context(|| format!("`{program}` did not finish"))?;

        Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput> {
        let mut command = RealRunner::build(program, args, options);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        command.stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))?;

        if let Some(input) = &options.stdin {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().context("stdin was piped")?;
            stdin.write_all(input).await?;
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .with_context(|| format!("`{program}` did not finish"))?;

        Ok(BytesOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32> {
        let mut command = RealRunner::build(program, args, options);
        // Null rather than piped: a pipe handed to the fork is exactly what
        // would keep this call waiting for a selection nobody is going to
        // replace.
        command.stdout(Stdio::null()).stderr(Stdio::null());
        command.stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))?;

        if let Some(input) = &options.stdin {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().context("stdin was piped")?;
            stdin.write_all(input).await?;
            // The fork does not take ownership of the selection until it has
            // read the content to EOF, so closing this is what completes the
            // copy rather than merely tidying up.
            drop(stdin);
        }

        let status = child
            .wait()
            .await
            .with_context(|| format!("`{program}` did not finish"))?;
        Ok(status.code().unwrap_or(1))
    }

    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>> {
        let command = RealRunner::build(program, args, options);
        Ok(Box::new(child::RealChild::spawn(command, program)?))
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        let mut command = RealRunner::build(program, args, options);

        // The handoff `CLAUDE.md` describes is still the default, and still the
        // rule for every site that leaves `subdued` unset. Where riabuild does
        // perform the IO it does so through `AsyncFd` on the current-thread
        // runtime — see `pty.rs`.
        #[cfg(unix)]
        if let Some(theme) = should_subdue(pty::available(), options.subdued) {
            return pty::run(command, theme, program).await;
        }

        let status = command
            .status()
            .await
            .with_context(|| format!("could not start `{program}`"))?;
        Ok(status.code().unwrap_or(1))
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
/// One scripted response, and the conditions it answers to.
struct Stub {
    invocation: String,
    /// Environment entries that must all be present for this stub to apply.
    /// Empty means "any environment", which is what `with` produces.
    env: Vec<(String, String)>,
    output: CommandOutput,
    /// Raw stdout, for a command whose output is not text. `None` falls back to
    /// `output.stdout`, so a test that only cares about the exit code can stub
    /// with `with` and still be read through `run_bytes`.
    bytes: Option<Vec<u8>>,
    /// Matched anywhere in the invocation rather than only at its front — what
    /// `containing` produces. For `ssh <options…> host <the real command>` the
    /// part that distinguishes one remote invocation from another is the tail,
    /// not the program name every one of them shares.
    fragment: bool,
}

#[cfg(test)]
/// Scripted `CommandRunner` for tests.
///
/// Each `Stub` is matched by `"program arg1 arg2"` prefix; the longest matching
/// prefix wins, so a test can stub `gh auth status` and `gh --version`
/// independently. `containing` relaxes that to a fragment anywhere in the
/// invocation, for the remote commands whose distinguishing part is the tail.
///
/// A stub can also require environment entries. `claude auth status --json` is
/// the same command string for every Claude Code account — only
/// `CLAUDE_CONFIG_DIR` differs — so without this the central behaviour of the
/// account feature could not be written as a test at all.
#[derive(Default)]
pub struct FakeRunner {
    responses: Vec<Stub>,
    /// Queued stubs: each call matching a key pops the next response queued
    /// for it by `then()`, in order, before falling through to `responses`
    /// once that key's queue is empty. This is the "first call fails, second
    /// succeeds" shape a single `with()` stub cannot express, since every call
    /// to it returns the same fixed response — needed for a probe that's run
    /// before and after some action the test is asserting changed the server's
    /// state.
    sequenced: std::sync::Mutex<HashMap<String, VecDeque<CommandOutput>>>,
    available: Vec<String>,
    pub calls: std::sync::Mutex<Vec<String>>,
    /// Invocation, the environment it was given, and the bytes it was given on
    /// stdin — so a test can assert a task ran against the right configuration
    /// directory and not merely that it ran, and can assert a piped payload
    /// actually *arrived* rather than only that it was absent from argv.
    ///
    /// Recording stdin is not a convenience. Every "the secret travels on
    /// stdin" test is otherwise half-blind: deleting the `stdin: Some(…)` from
    /// the call site leaves the token absent from argv, which is all such a
    /// test could see, so it stays green while the child receives an empty
    /// pipe — an empty `session.token` written 0600 and reported as success.
    /// The clipboard channel has no argv half at all: a clipboard write is
    /// *only* its stdin, so without this a test could assert the invocation and
    /// still not know whether the bytes survived.
    pub recorded: std::sync::Mutex<Vec<Recorded>>,
    /// Scripted children, queued per key and consumed in spawn order. A
    /// standing stub cannot express what the supervisor is judged on: the
    /// backoff schedule is a property of the *sequence* of failures, and a stub
    /// answering every spawn identically cannot tell the first attempt from the
    /// fourth.
    children: std::sync::Mutex<HashMap<String, VecDeque<Ending>>>,
    /// Every child started, in order, each still reachable so a test can ask
    /// afterwards whether it was killed.
    spawned: std::sync::Mutex<Vec<Arc<FakeChild>>>,
}

/// How a scripted child ends.
#[cfg(test)]
enum Ending {
    /// It exits on its own, with this status and stderr.
    Alone(CommandOutput),
    /// It stays up until something kills it — the tunnel that is *working*.
    /// Without this a ping-timeout test could never reach the teardown it is
    /// about: the child would exit first and the supervisor would be rebuilding
    /// after a clean exit rather than tearing down a wedged forward.
    OnlyWhenKilled,
}

/// One child the fake started.
#[cfg(test)]
struct FakeChild {
    invocation: String,
    /// `None` for a child scripted to stay up: `wait` then resolves only once
    /// `kill` has been called.
    exit: Option<CommandOutput>,
    killed: std::sync::Mutex<bool>,
    /// Wakes a pending `wait`. `notify_one` rather than `notify_waiters`
    /// because it stores a permit when nobody is waiting yet — a kill that
    /// lands between `wait` reading the flag and registering itself would
    /// otherwise leave the waiter parked on a child that is already dead.
    stopped: tokio::sync::Notify,
}

#[cfg(test)]
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
        self.stopped.notify_one();
        Ok(())
    }
}

/// One recorded invocation: what was run, with what environment, and what was
/// piped to it.
#[cfg(test)]
pub struct Recorded {
    pub invocation: String,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    /// Whether the caller asked for the pty. Recorded rather than acted on:
    /// there is no terminal under `cargo test`, so the real path is the plain
    /// inherit either way, and what a test can still assert is the *intent*.
    pub subdued: bool,
}

#[cfg(test)]
impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(self, invocation: &str, code: i32, stdout: &str, stderr: &str) -> Self {
        self.with_env(invocation, &[], code, stdout, stderr)
    }

    /// A stub that only answers when the environment carries every named pair.
    pub fn with_env(
        mut self,
        invocation: &str,
        env: &[(&str, &str)],
        code: i32,
        stdout: &str,
        stderr: &str,
    ) -> Self {
        self.responses.push(Stub {
            invocation: invocation.to_string(),
            env: env
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            output: CommandOutput {
                code: Some(code),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            },
            bytes: None,
            fragment: false,
        });
        self.make_available(invocation);
        self
    }

    /// Scripts a command whose stdout is binary.
    ///
    /// One stub, like every other, so `which`, the exit code and the env
    /// matching all resolve through exactly the same path as `with` — only
    /// stdout differs.
    pub fn with_bytes(mut self, invocation: &str, code: i32, stdout: &[u8], stderr: &str) -> Self {
        self = self.with(invocation, code, "", stderr);
        if let Some(stub) = self.responses.last_mut() {
            stub.bytes = Some(stdout.to_vec());
        }
        self
    }

    /// Stubs on a fragment appearing anywhere in the invocation, for commands
    /// whose distinguishing part is not at the front — `ssh … host uname -sm`.
    pub fn containing(mut self, fragment: &str, code: i32, stdout: &str, stderr: &str) -> Self {
        self.responses.push(Stub {
            invocation: fragment.to_string(),
            env: Vec::new(),
            output: CommandOutput {
                code: Some(code),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            },
            bytes: None,
            fragment: true,
        });
        self.make_available(fragment);
        self
    }

    /// Queues a response after any already queued for this key, consumed in
    /// call order and ahead of `with`/`containing`. Additive: once the queue
    /// for a key empties, a later matching call falls through to those as
    /// before, so existing stubs are unaffected by a test that never calls
    /// this.
    pub fn then(mut self, invocation: &str, code: i32, stdout: &str, stderr: &str) -> Self {
        self.sequenced
            .get_mut()
            .unwrap()
            .entry(invocation.to_string())
            .or_default()
            .push_back(CommandOutput {
                code: Some(code),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            });
        self.make_available(invocation);
        self
    }

    /// Scripts a child that starts and then exits on its own.
    ///
    /// Queued per invocation and consumed in spawn order, the way `then` queues
    /// responses, so successive calls script successive attempts — which is
    /// what an assertion about the backoff schedule needs, since the delay
    /// after the first failure is not the delay after the fourth.
    pub fn spawning(mut self, invocation: &str, code: i32, stderr: &str) -> Self {
        self.queue_child(
            invocation,
            Ending::Alone(CommandOutput {
                code: Some(code),
                stdout: String::new(),
                stderr: stderr.to_string(),
            }),
        );
        self
    }

    /// Scripts a child that stays up until it is killed — the tunnel that came
    /// up fine and then went quiet. Queued alongside `spawning`, so a test can
    /// script a live child, then the one that replaces it after teardown.
    pub fn spawning_until_killed(mut self, invocation: &str) -> Self {
        self.queue_child(invocation, Ending::OnlyWhenKilled);
        self
    }

    fn queue_child(&mut self, invocation: &str, ending: Ending) {
        self.children
            .get_mut()
            .unwrap()
            .entry(invocation.to_string())
            .or_default()
            .push_back(ending);
        self.make_available(invocation);
    }

    /// Marks the program a stub names as resolvable by `which`, so scripting a
    /// command is also what makes it look installed.
    fn make_available(&mut self, invocation: &str) {
        let program = invocation.split_whitespace().next().unwrap_or_default();
        if !program.is_empty() && !self.available.iter().any(|p| p == program) {
            self.available.push(program.to_string());
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The invocations that asked for a pty.
    ///
    /// `calls()` answers what ran; this answers which of it riabuild took
    /// responsibility for the look of. The split matters because it is not a
    /// property of the command — the same `gh` runs subdued for a sign-in and
    /// unsubdued everywhere else.
    pub fn subdued_calls(&self) -> Vec<String> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.subdued)
            .map(|call| call.invocation.clone())
            .collect()
    }

    /// Every child started, in spawn order.
    pub fn spawns(&self) -> Vec<String> {
        self.spawned
            .lock()
            .unwrap()
            .iter()
            .map(|child| child.invocation.clone())
            .collect()
    }

    /// The children whose handles were killed, in spawn order.
    ///
    /// The teardown half of the supervisor's contract, and the half `calls()`
    /// structurally cannot show — a kill is not an invocation. A ping-timeout
    /// test asserting only that a second tunnel was spawned passes just as well
    /// against a supervisor that leaks every wedged ssh it replaces, which on a
    /// laptop that suspends and resumes all day is a process per resume, each
    /// still holding a forward.
    pub fn killed(&self) -> Vec<String> {
        self.spawned
            .lock()
            .unwrap()
            .iter()
            .filter(|child| *child.killed.lock().unwrap())
            .map(|child| child.invocation.clone())
            .collect()
    }

    /// The environment the first matching invocation was run with.
    pub fn env_of(&self, prefix: &str) -> Vec<(String, String)> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.invocation.starts_with(prefix))
            .map(|call| call.env.clone())
            .unwrap_or_default()
    }

    /// The bytes the first matching invocation was given on stdin, or `None`
    /// if it was given none.
    ///
    /// The positive half of every "a secret travels on stdin, never in argv"
    /// assertion: `calls()` can only ever show a secret's *absence* from the
    /// command line, which a call site that pipes nothing at all satisfies
    /// just as well as one that pipes correctly.
    pub fn stdin_of(&self, prefix: &str) -> Option<Vec<u8>> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.invocation.starts_with(prefix))
            .and_then(|call| call.stdin.clone())
    }

    /// [`Self::stdin_of`] decoded as UTF-8, for the common case where the
    /// piped payload is a token rather than a binary.
    pub fn stdin_text_of(&self, prefix: &str) -> Option<String> {
        self.stdin_of(prefix)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The stdin every call was given, as `(invocation, bytes)`. Calls that
    /// were piped nothing are left out, so this is the list of writes.
    pub fn inputs(&self) -> Vec<(String, Vec<u8>)> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .filter_map(|call| {
                call.stdin
                    .clone()
                    .map(|bytes| (call.invocation.clone(), bytes))
            })
            .collect()
    }

    /// The bytes piped into the first call whose invocation contains `needle`.
    ///
    /// The fragment-matching twin of [`Self::stdin_of`], for a call whose
    /// distinguishing part is not at the front of the command line.
    pub fn input_for(&self, needle: &str) -> Option<Vec<u8>> {
        self.inputs()
            .into_iter()
            .find(|(invocation, _)| invocation.contains(needle))
            .map(|(_, bytes)| bytes)
    }

    fn record(&self, program: &str, args: &[&str], options: &RunOptions) -> String {
        let invocation = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.calls.lock().unwrap().push(invocation.clone());
        self.recorded.lock().unwrap().push(Recorded {
            invocation: invocation.clone(),
            env: options.env.clone(),
            stdin: options.stdin.clone(),
            subdued: options.subdued.is_some(),
        });
        invocation
    }

    /// The invocation a stub is matched against.
    ///
    /// The program is reduced to its file name, because riabuild runs the tools
    /// it owns by absolute path — `~/.riabuild/gh/2.97.0/bin/gh`, under a
    /// per-test tempdir. Without this every stub would have to be built from a
    /// path the test does not care about, and `with("gh --version", …)` would
    /// stop meaning "when gh is asked its version".
    ///
    /// `calls` still records the full path, so a test can assert riabuild ran
    /// *its* gh rather than whatever is on `PATH`.
    fn stub_key(program: &str, args: &[&str]) -> String {
        let name = Path::new(program)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string());
        format!("{name} {}", args.join(" ")).trim_end().to_string()
    }

    /// Pops the next response queued by `then()` for the longest matching
    /// key (same prefix rule as `responses`), leaving an exhausted queue in
    /// place so a later call matching the same key falls through to the
    /// ordinary stubs instead of finding nothing.
    fn next_queued(&self, invocation: &str) -> Option<CommandOutput> {
        let mut sequenced = self.sequenced.lock().unwrap();
        let key = sequenced
            .keys()
            .filter(|key| invocation == key.as_str() || invocation.starts_with(&format!("{key} ")))
            .max_by_key(|key| key.len())
            .cloned()?;
        sequenced.get_mut(&key).and_then(VecDeque::pop_front)
    }

    /// Pops the next child queued for the longest matching key, by the same
    /// prefix rule the response stubs use — `spawning("ssh", …)` answers for
    /// the whole `ssh -N -R … ada@box` command line the supervisor builds,
    /// which no test should have to spell out to script an exit.
    fn next_child(&self, invocation: &str) -> Option<Ending> {
        let mut children = self.children.lock().unwrap();
        let key = children
            .keys()
            .filter(|key| invocation == key.as_str() || invocation.starts_with(&format!("{key} ")))
            .max_by_key(|key| key.len())
            .cloned()?;
        children.get_mut(&key).and_then(VecDeque::pop_front)
    }

    /// Finds a stub for an invocation, by full program path or by file name.
    ///
    /// Both, because tasks run some binaries by absolute path and others by
    /// name, and a test should be able to say whichever it means. `toolchain`
    /// stubs the exact `~/.riabuild/node/<version>/bin/node` it is asserting
    /// about; `github_cli` says `gh --version` and does not care where gh is.
    ///
    /// A response queued by `then()` is consumed first, and consumed exactly
    /// once per call: the queue lookup that finds nothing pops nothing, so
    /// trying the file-name key after the full one cannot eat a second entry.
    fn resolve(&self, program: &str, args: &[&str], options: &RunOptions) -> Option<CommandOutput> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        let key = FakeRunner::stub_key(program, args);
        if let Some(output) = self
            .next_queued(&full)
            .or_else(|| self.next_queued(key.as_str()))
        {
            return Some(output);
        }
        self.stubbed(&full, options)
            .or_else(|| self.stubbed(&key, options))
    }

    fn stubbed(&self, invocation: &str, options: &RunOptions) -> Option<CommandOutput> {
        self.matching(invocation, options)
            .map(|stub| stub.output.clone())
    }

    /// The stub an invocation selects, if any.
    ///
    /// Shared by the text and byte lookups so a binary stub can never be
    /// selected by different rules from the text one beside it.
    fn matching(&self, invocation: &str, options: &RunOptions) -> Option<&Stub> {
        self.responses
            .iter()
            .filter(|stub| {
                let name_matches = if stub.fragment {
                    invocation.contains(stub.invocation.as_str())
                } else {
                    invocation == stub.invocation
                        || invocation.starts_with(&format!("{} ", stub.invocation))
                };
                name_matches
                    && stub
                        .env
                        .iter()
                        .all(|(key, value)| options.env.iter().any(|(k, v)| k == key && v == value))
            })
            // Most specific wins: the longest command, then the most
            // environment entries. `max_by_key` keeps the last of equal
            // candidates, so a later identical stub replaces an earlier one —
            // which is what the map this replaced did.
            //
            // Command length is compared before env-pair count, so a longer
            // env-less stub outranks a shorter env-scoped one: `with("claude
            // auth status --json")` beats `with_env("claude auth", &[("CLAUDE_
            // CONFIG_DIR", "/one")])` for `claude auth status --json` run in
            // `/one`. That is fine for the account use case, where every
            // account is asked the identical command string, but it means env
            // specificity only breaks ties between stubs that already match on
            // the same invocation length.
            //
            // Length is also what settles a fragment against a prefix: a longer
            // match is a more specific stub, so a prefix stub on `"ssh"` cannot
            // silently answer for every remote invocation a fragment stub
            // scripts. The last tuple element keeps a prefix stub ahead of a
            // fragment of the identical length, which is the direction the
            // fragment rule was originally written with.
            .max_by_key(|stub| {
                (
                    stub.invocation.len(),
                    stub.env.len(),
                    u8::from(!stub.fragment),
                )
            })
    }

    /// The byte-stub twin of `resolve`, matched by exactly the same rules so a
    /// binary stub cannot be selected differently from a text one.
    fn resolve_bytes(&self, program: &str, args: &[&str], options: &RunOptions) -> Option<Vec<u8>> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.matching(&full, options)
            .or_else(|| self.matching(&FakeRunner::stub_key(program, args), options))
            .and_then(|stub| stub.bytes.clone())
    }

    fn lookup(&self, program: &str, args: &[&str], options: &RunOptions) -> CommandOutput {
        self.resolve(program, args, options)
            .unwrap_or_else(|| CommandOutput {
                code: Some(127),
                stdout: String::new(),
                stderr: format!(
                    "fake runner: no stub for `{}`",
                    FakeRunner::stub_key(program, args)
                ),
            })
    }
}

#[cfg(test)]
#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
        self.record(program, args, options);
        Ok(self.lookup(program, args, options))
    }

    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput> {
        self.record(program, args, options);

        let text = self.lookup(program, args, options);
        // A test that only cares about the exit code can stub with `with` and
        // still be read through `run_bytes`.
        let stdout = self
            .resolve_bytes(program, args, options)
            .unwrap_or_else(|| text.stdout.into_bytes());

        Ok(BytesOutput {
            code: text.code,
            stdout,
            stderr: text.stderr,
        })
    }

    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32> {
        self.record(program, args, options);
        Ok(self.lookup(program, args, options).code.unwrap_or(0))
    }

    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>> {
        let invocation = self.record(program, args, options);
        let key = FakeRunner::stub_key(program, args);
        // Unscripted spawns end the way unstubbed commands do, rather than
        // hanging: a test that forgot to script the second attempt should read
        // "no stub" in a failed assertion, not time out.
        let ending = self
            .next_child(&invocation)
            .or_else(|| self.next_child(&key))
            .unwrap_or_else(|| {
                Ending::Alone(CommandOutput {
                    code: Some(127),
                    stdout: String::new(),
                    stderr: format!("fake runner: no stub for `{key}`"),
                })
            });

        let child = Arc::new(FakeChild {
            invocation,
            exit: match ending {
                Ending::Alone(output) => Some(output),
                Ending::OnlyWhenKilled => None,
            },
            killed: std::sync::Mutex::new(false),
            stopped: tokio::sync::Notify::new(),
        });
        // Kept here as well as handed out, so `killed()` can answer after the
        // code under test has dropped its handle.
        self.spawned.lock().unwrap().push(child.clone());
        Ok(Box::new(child))
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        self.record(program, args, options);
        // A stub's exit code applies here too: interactive commands fail as
        // well — a developer who abandons a device-code prompt leaves `gh`
        // exiting non-zero — and a task that ignores that reports a sign-in
        // it never got. Unstubbed commands still succeed, so tests that only
        // care about which commands ran need not script every prompt.
        Ok(self
            .resolve(program, args, options)
            .and_then(|output| output.code)
            .unwrap_or(0))
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.available
            .iter()
            .any(|p| p == program)
            .then(|| PathBuf::from(format!("/usr/bin/{program}")))
    }
}

/// A `CommandRunner` that adds a fixed environment to every command.
///
/// This is why `github_cli` cannot authenticate the wrong developer on a shared
/// server. `GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` are not something each task
/// remembers to pass — the runner every task already holds carries them, so a
/// task that forgets is not a thing anyone can write.
///
/// Whatever keys the scope was constructed with are applied *after* the
/// caller's `RunOptions.env`, so they are not overridable:
/// `std::process::Command::env()` (see `RealRunner::build`) overwrites on a
/// repeated key, and the scope's entries are the ones applied last. A task
/// cannot escape its namespace even by naming one of these keys itself —
/// accidentally (a copy-pasted env vector from another task) or otherwise.
/// Every other variable a caller sets — `env_local`'s `INFISICAL_TOKEN`, for
/// instance — has nothing here to collide with, so it reaches the child
/// untouched. See the precedence tests below, including one that pins the
/// collision case: it is written to fail if the merge order were ever put back
/// the other way around.
///
/// The un-overridable set is exactly what `main.rs` puts in, and today that is
/// **`GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` only**. `RIABUILD_ROOT` is *not*
/// in it: it reaches children by ordinary process-environment inheritance
/// instead, which the precedence rule above does not cover, so a task that put
/// `RIABUILD_ROOT` in its own `RunOptions.env` would win over the inherited
/// value. No task does that today. Do not read this comment as saying the
/// namespace root is protected the way the two config paths are — if that
/// protection is ever wanted, `main.rs` has to add the key here.
///
/// Every method scopes, including the ones the laptop channel added: a
/// clipboard read through `run_bytes`, a clipboard write through
/// `run_forking`, and the tunnel held open by `spawn` all reach the child with
/// the same environment `run` would have given it, so no route through this
/// trait is an unscoped one. `spawn` matters most of the three — it is the
/// longest-lived child riabuild starts, so an unscoped one would keep pointing
/// at the wrong developer's configuration for the whole session.
pub struct ScopedRunner {
    inner: Arc<dyn CommandRunner>,
    env: Vec<(String, String)>,
}

impl ScopedRunner {
    pub fn new(inner: Arc<dyn CommandRunner>, env: Vec<(String, String)>) -> Self {
        Self { inner, env }
    }

    fn merge(&self, options: &RunOptions) -> RunOptions {
        let mut merged = options.clone();
        let mut env = options.env.clone();
        env.extend(self.env.iter().cloned());
        merged.env = env;
        merged
    }
}

#[async_trait]
impl CommandRunner for ScopedRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
        self.inner.run(program, args, &self.merge(options)).await
    }

    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput> {
        self.inner
            .run_bytes(program, args, &self.merge(options))
            .await
    }

    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32> {
        self.inner
            .run_forking(program, args, &self.merge(options))
            .await
    }

    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>> {
        self.inner.spawn(program, args, &self.merge(options)).await
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        self.inner
            .run_interactive(program, args, &self.merge(options))
            .await
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.inner.which(program)
    }
}

#[cfg(test)]
mod bytes_tests {
    use super::*;

    /// A PNG is not valid UTF-8. Read through `run`, its bytes come back
    /// mangled into replacement characters; `run_bytes` is what makes the
    /// clipboard channel possible at all.
    #[tokio::test]
    async fn binary_stdout_survives_the_runner() {
        // PNG magic, then a byte that is illegal as UTF-8 on its own.
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF];
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, &png, "");

        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();

        assert!(out.ok());
        assert_eq!(out.stdout, png);
    }

    /// The bug this whole method exists to avoid, pinned against the real
    /// runner so nobody "simplifies" the clipboard backends back onto `run`.
    #[tokio::test]
    async fn the_same_bytes_through_run_would_have_been_corrupted() {
        let png = [0x89u8, b'P', b'N', b'G', 0xFF];
        let emit = ["-c", r"printf '\211PNG\377'"];

        let lossy = RealRunner
            .run("sh", &emit, &RunOptions::default())
            .await
            .unwrap();
        let raw = RealRunner
            .run_bytes("sh", &emit, &RunOptions::default())
            .await
            .unwrap();

        assert_eq!(raw.stdout, png);
        assert_ne!(lossy.stdout.as_bytes(), png);
        assert!(
            lossy.stdout.contains('\u{FFFD}'),
            "expected replacement characters, got {:?}",
            lossy.stdout
        );
    }

    #[tokio::test]
    async fn an_unstubbed_command_fails_the_same_way_as_run() {
        let runner = FakeRunner::new();
        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(out.code, Some(127));
        assert!(out.stdout.is_empty());
        assert!(out.stderr.contains("no stub"), "{}", out.stderr);
    }

    #[tokio::test]
    async fn bytes_calls_are_recorded_like_every_other_call() {
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, b"hi", "");
        runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(runner.calls(), vec!["xclip -o".to_string()]);
    }

    /// The real runner is exercised through a shell builtin every supported
    /// platform has, so this stays a unit test rather than a fixture.
    #[tokio::test]
    async fn the_real_runner_returns_raw_bytes() {
        let out = RealRunner
            .run_bytes(
                "sh",
                &["-c", r"printf '\211PNG\377'"],
                &RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout, [0x89u8, b'P', b'N', b'G', 0xFF]);
    }
}

#[cfg(test)]
mod spawn_tests {
    use super::*;
    use std::time::Duration;

    /// The whole reason `spawn` exists: the call returns while the child is
    /// still running, and stderr is still there to be read when it finally
    /// exits — which is all `supervisor::diagnose` has to work from.
    #[tokio::test]
    async fn a_spawned_child_is_handed_back_before_it_has_finished() {
        let handle = RealRunner
            .spawn(
                "sh",
                &["-c", "printf 'refused the forward' >&2; exit 3"],
                &RunOptions::default(),
            )
            .await
            .expect("spawns");

        let output = handle.wait().await.expect("waits");
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stderr, "refused the forward");
    }

    /// The teardown path, and the reason `wait` takes `&self`: the handle is
    /// still usable while a wait on it is outstanding, which is what lets the
    /// supervisor kill a tunnel that has gone quiet instead of waiting out an
    /// exit that is never coming.
    #[tokio::test]
    async fn a_child_can_be_killed_while_a_wait_on_it_is_outstanding() {
        let handle = RealRunner
            .spawn("sleep", &["30"], &RunOptions::default())
            .await
            .expect("spawns");

        tokio::select! {
            _ = handle.wait() => panic!("`sleep 30` should still be running"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        handle.kill().await.expect("kills");
        let output = handle.wait().await.expect("waits");
        assert_eq!(output.code, None, "a killed process exits by signal");
    }

    /// A ping timeout fires without knowing whether ssh has meanwhile exited
    /// on its own, so a second kill has to be a no-op rather than an error the
    /// supervisor has to special-case.
    #[tokio::test]
    async fn killing_a_child_that_has_already_gone_is_not_an_error() {
        let handle = RealRunner
            .spawn("true", &[], &RunOptions::default())
            .await
            .expect("spawns");

        handle.wait().await.expect("waits");
        handle.kill().await.expect("first kill");
        handle.kill().await.expect("second kill");
    }

    /// Without `kill_on_drop`, a handle dropped anywhere above the supervisor
    /// leaves an ssh alive holding the remote socket, the next attempt cannot
    /// bind it, and the channel comes up permanently dead. Asserted through a
    /// file the child only creates if it outlived the handle.
    #[tokio::test]
    async fn a_dropped_handle_does_not_leave_the_child_running() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let marker = dir.path().join("still-running");
        let script = format!("sleep 0.3; : > {}", marker.display());

        let handle = RealRunner
            .spawn("sh", &["-c", &script], &RunOptions::default())
            .await
            .expect("spawns");
        drop(handle);

        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(
            tokio::fs::metadata(&marker).await.is_err(),
            "the child outlived its handle"
        );
    }

    /// A spawned child is as much a namespaced process as any other, and the
    /// longest-lived one riabuild starts — an unscoped tunnel would point at
    /// the wrong developer's configuration for the whole session.
    #[tokio::test]
    async fn a_spawned_child_is_scoped_too() {
        let fake = Arc::new(FakeRunner::new().spawning("ssh", 0, ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())],
        );

        scoped
            .spawn("ssh", &["-N"], &RunOptions::default())
            .await
            .expect("spawns");

        assert_eq!(
            fake.env_of("ssh -N"),
            vec![("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())]
        );
    }

    /// The backoff schedule is a property of the *sequence* of failures, so
    /// the fake has to be able to tell the first attempt from the third.
    #[tokio::test]
    async fn successive_spawns_get_successive_scripted_endings() {
        let fake = FakeRunner::new()
            .spawning("ssh", 255, "Connection refused")
            .spawning("ssh", 255, "Bad remote forwarding specification")
            .spawning("ssh", 0, "");
        let args = ["-N", "-R", "/run/sock:/tmp/sock", "ada@box"];

        let mut endings = Vec::new();
        for _ in 0..3 {
            let handle = fake
                .spawn("ssh", &args, &RunOptions::default())
                .await
                .expect("spawns");
            let output = handle.wait().await.expect("waits");
            endings.push((output.code, output.stderr));
        }

        assert_eq!(endings[0], (Some(255), "Connection refused".to_string()));
        assert_eq!(
            endings[1],
            (Some(255), "Bad remote forwarding specification".to_string())
        );
        assert_eq!(endings[2], (Some(0), String::new()));
    }

    /// The tunnel that came up fine and then went quiet. A ping-timeout test
    /// needs a child that is still there to be torn down; one that exits on
    /// its own puts the supervisor on the rebuild-after-clean-exit path
    /// instead, which is a different behaviour entirely.
    #[tokio::test]
    async fn a_child_scripted_to_stay_up_resolves_only_once_it_is_killed() {
        let fake = FakeRunner::new().spawning_until_killed("ssh");
        let handle = fake
            .spawn("ssh", &["-N"], &RunOptions::default())
            .await
            .expect("spawns");

        tokio::select! {
            _ = handle.wait() => panic!("a live child must not resolve on its own"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }

        handle.kill().await.expect("kills");
        let output = handle.wait().await.expect("waits");
        assert_eq!(output.code, None);
    }

    /// `calls()` can never show a teardown, because a kill is not an
    /// invocation — so without `killed()` a supervisor that leaked every
    /// wedged ssh it replaced would pass a rebuild test unchanged.
    #[tokio::test]
    async fn the_fake_records_which_children_were_started_and_which_were_killed() {
        let fake = FakeRunner::new()
            .spawning_until_killed("ssh")
            .spawning_until_killed("ssh");

        let first = fake
            .spawn("ssh", &["-N", "one"], &RunOptions::default())
            .await
            .expect("spawns");
        let second = fake
            .spawn("ssh", &["-N", "two"], &RunOptions::default())
            .await
            .expect("spawns");
        first.kill().await.expect("kills");
        drop(first);
        drop(second);

        assert_eq!(fake.spawns(), vec!["ssh -N one", "ssh -N two"]);
        assert_eq!(fake.killed(), vec!["ssh -N one"]);
    }

    /// A test that forgot to script an attempt should read "no stub" in a
    /// failed assertion rather than time out waiting on a child nobody ever
    /// told how to end.
    #[tokio::test]
    async fn an_unscripted_spawn_ends_the_way_an_unstubbed_command_does() {
        let fake = FakeRunner::new();
        let handle = fake
            .spawn("ssh", &["-N"], &RunOptions::default())
            .await
            .expect("spawns");
        let output = handle.wait().await.expect("waits");

        assert_eq!(output.code, Some(127));
        assert!(output.stderr.contains("no stub"), "{}", output.stderr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_dir(dir: &str) -> RunOptions {
        RunOptions {
            env: vec![("CLAUDE_CONFIG_DIR".to_string(), dir.to_string())],
            ..Default::default()
        }
    }

    fn in_env(key: &str, value: &str) -> RunOptions {
        RunOptions {
            env: vec![(key.to_string(), value.to_string())],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_scoped_runner_puts_its_environment_on_every_command() {
        let fake = Arc::new(FakeRunner::new().with("gh auth status", 0, "", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/run/user/1000/riabuild-gh".into())],
        );

        scoped
            .run("gh", &["auth", "status"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("gh auth status"),
            vec![(
                "GH_CONFIG_DIR".to_string(),
                "/run/user/1000/riabuild-gh".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn a_caller_can_still_add_its_own_environment() {
        // `env_local` passes INFISICAL_TOKEN this way. The scope adds to that,
        // never replaces it.
        let fake = Arc::new(FakeRunner::new().with("infisical export", 0, "A=b\n", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())],
        );

        scoped
            .run(
                "infisical",
                &["export"],
                &RunOptions {
                    env: vec![("INFISICAL_TOKEN".into(), "st.secret".into())],
                    ..Default::default()
                },
            )
            .await
            .expect("runs");

        let env = fake.env_of("infisical export");
        assert!(env.contains(&("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())));
        assert!(env.contains(&("INFISICAL_TOKEN".to_string(), "st.secret".to_string())));
    }

    #[tokio::test]
    async fn an_interactive_command_is_scoped_too() {
        // `gh auth login` is interactive, and it is exactly the command that must
        // not write into another developer's configuration directory.
        let fake = Arc::new(FakeRunner::new().with("gh auth login", 0, "", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())],
        );

        scoped
            .run_interactive("gh", &["auth", "login"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("gh auth login"),
            vec![("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())]
        );
    }

    #[tokio::test]
    async fn the_byte_and_forking_commands_are_scoped_too() {
        // The two methods the laptop channel added. A clipboard read and a
        // clipboard write are ordinary children of a namespaced session, so a
        // route through the trait that skipped `merge()` would be an unscoped
        // command nobody would notice until two developers shared a server.
        let fake = Arc::new(
            FakeRunner::new()
                .with_bytes("xclip -o", 0, b"copied", "")
                .with("xclip -i", 0, "", ""),
        );
        let scoped = ScopedRunner::new(fake.clone(), vec![("DISPLAY".into(), ":17".into())]);

        scoped
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .expect("runs");
        scoped
            .run_forking("xclip", &["-i"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("xclip -o"),
            vec![("DISPLAY".to_string(), ":17".to_string())]
        );
        assert_eq!(
            fake.env_of("xclip -i"),
            vec![("DISPLAY".to_string(), ":17".to_string())]
        );
    }

    #[tokio::test]
    async fn a_caller_cannot_override_a_namespace_key() {
        // A task cannot escape its namespace even by naming one of the scope's
        // own keys itself — accidentally (a copy-pasted env vector from another
        // task) or otherwise. Both entries still reach the inner runner (this
        // type does not deduplicate), but `std::process::Command::env` (see
        // `RealRunner::build`) overwrites on a repeated key with whichever call
        // came last, and the scope's entry is appended after the caller's in
        // `merge()` — so it is the scope's value the real child process sees.
        // This is written to fail if that merge order were ever put back the
        // other way around: see "Prove it bites" in the Task 8 report.
        let fake = Arc::new(FakeRunner::new().with("gh auth status", 0, "", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/run/user/1000/riabuild-gh".into())],
        );

        scoped
            .run(
                "gh",
                &["auth", "status"],
                &RunOptions {
                    env: vec![("GH_CONFIG_DIR".into(), "/tmp/some-other-place".into())],
                    ..Default::default()
                },
            )
            .await
            .expect("runs");

        let env = fake.env_of("gh auth status");
        assert_eq!(
            env,
            vec![
                (
                    "GH_CONFIG_DIR".to_string(),
                    "/tmp/some-other-place".to_string()
                ),
                (
                    "GH_CONFIG_DIR".to_string(),
                    "/run/user/1000/riabuild-gh".to_string()
                ),
            ],
            "the scope's entry must be last, since std::process::Command::env() lets the last call for a key win"
        );
    }

    #[tokio::test]
    async fn the_scoped_environment_reaches_a_real_child_process_not_just_the_struct() {
        // Everything above goes through `FakeRunner`, which proves the merged
        // environment is threaded through the call, not merely stored on
        // `ScopedRunner`. This test closes the last gap by running a real
        // process and reading its actual environment back out of its stdout.
        let scoped = ScopedRunner::new(
            Arc::new(RealRunner),
            vec![(
                "RIABUILD_SCOPED_RUNNER_TEST".into(),
                "namespaced-value".into(),
            )],
        );

        let output = scoped
            .run("env", &[], &RunOptions::default())
            .await
            .expect("runs");

        assert!(
            output
                .stdout
                .lines()
                .any(|line| line == "RIABUILD_SCOPED_RUNNER_TEST=namespaced-value"),
            "child environment did not contain the scoped variable:\n{}",
            output.stdout
        );
    }

    #[tokio::test]
    async fn a_child_receives_bytes_that_are_not_valid_utf8() {
        // A gzip header is not valid UTF-8, and Task 17 streams a whole binary
        // through here. Asserting a field returns what was just assigned to
        // it would prove nothing about the code that could get
        // bytes-versus-UTF-8 wrong; this runs a real child instead.
        //
        // The check goes through `wc -c` rather than `cat` + a byte-length
        // comparison on `stdout`: `CommandOutput::stdout` is a lossy-decoded
        // `String` (`String::from_utf8_lossy`), and every invalid byte in
        // these six becomes a 3-byte U+FFFD replacement on the way back out
        // — echoing the input and measuring the echo would be measuring the
        // lossy decoder, not what the child actually received on stdin.
        // `wc -c` reports the byte count as plain ASCII digits, which round-
        // trips through that same lossy decoding unchanged, so it proves the
        // six raw bytes reached the child intact without depending on stdout
        // being representable as UTF-8 at all.
        let bytes = vec![0x1f, 0x8b, 0x08, 0x00, 0xff, 0xfe];
        let output = RealRunner
            .run(
                "wc",
                &["-c"],
                &RunOptions {
                    stdin: Some(bytes.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("wc runs");
        assert_eq!(output.trimmed(), bytes.len().to_string());
    }

    #[tokio::test]
    async fn the_fake_records_piped_bytes_and_reports_none_when_nothing_was_piped() {
        // The accessor every "a secret travels on stdin" test now leans on. It
        // has to distinguish the two cases, not merely return something: a
        // `stdin_of` that answered `Some` unconditionally would make all four
        // of those tests green again for exactly the reason they were written.
        let fake = FakeRunner::new()
            .with("security", 0, "", "")
            .with("id", 0, "", "");
        fake.run(
            "security",
            &["add-generic-password"],
            &RunOptions {
                stdin: Some(b"piped-secret".to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect("runs");
        fake.run("id", &[], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.stdin_text_of("security").as_deref(),
            Some("piped-secret")
        );
        assert_eq!(fake.stdin_of("id"), None);
        assert_eq!(fake.stdin_of("never-run"), None);
    }

    #[tokio::test]
    async fn the_stdin_of_a_call_is_also_reachable_by_fragment() {
        // `input_for` is the clipboard channel's reader: `wl-copy` is invoked
        // with the payload's type in front of nothing the test can predict, so
        // it looks the call up by a fragment rather than by a prefix. Both
        // accessors read the one recording, so neither can go stale while the
        // other still works.
        let fake = FakeRunner::new().with("wl-copy", 0, "", "");
        fake.run_forking(
            "wl-copy",
            &["--type", "image/png"],
            &RunOptions {
                stdin: Some(vec![0x89, b'P', b'N', b'G']),
                ..Default::default()
            },
        )
        .await
        .expect("runs");

        assert_eq!(
            fake.input_for("image/png"),
            Some(vec![0x89, b'P', b'N', b'G'])
        );
        assert_eq!(fake.inputs().len(), 1);
        assert_eq!(fake.input_for("xclip"), None);
    }

    #[tokio::test]
    async fn a_fragment_stub_can_answer_for_the_end_of_a_command() {
        let fake = FakeRunner::new()
            .with("ssh", 1, "", "unmatched")
            .containing("uname -sm", 0, "Linux x86_64\n", "");

        let output = fake
            .run(
                "ssh",
                &["-p", "22", "ada@box", "uname -sm"],
                &RunOptions::default(),
            )
            .await
            .expect("runs");
        assert_eq!(output.trimmed(), "Linux x86_64");
    }

    #[tokio::test]
    async fn a_queued_response_is_consumed_before_the_standing_stub() {
        // The "first call fails, second succeeds" shape: a probe run before and
        // after the action a test is asserting changed the server's state. Once
        // the queue empties, the standing stub answers again.
        let fake = FakeRunner::new()
            .with("ssh box cat /token", 0, "standing", "")
            .then("ssh box cat /token", 1, "before", "")
            .then("ssh box cat /token", 0, "after", "");
        let args = ["box", "cat", "/token"];

        let first = fake
            .run("ssh", &args, &RunOptions::default())
            .await
            .expect("runs");
        let second = fake
            .run("ssh", &args, &RunOptions::default())
            .await
            .expect("runs");
        let third = fake
            .run("ssh", &args, &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!((first.trimmed(), first.code), ("before", Some(1)));
        assert_eq!((second.trimmed(), second.code), ("after", Some(0)));
        assert_eq!(
            third.trimmed(),
            "standing",
            "an exhausted queue falls through to the standing stub"
        );
    }

    #[tokio::test]
    async fn a_stub_can_be_scoped_to_an_environment_variable() {
        // The same command, twice, told apart only by the directory it is
        // pointed at — which is exactly how riabuild asks each Claude Code
        // account who it is.
        let runner = FakeRunner::new()
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/one")],
                0,
                r#"{"loggedIn":true,"email":"first@example.com"}"#,
                "",
            )
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/two")],
                1,
                r#"{"loggedIn":false}"#,
                "",
            );

        let one = runner
            .run("claude", &["auth", "status", "--json"], &in_dir("/one"))
            .await
            .unwrap();
        assert!(one.stdout.contains("first@example.com"), "{one:?}");

        let two = runner
            .run("claude", &["auth", "status", "--json"], &in_dir("/two"))
            .await
            .unwrap();
        assert_eq!(two.code, Some(1));
        assert!(two.stdout.contains("false"), "{two:?}");
    }

    #[tokio::test]
    async fn a_stub_with_no_environment_still_matches_anything() {
        let runner = FakeRunner::new().with("claude --version", 0, "2.1.223 (Claude Code)", "");
        let output = runner
            .run("claude", &["--version"], &in_dir("/anywhere"))
            .await
            .unwrap();
        assert!(output.ok(), "{output:?}");
    }

    #[tokio::test]
    async fn an_environment_stub_beats_a_general_one() {
        let runner = FakeRunner::new()
            .with("claude auth status --json", 1, r#"{"loggedIn":false}"#, "")
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/one")],
                0,
                r#"{"loggedIn":true,"email":"first@example.com"}"#,
                "",
            );

        let one = runner
            .run("claude", &["auth", "status", "--json"], &in_dir("/one"))
            .await
            .unwrap();
        assert!(one.stdout.contains("first@example.com"), "{one:?}");

        let other = runner
            .run(
                "claude",
                &["auth", "status", "--json"],
                &in_dir("/elsewhere"),
            )
            .await
            .unwrap();
        assert_eq!(other.code, Some(1));
    }

    #[tokio::test]
    async fn an_invocation_naming_no_account_matches_no_account_specific_stub() {
        // Pins the direction of the match: requiring an env pair is a real
        // requirement, not merely a tie-breaker. A caller that names no
        // `CLAUDE_CONFIG_DIR` at all must come away empty-handed even though
        // two stubs exist for this exact command — each scoped to a different
        // account. If this ever passed, the account-lookup feature could go
        // green in its own tests while the production code never actually
        // threaded `CLAUDE_CONFIG_DIR` through to `claude auth status --json`
        // — every account would silently be answered by whichever stub ranks
        // first.
        let runner = FakeRunner::new()
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/one")],
                0,
                r#"{"loggedIn":true,"email":"first@example.com"}"#,
                "",
            )
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/two")],
                1,
                r#"{"loggedIn":false}"#,
                "",
            );

        let unscoped = runner
            .run(
                "claude",
                &["auth", "status", "--json"],
                &RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(unscoped.code, Some(127), "{unscoped:?}");
        assert!(
            unscoped
                .stderr
                .contains("fake runner: no stub for `claude auth status --json`"),
            "{unscoped:?}"
        );
    }

    #[tokio::test]
    async fn a_later_stub_replaces_an_identical_earlier_one() {
        let runner = FakeRunner::new()
            .with("claude --version", 0, "2.0.0 (Claude Code)", "")
            .with("claude --version", 0, "2.1.223 (Claude Code)", "");
        let output = runner
            .run("claude", &["--version"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(output.trimmed(), "2.1.223 (Claude Code)");
    }

    /// A binary stub is selected by exactly the same rules as a text one, so
    /// the byte lookup cannot quietly diverge from the command that ran.
    #[tokio::test]
    async fn a_byte_stub_can_be_scoped_to_an_environment_variable_too() {
        let runner = FakeRunner::new()
            .with_bytes("xclip -o", 0, b"\x89PNG\xFF", "")
            .with_env("xclip -o", &[("DISPLAY", ":1")], 1, "", "no display");

        let scoped = runner
            .run_bytes("xclip", &["-o"], &in_env("DISPLAY", ":1"))
            .await
            .unwrap();
        assert_eq!(scoped.code, Some(1), "the env-scoped stub should win");

        let plain = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(plain.stdout, b"\x89PNG\xFF");
    }
}

#[cfg(test)]
mod subdued_tests {
    use super::*;

    #[test]
    fn no_terminal_means_no_subduing_whatever_the_caller_asked_for() {
        // CI, `cargo test`, a pipe. An unattended run must not take a
        // different code path from an attended one for a cosmetic reason.
        assert_eq!(should_subdue(false, Some(Theme::plain())), None);
    }

    #[test]
    fn a_terminal_and_a_theme_is_the_only_combination_that_subdues() {
        assert_eq!(
            should_subdue(true, Some(Theme::plain())),
            Some(Theme::plain())
        );
        assert_eq!(should_subdue(true, None), None);
        assert_eq!(should_subdue(false, None), None);
    }

    #[test]
    fn the_default_run_is_not_subdued() {
        // Every existing call site constructs this way, and none of them
        // changes behaviour because this field was added.
        assert_eq!(RunOptions::default().subdued, None);
    }

    #[tokio::test]
    async fn the_stub_records_which_commands_were_subdued() {
        let runner = FakeRunner::new();
        runner
            .run_interactive(
                "sudo",
                &["apt-get", "update"],
                &RunOptions {
                    subdued: Some(Theme::plain()),
                    ..Default::default()
                },
            )
            .await
            .expect("interactive run");
        runner
            .run_interactive("bash", &["-l"], &RunOptions::default())
            .await
            .expect("interactive run");

        assert_eq!(runner.calls().len(), 2);
        assert_eq!(runner.subdued_calls(), vec!["sudo apt-get update"]);
    }

    #[tokio::test]
    async fn a_scope_carries_the_subdued_flag_through() {
        // `ScopedRunner::merge` clones the options; a field it forgot would be
        // silently dropped for every task that runs under a scope, which is
        // every task that touches `gh`.
        let inner = Arc::new(FakeRunner::new());
        let scoped = ScopedRunner::new(inner.clone(), vec![("K".into(), "V".into())]);
        scoped
            .run_interactive(
                "gh",
                &["auth", "login"],
                &RunOptions {
                    subdued: Some(Theme::plain()),
                    ..Default::default()
                },
            )
            .await
            .expect("interactive run");

        assert_eq!(inner.subdued_calls(), vec!["gh auth login"]);
    }
}

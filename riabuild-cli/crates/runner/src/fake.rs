//! The scripted `CommandRunner` every unit test in the workspace runs against.
//!
//! Gated on `feature = "testing"` as well as on `cfg(test)`, because a
//! `cfg(test)` double is invisible to every crate but its own: this is
//! compiled into a downstream build whenever that crate turns the feature on.
//!
//! This file is the runner itself and the scripting half of its API — what a
//! test says *before* the code under test runs. [`inspect`] is what it may ask
//! afterwards, [`matching`] is how an invocation finds its stub, [`answer`] is
//! the `CommandRunner` impl, and [`child`] is a spawned child that was never a
//! process.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::output::CommandOutput;

mod answer;
mod child;
mod inspect;
mod matching;

pub use child::FakePipes;
use child::{Ending, FakeChild};

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

/// One recorded invocation: what was run, with what environment, and what was
/// piped to it.
pub struct Recorded {
    pub invocation: String,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    /// Whether the caller asked for the pty. Recorded rather than acted on:
    /// there is no terminal under `cargo test`, so the real path is the plain
    /// inherit either way, and what a test can still assert is the *intent*.
    pub subdued: bool,
}

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
}

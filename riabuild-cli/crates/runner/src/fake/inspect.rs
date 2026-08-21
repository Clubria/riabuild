//! What a test may ask the fake once the code under test has run.
//!
//! Every accessor here answers a question about invocations that already
//! happened. Nothing in this file scripts anything.

use crate::fake::{FakePipes, FakeRunner};

impl FakeRunner {
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

    /// The far end of the `n`th piped child's stdio, taken once.
    ///
    /// A test drives the channel through this: write a request frame into
    /// `to_riabuild`, read the reply out of `from_riabuild`. Taken rather than
    /// borrowed because the two halves usually go to two tasks, the same way
    /// the supervisor splits the real thing.
    pub fn pipes(&self, n: usize) -> Option<FakePipes> {
        let spawned = self.spawned.lock().unwrap();
        spawned.get(n)?.far.lock().unwrap().take()
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
}

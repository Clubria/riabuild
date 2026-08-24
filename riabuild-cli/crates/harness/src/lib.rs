//! One event stream from three agent harnesses that agree on nothing.
//!
//! Claude Code, the Codex CLI and Grok Build all run headless and all emit
//! NDJSON, and there the resemblance stops. They disagree about what a session
//! is, about whether one process serves more than a single turn, and about
//! every field name in between. This crate is the one place that knows, so that
//! `riabuild-agents` can draw a list of running agents without a `match` on
//! which vendor produced each row.
//!
//! # The transport genuinely differs, and that is modelled as data
//!
//! The tempting design is one trait with three implementations that each spawn
//! a long-lived child and talk to it. Two of the three cannot do that:
//!
//! | Harness | Session shape | Why |
//! |---|---|---|
//! | Claude Code | one process, many turns | `--input-format stream-json` reads a JSON user message per turn off stdin and never closes it |
//! | Codex CLI | one process per turn | `codex exec` reads a prompt and exits; continuity is `codex exec resume <thread>` |
//! | Grok Build | one process per turn | `-p/--single` is documented as "single-turn prompt … and exits"; continuity is `--resume <id>` |
//!
//! So [`Restart`] is a field rather than three code paths, and the pump treats a
//! child that exits as the end of a *turn* for two of them and as the end of the
//! *session* for one. Getting this wrong is not a subtle bug: a Codex session
//! modelled as persistent looks like an agent that dies after every reply.
//!
//! Codex and Grok both also speak a persistent protocol — `codex app-server`
//! (JSON-RPC 2.0) and `grok agent stdio` (native ACP) — and either would be a
//! better transport than respawning. Neither is used yet, and the reason is
//! recorded rather than left to be rediscovered: `codex --help` marks
//! `app-server` `[experimental]` and OpenAI's own documentation says the
//! interface changes without notice, and `grok agent stdio` is beta. Both are
//! schema-per-version, so adopting one means generating types against a pinned
//! release — which is worth doing, and is a different change from this one.
//!
//! # Permissions are bypassed on purpose
//!
//! riabuild provisions machines that are already "agents can do anything"
//! environments, and the launchers it writes already say so: `grok_cli` adds
//! `--permission-mode bypassPermissions` and `codex_cli` adds `--yolo` to every
//! launcher on disk. This crate spawns the harnesses *directly* rather than
//! through those launchers — it needs argv the launchers do not pass — so it
//! restates the bypass itself, per harness, in [`Kind::bypass`]. There is no
//! approval round-trip to answer anywhere in this crate, and that absence is
//! the point: the three harnesses each ask permission in a different, badly
//! documented way, and never being asked is what makes one event model possible.
//!
//! # What is verified and what is not
//!
//! Read out of Claude Code 2.1.235, codex-cli 0.148.0 and Grok Build 1.0.5.
//! Claude's stream-json is pinned against a transcript captured from the real
//! binary, in `claude::tests`. Codex's *envelope* — `thread.started`,
//! `turn.started`, `item.completed`, `turn.failed` — is captured from the real
//! binary too, but only its failure path: this machine has no OpenAI or xAI
//! sign-in, so the success-path item bodies and every Grok update shape are
//! written from documentation and are **inferred**. Each is marked at its match
//! arm. That is why every decoder here degrades rather than fails: an unknown
//! `type` becomes nothing at all, never an error, so a schema that moves under
//! us loses detail instead of killing the session.

// `unwrap_used`, `panic` and `expect_used` are denied workspace-wide. In a test
// a panic *is* how a failed precondition is reported, so unwrapping a canned
// transcript is correct — but the exemption is `test` and nothing else.
// Spelling it `any(test, feature = "testing")` would switch the lint off for
// this crate's production code under the one command that enforces it, because
// `cargo clippy --workspace --all-targets` resolves dev-dependencies and
// features unify onto the lib target — and this crate *has* a `testing`
// feature, so that mistake is one character away. See `riabuild-theme`, where
// it was found and the reasoning is written out in full.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use riabuild_runner::{ChildReader, CommandRunner, RunOptions};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

mod claude;
mod codex;
mod grok;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

/// Which vendor's agent this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Claude,
    Codex,
    Grok,
}

/// Whether one child serves the whole session or one turn of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// The child lives for the session. A prompt is a line on its stdin.
    Persistent,
    /// The child exits when the turn does. The next prompt starts another,
    /// resuming the thread the first one opened.
    PerTurn,
}

impl Kind {
    /// Every harness riabuild drives, in the order they are shown.
    ///
    /// A window opens one session per entry: riabuild installs all three, so
    /// asking a developer which ones to enable is a decision riabuild made them
    /// make for no benefit. A per-turn harness costs nothing until it is spoken
    /// to — no process is started — so the two that are idle are three lines on
    /// screen and nothing else.
    pub const ALL: [Kind; 3] = [Kind::Claude, Kind::Codex, Kind::Grok];

    /// The name this harness is known by, for a pane title.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Claude => "Claude Code",
            Kind::Codex => "Codex",
            Kind::Grok => "Grok Build",
        }
    }

    /// The short tag a dense list has room for.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Grok => "grok",
        }
    }

    pub fn restart(self) -> Restart {
        match self {
            Kind::Claude => Restart::Persistent,
            Kind::Codex | Kind::Grok => Restart::PerTurn,
        }
    }

    /// The flags that stop this harness asking anybody anything.
    ///
    /// Three spellings for one idea, none of them interchangeable:
    ///
    /// - Claude Code takes `--permission-mode bypassPermissions`. Its
    ///   `--dangerously-skip-permissions` means the same thing and is *not*
    ///   used, because the two are rejected together and the mode flag is the
    ///   one that also reads correctly in `system/init`'s `permissionMode`,
    ///   which is how the pane shows what it is running under.
    /// - Codex takes `--dangerously-bypass-approvals-and-sandbox`, which is
    ///   both halves at once: `--yolo` — what riabuild's own launchers pass — is
    ///   the interactive spelling, and `codex exec` does not accept it.
    /// - Grok Build takes `--always-approve`. `--permission-mode
    ///   bypassPermissions` is accepted beside it and means the same, but it is
    ///   a **root** option only: after the `-p` this crate always passes it
    ///   would be `unexpected argument`.
    ///
    /// `dontAsk` is not any of these. It reads like the same thing on all three
    /// and silently *denies* whatever was not pre-approved, which presents as an
    /// agent that refuses its own tools.
    fn bypass(self) -> &'static [&'static str] {
        match self {
            Kind::Claude => &["--permission-mode", "bypassPermissions"],
            Kind::Codex => &["--dangerously-bypass-approvals-and-sandbox"],
            Kind::Grok => &["--always-approve"],
        }
    }
}

/// What a session was asked to be, before it exists.
#[derive(Debug, Clone)]
pub struct Launch {
    pub kind: Kind,
    /// The binary. An absolute path from `Ctx::claude()`, `Ctx::codex()` or
    /// `Ctx::grok()` — never a bare name, because during provisioning
    /// `~/.riabuild/bin` is not on `PATH` and a bare name finds whatever the
    /// laptop already had.
    pub program: String,
    /// Where the agent works. Unlike every other riabuild subprocess this is
    /// required rather than optional: an agent with no working directory has
    /// no repository to read, and the whole point of a session is the checkout
    /// it is pointed at.
    pub cwd: String,
    /// The first thing to say. `None` opens the session idle.
    pub prompt: Option<String>,
}

/// A session's handle, unique for this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// Something one agent did, in terms every harness can be read into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The harness has started and named itself. `thread` is the id this
    /// session resumes under, which is the only durable handle any of the three
    /// gives out.
    Ready {
        thread: Option<String>,
        model: Option<String>,
    },
    /// Prose for the developer.
    Said(String),
    /// The model's own reasoning, where the harness emits it separately.
    Thought(String),
    /// A tool call started. `detail` is a one-line rendering of its input —
    /// never the whole of it, which for a file write is the entire file.
    ToolStarted {
        id: String,
        name: String,
        detail: Option<String>,
    },
    /// A tool call finished.
    ToolFinished { id: String, ok: bool },
    /// Token counts, cumulative for the turn.
    Usage { input: u64, output: u64 },
    /// Work attributed to a subagent this session spawned, rather than to the
    /// session itself. Claude Code is the only one of the three that says so,
    /// through `parent_tool_use_id`.
    Delegated { parent: String, inner: Box<Event> },
    /// The turn is over and the agent is waiting for a person.
    Idle,
    /// Something went wrong that did not end the session.
    Trouble(String),
    /// The child is gone. For [`Restart::PerTurn`] this ends a turn, not the
    /// session.
    Exited(i32),
}

/// Turns one harness's lines into [`Event`]s.
///
/// Deliberately synchronous, owning no IO and no process: a decoder is fed the
/// exact bytes a real binary produced and asserted against, which is what makes
/// three undocumented wire formats testable at all.
trait Decoder: Send {
    /// One line of the child's **stdout**.
    ///
    /// Never stderr. Codex writes `tracing` diagnostics there and they are not
    /// JSON — merging the two streams produces a decoder that fails on the
    /// first retry a flaky connection causes.
    ///
    /// A decoder has no way to report the thread id back to the [`Fleet`]
    /// except through [`Event::Ready`], and that is deliberate: the reader runs
    /// in its own task, so any second channel would be shared mutable state
    /// between it and the fleet for a value that already travels in the stream.
    fn read(&mut self, line: &str) -> Vec<Event>;
}

/// Composes the argv and stdin one harness needs.
trait Encoder: Send {
    /// Arguments after the program name, for a session that is starting.
    fn argv(&self, launch: &Launch, thread: Option<&str>, prompt: Option<&str>) -> Vec<String>;

    /// A prompt written to a [`Restart::Persistent`] child's stdin.
    ///
    /// `None` where this harness takes its prompt in argv instead, which is
    /// both of the per-turn ones.
    fn stdin_prompt(&self, _text: &str) -> Option<String> {
        None
    }
}

fn codec(kind: Kind) -> (Box<dyn Encoder>, Box<dyn Decoder>) {
    match kind {
        Kind::Claude => (Box::new(claude::Claude), Box::new(claude::Reader)),
        Kind::Codex => (Box::new(codex::Codex), Box::new(codex::Reader)),
        Kind::Grok => (Box::new(grok::Grok), Box::new(grok::Reader::default())),
    }
}

/// What the fleet knows about one session.
pub struct Session {
    pub id: SessionId,
    pub kind: Kind,
    pub launch: Launch,
    /// The id this session resumes under, learned from the harness.
    pub thread: Option<String>,
    /// Whether a turn is in flight.
    pub busy: bool,
    /// Everything it has said, in order.
    pub transcript: Vec<Event>,
    encoder: Box<dyn Encoder>,
    /// The live child, for a persistent harness. `None` between turns of a
    /// per-turn one, and after an exit.
    child: Option<Box<dyn riabuild_runner::PipedChildHandle>>,
}

impl Session {
    /// Whether this session can be spoken to right now.
    pub fn accepts_prompt(&self) -> bool {
        !self.busy
    }
}

/// Every agent running under one `riabuild agents`.
///
/// Owns the sessions and the single channel their reader tasks report on, so
/// the TUI awaits one receiver rather than N children.
pub struct Fleet {
    runner: Arc<dyn CommandRunner>,
    sessions: Vec<Session>,
    by_id: HashMap<SessionId, usize>,
    next: u64,
    tx: UnboundedSender<(SessionId, Event)>,
    rx: UnboundedReceiver<(SessionId, Event)>,
}

impl Fleet {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            runner,
            sessions: Vec::new(),
            by_id: HashMap::new(),
            next: 1,
            tx,
            rx,
        }
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.by_id
            .get(&id)
            .and_then(|index| self.sessions.get(*index))
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Starts a session and, where a prompt was given, its first turn.
    pub async fn open(&mut self, launch: Launch) -> Result<SessionId> {
        let id = SessionId(self.next);
        self.next += 1;
        let (encoder, _) = codec(launch.kind);
        let index = self.sessions.len();
        self.sessions.push(Session {
            id,
            kind: launch.kind,
            launch: launch.clone(),
            thread: None,
            busy: false,
            transcript: Vec::new(),
            encoder,
            child: None,
        });
        self.by_id.insert(id, index);

        match (launch.kind.restart(), launch.prompt.clone()) {
            (_, Some(prompt)) => self.send(id, &prompt).await?,
            // A persistent harness starts with the window: it introduces itself
            // — which is where the thread id comes from — and is warm by the
            // time the developer has finished typing.
            (Restart::Persistent, None) => self.spawn_turn(index, None).await?,
            // A per-turn one is not started until it is spoken to. `codex exec`
            // and `grok -p` with nothing to answer print nothing and exit, so
            // starting one here would spend a process to produce an immediate
            // `Exited` and an empty pane.
            (Restart::PerTurn, None) => {}
        }
        Ok(id)
    }

    /// Says something to a session, starting a child if it needs one.
    pub async fn send(&mut self, id: SessionId, text: &str) -> Result<()> {
        let index = *self
            .by_id
            .get(&id)
            .with_context(|| format!("no session {}", id.0))?;

        // A persistent child that is already running takes the prompt on its
        // stdin; anything else needs a child started for this turn.
        let existing = {
            let session = &self.sessions[index];
            session.child.is_some() && session.kind.restart() == Restart::Persistent
        };

        if existing {
            let line = {
                let session = &self.sessions[index];
                session.encoder.stdin_prompt(text)
            };
            if let Some(line) = line {
                let session = &mut self.sessions[index];
                let child = session
                    .child
                    .as_ref()
                    .context("a persistent session lost its child")?;
                let mut stdin = child
                    .take_stdin()
                    .context("this session's stdin has already been taken")?;
                use tokio::io::AsyncWriteExt;
                stdin.write_all(line.as_bytes()).await?;
                stdin.flush().await?;
                session.busy = true;
                return Ok(());
            }
        }

        self.spawn_turn(index, Some(text.to_string())).await
    }

    /// Starts the child that will serve one turn (or the whole session).
    async fn spawn_turn(&mut self, index: usize, prompt: Option<String>) -> Result<()> {
        let (program, args, id, kind, cwd) = {
            let session = &self.sessions[index];
            let args = session.encoder.argv(
                &session.launch,
                session.thread.as_deref(),
                prompt.as_deref(),
            );
            (
                session.launch.program.clone(),
                args,
                session.id,
                session.kind,
                session.launch.cwd.clone(),
            )
        };

        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let options = RunOptions {
            cwd: Some(cwd.into()),
            ..RunOptions::default()
        };
        let child = self
            .runner
            .spawn_piped(&program, &borrowed, &options)
            .await
            .with_context(|| format!("could not start {}", kind.label()))?;

        let stdout = child
            .take_stdout()
            .context("the harness was started without a readable stdout")?;

        // A persistent child is kept so the next turn can write to its stdin; a
        // per-turn one is not, because it will have exited by then and holding
        // the handle only keeps a dead pipe open.
        {
            let session = &mut self.sessions[index];
            // Busy only if something was actually asked. A persistent harness
            // is started with the window and then sits waiting on stdin, which
            // is not work — reporting it as work would show every session
            // spinning from the moment the window opened.
            session.busy = prompt.is_some();
            session.child = match kind.restart() {
                Restart::Persistent => Some(child),
                Restart::PerTurn => None,
            };
        }

        let (_, decoder) = codec(kind);
        tokio::spawn(pump(id, stdout, decoder, self.tx.clone()));
        Ok(())
    }

    /// The next thing any session said.
    ///
    /// Records it against that session before handing it back, so a caller that
    /// only draws does not have to maintain the transcript itself.
    pub async fn next_event(&mut self) -> Option<(SessionId, Event)> {
        let (id, event) = self.rx.recv().await?;
        if let Some(index) = self.by_id.get(&id).copied() {
            let session = &mut self.sessions[index];
            match &event {
                // Only a `Ready` that named one: Codex's `thread.started` always
                // does, but nothing obliges the others to, and overwriting a
                // known thread id with `None` would lose the handle this
                // session resumes under.
                Event::Ready {
                    thread: Some(thread),
                    ..
                } => session.thread = Some(thread.clone()),
                Event::Idle => session.busy = false,
                Event::Exited(_) => {
                    session.busy = false;
                    session.child = None;
                }
                _ => {}
            }
            session.transcript.push(event.clone());
        }
        Some((id, event))
    }

    /// Ends every session. A harness left running holds a model connection and,
    /// for the persistent one, a pipe that keeps the process alive after the
    /// TUI has gone.
    pub async fn shutdown(&mut self) {
        for session in &mut self.sessions {
            if let Some(child) = session.child.take() {
                let _ = child.kill().await;
            }
        }
    }
}

/// Reads one child's stdout to EOF, reporting what it said.
///
/// Lines rather than bytes: all three harnesses emit NDJSON, and a partial line
/// is never a whole event. A line that is not JSON at all is dropped — Codex
/// prints a plain `Reading additional input from stdin...` before its first
/// frame, and that is not a failure of anything.
async fn pump(
    id: SessionId,
    stdout: ChildReader,
    mut decoder: Box<dyn Decoder>,
    tx: UnboundedSender<(SessionId, Event)>,
) {
    let mut lines = BufReader::new(stdout).lines();
    // The loop ends on EOF *or* on a read error, and both mean the same thing:
    // this child has nothing more to say. The exit code is not available here —
    // the handle owns `wait` — and a turn that produced a result has already
    // reported it, so the end is reported as clean and the session's own kind
    // decides whether that finished a turn or the whole session.
    while let Ok(Some(line)) = lines.next_line().await {
        for event in decoder.read(&line) {
            if tx.send((id, event)).is_err() {
                return;
            }
        }
    }
    let _ = tx.send((id, Event::Exited(0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_harness_is_started_with_its_own_spelling_of_the_bypass() {
        // Three vendors, three flags, no two interchangeable. If one of these
        // drifts the symptom is not an error — it is an agent that silently
        // stops half way through a turn waiting for an approval nobody can give.
        assert_eq!(
            Kind::Claude.bypass(),
            ["--permission-mode", "bypassPermissions"]
        );
        assert_eq!(
            Kind::Codex.bypass(),
            ["--dangerously-bypass-approvals-and-sandbox"]
        );
        assert_eq!(Kind::Grok.bypass(), ["--always-approve"]);
    }

    #[test]
    fn only_claude_code_serves_more_than_one_turn_from_one_process() {
        // `codex exec` and `grok -p` both print a reply and exit. A fleet that
        // assumed otherwise would show them as agents that die after every
        // answer.
        assert_eq!(Kind::Claude.restart(), Restart::Persistent);
        assert_eq!(Kind::Codex.restart(), Restart::PerTurn);
        assert_eq!(Kind::Grok.restart(), Restart::PerTurn);
    }

    #[test]
    fn the_bypass_never_reaches_for_dont_ask() {
        // `dontAsk` exists on two of the three, reads like "stop asking me", and
        // denies everything not already allowed — an agent that refuses its own
        // tools, which is the opposite of what riabuild wants here.
        for kind in [Kind::Claude, Kind::Codex, Kind::Grok] {
            assert!(
                !kind.bypass().iter().any(|flag| flag.contains("dontAsk")),
                "{kind:?}"
            );
        }
    }
}

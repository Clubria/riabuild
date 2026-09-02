//! One event stream from three agent harnesses that agree on nothing.
//!
//! Claude Code, the Codex CLI and Grok Build all run headless and all emit
//! NDJSON, and there the resemblance stops. They disagree about how a session is
//! resumed, about where a profile's sign-in lives, and about every field name in
//! between. This crate is the one place that knows.
//!
//! It **starts nothing and reads nothing**. Everything here is argv-building and
//! line-decoding — pure, synchronous, and testable against bytes three real
//! binaries produced. Where a turn actually runs, and where its output is
//! written, belongs to `riabuild-agents`; a crate that both knew the wire format
//! and owned the processes would have no way to be tested without them.
//!
//! # One child per turn, for all three
//!
//! Claude Code *can* hold a session open — `--input-format stream-json` reads a
//! user message per turn off stdin and never closes it — and an earlier version
//! of this crate used that. It cannot be used here, and the reason is worth
//! writing down because the capability is real and the temptation to go back to
//! it will recur: a turn has to keep running after the window that started it
//! has closed, and a detached child's stdin has no writer left holding it open.
//! Claude Code reads EOF and exits. So every harness gets one child per turn,
//! resumed by id, and the difference between them collapses to *how you spell
//! resume*:
//!
//! | Harness | Resume |
//! |---|---|
//! | Claude Code | `--resume <uuid>` |
//! | Codex CLI | `exec resume <SESSION_ID> <PROMPT>` |
//! | Grok Build | `--resume <id>` |
//!
//! What that costs is process warmth, not context: `--resume` reloads the
//! conversation from the harness's own store, so nothing is forgotten and the
//! bill is one process start per turn. Verified against Claude Code 2.1.235 —
//! `claude -p --output-format stream-json --verbose --permission-mode
//! bypassPermissions --resume <uuid> "…"` answers in the same `session_id`.
//!
//! # Permissions are bypassed on purpose
//!
//! riabuild provisions machines that are already "agents can do anything"
//! environments, and the launchers it writes already say so: `grok_cli` adds
//! `--permission-mode bypassPermissions` and `codex_cli` adds `--yolo` to every
//! launcher on disk. This crate builds argv the launchers do not pass, so it
//! restates the bypass itself, per harness, in [`Kind::bypass`]. There is no
//! approval round-trip to answer anywhere in this crate, and that absence is
//! the point: the three harnesses each ask permission in a different, badly
//! documented way, and never being asked is what makes one event model possible.
//!
//! # What is verified and what is not
//!
//! Read out of Claude Code 2.1.235, codex-cli 0.148.0 and Grok Build 1.0.5.
//! Claude's stream-json is pinned against transcripts captured from the real
//! binary, in `claude::tests`. Codex's *envelope* — `thread.started`,
//! `turn.started`, `item.completed`, `turn.failed` — is captured from the real
//! binary too, but only its failure path: the machine this was written on had no
//! OpenAI or xAI sign-in, so the success-path item bodies and every Grok update
//! shape are written from documentation and are **inferred**. Each is marked at
//! its match arm. That is why every decoder here degrades rather than fails: an
//! unknown `type` becomes nothing at all, never an error, so a schema that moves
//! under us loses detail instead of killing a session.

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

impl Kind {
    /// Every harness riabuild drives, in the order they are shown.
    ///
    /// A window opens one session per entry: riabuild installs all three, so
    /// asking a developer which ones to enable is a decision riabuild made them
    /// make for no benefit. A session that has not been spoken to has started no
    /// process, so the ones sitting idle are three lines on screen and nothing
    /// else.
    pub const ALL: [Kind; 3] = [Kind::Claude, Kind::Codex, Kind::Grok];

    /// The name this harness is known by, for a pane title.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Claude => "Claude Code",
            Kind::Codex => "Codex",
            Kind::Grok => "Grok Build",
        }
    }

    /// The short tag a dense list has room for, and the one written to disk.
    ///
    /// Both, deliberately: a session record has to survive a riabuild upgrade,
    /// so what it stores must be a string this crate promises to keep reading
    /// rather than a `#[derive(Serialize)]` on an enum whose variants may be
    /// reordered. [`Kind::from_tag`] is the other half.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Grok => "grok",
        }
    }

    /// The inverse of [`Kind::tag`], for reading a record back.
    ///
    /// `None` for anything unrecognised — a record written by a *newer*
    /// riabuild naming a harness this one has never heard of. Skipped rather
    /// than treated as corruption: downgrading should lose a row, not the file.
    pub fn from_tag(tag: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.tag() == tag)
    }

    /// The environment variable that pins which sign-in this harness uses.
    ///
    /// riabuild keeps nine profiles for each of the three, and a session is only
    /// resumable under the profile that created it — each tool stores its
    /// transcripts inside its own home. So this is not a nicety: a turn started
    /// under one home and resumed under another finds no session and silently
    /// begins a new conversation.
    pub fn home_env(self) -> &'static str {
        match self {
            Kind::Claude => "CLAUDE_CONFIG_DIR",
            Kind::Codex => "CODEX_HOME",
            Kind::Grok => "GROK_HOME",
        }
    }

    /// The flags that stop this harness asking anybody anything.
    ///
    /// Three spellings for one idea, none of them interchangeable:
    ///
    /// - Claude Code takes `--permission-mode bypassPermissions`. Its
    ///   `--dangerously-skip-permissions` means the same thing and is *not*
    ///   used, because the two are rejected together and the mode flag is the
    ///   one that also reads back in `system/init`'s `permissionMode`, which is
    ///   how a pane can show what it is running under.
    /// - Codex takes `--dangerously-bypass-approvals-and-sandbox`, which is both
    ///   halves at once: `--yolo` — what riabuild's own launchers pass — is the
    ///   interactive spelling, and `codex exec` does not accept it.
    /// - Grok Build takes `--always-approve`. `--permission-mode
    ///   bypassPermissions` is accepted beside it and means the same, but it is
    ///   a **root** option only: after a subcommand it is `unexpected argument`.
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

    /// The arguments for one turn.
    ///
    /// `thread` is what this harness answered with last time, and its absence is
    /// what makes a turn the *first* one. Nothing else distinguishes them.
    ///
    /// `org_settings` is the team's Claude Code settings file, and it reaches
    /// **Claude Code only**. That is a fact about the other two rather than an
    /// omission here: neither the Codex CLI nor Grok Build reads a file riabuild
    /// brokers on the org's behalf, so there is nothing for the other two arms
    /// to do with it. `None` where the file is not on disk — a machine nothing
    /// has provisioned still runs turns, the same way the account launchers drop
    /// `--settings` rather than naming a file that is not there.
    pub fn argv(
        self,
        thread: Option<&str>,
        prompt: &str,
        org_settings: Option<&str>,
    ) -> Vec<String> {
        match self {
            Kind::Claude => claude::argv(thread, prompt, org_settings),
            Kind::Codex => codex::argv(thread, prompt),
            Kind::Grok => grok::argv(thread, prompt),
        }
    }
}

/// Something one agent did, in terms every harness can be read into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The harness has started and named itself. `thread` is the id this session
    /// resumes under, which is the only durable handle any of the three gives
    /// out — and therefore the one thing a record must capture.
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
}

/// Turns one harness's lines into [`Event`]s.
///
/// Fed the child's **stdout** and never its stderr: Codex writes `tracing`
/// diagnostics there and they are not JSON, so a reader over the merged streams
/// fails on the first retry a flaky connection causes.
///
/// The same type serves live output and history. A spool file replayed through
/// it produces exactly the events the window saw when they were written, which
/// is what makes reopening a session show what happened rather than an
/// approximation of it.
pub struct Reader {
    inner: Box<dyn Decode>,
}

trait Decode: Send {
    fn read(&mut self, line: &str) -> Vec<Event>;
}

impl Reader {
    pub fn new(kind: Kind) -> Self {
        Self {
            inner: match kind {
                Kind::Claude => Box::new(claude::Reader),
                Kind::Codex => Box::new(codex::Reader),
                Kind::Grok => Box::new(grok::Reader::default()),
            },
        }
    }

    /// One line of stdout.
    pub fn read(&mut self, line: &str) -> Vec<Event> {
        self.inner.read(line)
    }
}

/// Every event in a whole spool, for rehydrating a session that was closed.
pub fn replay(kind: Kind, spool: &str) -> Vec<Event> {
    let mut reader = Reader::new(kind);
    spool.lines().flat_map(|line| reader.read(line)).collect()
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
    fn the_bypass_never_reaches_for_dont_ask() {
        // `dontAsk` exists on two of the three, reads like "stop asking me", and
        // denies everything not already allowed — an agent that refuses its own
        // tools, which is the opposite of what riabuild wants here.
        for kind in Kind::ALL {
            assert!(
                !kind.bypass().iter().any(|flag| flag.contains("dontAsk")),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn every_turn_carries_the_bypass_whether_or_not_it_resumes() {
        // The first turn and the twentieth are started by the same code path,
        // and an approval prompt on turn twenty is a session that hangs with
        // nobody able to answer it.
        for kind in Kind::ALL {
            for thread in [None, Some("existing")] {
                for settings in [None, Some("/org.json")] {
                    let argv = kind.argv(thread, "hello", settings);
                    for flag in kind.bypass() {
                        assert!(argv.iter().any(|arg| arg == flag), "{kind:?} {thread:?}");
                    }
                    assert!(argv.iter().any(|arg| arg == "hello"), "{kind:?}");
                }
            }
        }
    }

    /// The team's settings are a Claude Code file, and handing the flag to a
    /// harness that has never heard of it would not be inert — both of the
    /// others would fail to parse their own command line, so every Codex and
    /// Grok turn on a provisioned machine would die at once while every test
    /// that passed `None` went on passing.
    #[test]
    fn only_claude_code_is_given_the_teams_settings_file() {
        for kind in Kind::ALL {
            let argv = kind.argv(Some("existing"), "hello", Some("/org.json"));
            let carried = argv.iter().any(|arg| arg == "--settings");
            assert_eq!(carried, kind == Kind::Claude, "{kind:?}");
        }
    }

    #[test]
    fn a_tag_survives_a_round_trip_and_an_unknown_one_is_not_fatal() {
        // Records outlive the riabuild that wrote them. A downgrade must lose a
        // row rather than refuse to read the file.
        for kind in Kind::ALL {
            assert_eq!(Kind::from_tag(kind.tag()), Some(kind));
        }
        assert_eq!(Kind::from_tag("gemini"), None);
    }

    #[test]
    fn each_harness_pins_its_sign_in_with_its_own_variable() {
        // A turn started under one home and resumed under another finds no
        // session and quietly starts a new conversation — which reads as an
        // agent that forgot everything, with nothing in the terminal saying so.
        assert_eq!(Kind::Claude.home_env(), "CLAUDE_CONFIG_DIR");
        assert_eq!(Kind::Codex.home_env(), "CODEX_HOME");
        assert_eq!(Kind::Grok.home_env(), "GROK_HOME");
    }

    #[test]
    fn a_spool_replays_to_the_events_the_window_saw() {
        // The property the whole persistence design rests on: history and live
        // output go through one decoder, so reopening a session cannot show
        // something different from what was on screen when it happened.
        let events = replay(Kind::Claude, testing::CLAUDE);
        assert!(matches!(events.first(), Some(Event::Ready { .. })));
        assert_eq!(events.last(), Some(&Event::Idle));
    }
}

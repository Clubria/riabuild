//! What is on screen, and how an event changes it.
//!
//! Pure: no terminal, no process, no IO. Everything here is driven by
//! [`Event`]s, which is what lets the whole of the interface be tested against
//! transcripts three real harnesses produced — see `riabuild_harness::testing`.
//! A renderer that owned this state would only be testable by drawing it.

use riabuild_harness::{Event, Kind, SessionId};

/// Where a session has got to.
///
/// Ordered so that the most interesting state sorts first in a list: a
/// developer scanning a column of nine agents is looking for the one that
/// stopped, not the six that are working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// Something went wrong and was reported.
    Trouble,
    /// Waiting for a person: either nothing has been asked yet, or the last
    /// turn is over.
    ///
    /// There is deliberately no `Starting` beside it. Two of the three
    /// harnesses do not start a process until they are spoken to, so "started"
    /// is not a state they can be in — and for the one that does, the gap
    /// between spawning and `system/init` is too short to draw. What a
    /// developer needs to tell apart is *asked and working* from *waiting for
    /// me*, and a third word between them only makes that read slower.
    Idle,
    /// A turn is in flight.
    Busy,
    /// The child has exited and this session takes no more turns.
    Gone,
}

impl State {
    /// The one-glyph mark a dense list has room for.
    ///
    /// Unicode only where the terminal can be trusted with it; the caller
    /// passes what `riabuild-ui` already decided about this terminal, rather
    /// than each widget guessing again.
    pub fn mark(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (State::Trouble, true) => "▲",
            (State::Trouble, false) => "!",
            (State::Idle, true) => "●",
            (State::Idle, false) => "*",
            (State::Busy, true) => "◐",
            (State::Busy, false) => "~",
            (State::Gone, true) => "×",
            (State::Gone, false) => "x",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            State::Trouble => "trouble",
            State::Idle => "idle",
            State::Busy => "working",
            State::Gone => "ended",
        }
    }
}

/// One line of a session's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Said(String),
    Thought(String),
    Tool {
        id: String,
        name: String,
        detail: Option<String>,
        /// `None` while it is still running.
        ok: Option<bool>,
    },
    Trouble(String),
    /// riabuild's own words — a session opening or ending.
    Note(String),
}

/// One session, as the screen understands it.
#[derive(Debug, Clone)]
pub struct Pane {
    pub id: SessionId,
    pub kind: Kind,
    pub cwd: String,
    pub thread: Option<String>,
    pub model: Option<String>,
    pub state: State,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub entries: Vec<Entry>,
    /// Entries produced by a subagent rather than by this session, by index.
    /// Kept beside `entries` rather than inside `Entry` so that every variant
    /// does not carry a flag only two of them ever set.
    pub delegated: Vec<usize>,
}

impl Pane {
    fn new(id: SessionId, kind: Kind, cwd: String) -> Self {
        Self {
            id,
            kind,
            cwd,
            thread: None,
            model: None,
            // Idle rather than anything more hopeful: a window that opens with
            // three sessions has asked none of them anything yet, and every one
            // of them is genuinely waiting for the developer.
            state: State::Idle,
            input_tokens: 0,
            output_tokens: 0,
            entries: Vec::new(),
            delegated: Vec::new(),
        }
    }

    /// The name this session goes by in the list.
    ///
    /// The working directory's last component, because that is what tells two
    /// agents apart when both are Claude Code — the harness name does not.
    pub fn title(&self) -> String {
        let leaf = self
            .cwd
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if leaf.is_empty() {
            self.kind.label().to_string()
        } else {
            leaf.to_string()
        }
    }

    fn push(&mut self, entry: Entry, delegated: bool) {
        if delegated {
            self.delegated.push(self.entries.len());
        }
        self.entries.push(entry);
    }

    /// Applies one event. `delegated` is set when it arrived wrapped in
    /// [`Event::Delegated`].
    fn observe(&mut self, event: &Event, delegated: bool) {
        match event {
            Event::Ready { thread, model } => {
                if thread.is_some() {
                    self.thread = thread.clone();
                }
                if model.is_some() {
                    self.model = model.clone();
                }
                // Deliberately does not touch the state. `Ready` only says the
                // harness introduced itself, which happens both when a session
                // opens with nothing to do and in the middle of a turn that is
                // very much in flight — so reading it as either would be wrong
                // half the time. `App::sent` is what marks a session busy,
                // because sending is the only thing that makes one busy.
            }
            Event::Said(text) => self.push(Entry::Said(text.clone()), delegated),
            Event::Thought(text) => self.push(Entry::Thought(text.clone()), delegated),
            Event::ToolStarted { id, name, detail } => self.push(
                Entry::Tool {
                    id: id.clone(),
                    name: name.clone(),
                    detail: detail.clone(),
                    ok: None,
                },
                delegated,
            ),
            Event::ToolFinished { id, ok } => {
                // Resolve the call this finishes, newest first: a long session
                // reuses tool names constantly and only the id is unique.
                let found = self.entries.iter_mut().rev().find(
                    |entry| matches!(entry, Entry::Tool { id: open, ok: None, .. } if open == id),
                );
                match found {
                    Some(Entry::Tool { ok: slot, .. }) => *slot = Some(*ok),
                    // A result whose call was never seen. Recorded rather than
                    // dropped, because the alternative is a tool that silently
                    // never appears.
                    _ => self.push(
                        Entry::Tool {
                            id: id.clone(),
                            name: "tool".into(),
                            detail: None,
                            ok: Some(*ok),
                        },
                        delegated,
                    ),
                }
            }
            Event::Usage { input, output } => {
                // Cumulative for the turn, so the larger figure wins rather than
                // being added: two `Usage` events in one turn are two reports of
                // the same tokens, and summing them doubles the count.
                self.input_tokens = self.input_tokens.max(*input);
                self.output_tokens = self.output_tokens.max(*output);
            }
            Event::Trouble(text) => {
                self.push(Entry::Trouble(text.clone()), delegated);
                self.state = State::Trouble;
            }
            Event::Idle => {
                // Trouble is not cleared by the turn ending. The developer has
                // not seen it yet, and a session that goes quietly green the
                // instant it fails is the one bug this whole screen exists to
                // stop.
                if self.state != State::Trouble {
                    self.state = State::Idle;
                }
            }
            Event::Exited(code) => {
                if self.kind.restart() == riabuild_harness::Restart::PerTurn {
                    // Expected: these harnesses exit at the end of every turn.
                    // The session is still there and still resumable.
                    if self.state == State::Busy {
                        self.state = State::Idle;
                    }
                } else {
                    self.state = State::Gone;
                    self.push(
                        Entry::Note(format!("the session ended ({code})")),
                        delegated,
                    );
                }
            }
            // Unwrapped by `App::observe` before it gets here.
            Event::Delegated { .. } => {}
        }
    }
}

/// Which half of the screen the keyboard is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Moving between sessions.
    List,
    /// Typing a prompt for the selected one.
    Compose,
}

/// The whole interface.
pub struct App {
    pub panes: Vec<Pane>,
    pub selected: usize,
    pub focus: Focus,
    pub composing: String,
    pub quit: bool,
    /// How far the transcript is scrolled from the bottom, in lines. Zero
    /// follows the newest output.
    pub scrollback: u16,
    /// Advanced on every redraw tick, for the one spinner.
    pub tick: usize,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            panes: Vec::new(),
            selected: 0,
            focus: Focus::List,
            composing: String::new(),
            quit: false,
            scrollback: 0,
            tick: 0,
        }
    }

    pub fn opened(&mut self, id: SessionId, kind: Kind, cwd: String) {
        self.panes.push(Pane::new(id, kind, cwd));
    }

    pub fn selected(&self) -> Option<&Pane> {
        self.panes.get(self.selected)
    }

    /// Records what a session said.
    pub fn observe(&mut self, id: SessionId, event: &Event) {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == id) else {
            return;
        };
        match event {
            Event::Delegated { inner, .. } => pane.observe(inner, true),
            other => pane.observe(other, false),
        }
    }

    /// Marks the selected session busy, for the moment between a developer
    /// pressing Enter and the harness saying anything.
    ///
    /// Without it the pane stays `idle` through the whole of a model's first
    /// think, which reads as a prompt that was not delivered.
    pub fn sent(&mut self, text: &str) {
        if let Some(pane) = self.panes.get_mut(self.selected) {
            pane.push(Entry::Note(format!("› {text}")), false);
            pane.state = State::Busy;
        }
        self.scrollback = 0;
    }

    pub fn select_next(&mut self) {
        if !self.panes.is_empty() {
            self.selected = (self.selected + 1) % self.panes.len();
            self.scrollback = 0;
        }
    }

    pub fn select_previous(&mut self) {
        if !self.panes.is_empty() {
            self.selected = self.selected.checked_sub(1).unwrap_or(self.panes.len() - 1);
            self.scrollback = 0;
        }
    }

    /// How many sessions are working right now, for the header.
    pub fn busy_count(&self) -> usize {
        self.panes
            .iter()
            .filter(|pane| pane.state == State::Busy)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_harness::testing;

    fn play(kind: Kind, transcript: &str) -> App {
        let mut app = App::new();
        let id = SessionId(1);
        app.opened(id, kind, "/work/ai-builders-hub".into());
        for event in testing::decode(kind, transcript) {
            app.observe(id, &event);
        }
        app
    }

    #[test]
    fn a_real_claude_session_renders_as_a_turn_that_finished() {
        let app = play(Kind::Claude, testing::CLAUDE);
        let pane = app.selected().unwrap();
        assert_eq!(pane.state, State::Idle);
        assert_eq!(pane.model.as_deref(), Some("claude-opus-5[1m]"));
        assert!(pane.thread.is_some());
        // The tool call resolved rather than being left open.
        assert!(pane.entries.iter().any(
            |entry| matches!(entry, Entry::Tool { ok: Some(true), name, .. } if name == "Bash")
        ));
        assert!(
            pane.entries
                .iter()
                .any(|entry| matches!(entry, Entry::Said(_)))
        );
        assert_eq!(pane.input_tokens, 28431);
    }

    #[test]
    fn a_session_that_failed_does_not_go_quietly_idle() {
        // Codex's real 401 transcript ends `turn.failed` then idle. The pane
        // must keep saying trouble: an agent that reports green after failing is
        // the failure this screen exists to prevent.
        let app = play(Kind::Codex, testing::CODEX);
        let pane = app.selected().unwrap();
        assert_eq!(pane.state, State::Trouble);
        assert!(
            pane.entries
                .iter()
                .any(|entry| matches!(entry, Entry::Trouble(_)))
        );
    }

    #[test]
    fn grok_reports_that_nobody_is_signed_in() {
        let app = play(Kind::Grok, testing::GROK);
        let pane = app.selected().unwrap();
        assert_eq!(pane.state, State::Trouble);
        let Some(Entry::Trouble(text)) = pane.entries.first() else {
            panic!("expected trouble, got {:?}", pane.entries);
        };
        assert!(text.contains("grok login"), "{text}");
    }

    #[test]
    fn a_per_turn_harness_exiting_ends_a_turn_and_not_the_session() {
        // `codex exec` and `grok -p` exit after every reply. Treating that as
        // the session ending would show every agent as dead the moment it
        // answered.
        for kind in [Kind::Codex, Kind::Grok] {
            let mut app = App::new();
            app.opened(SessionId(1), kind, "/work".into());
            app.observe(SessionId(1), &Event::Exited(0));
            assert_eq!(app.selected().unwrap().state, State::Idle, "{kind:?}");
        }

        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        app.observe(SessionId(1), &Event::Exited(0));
        assert_eq!(app.selected().unwrap().state, State::Gone);
    }

    #[test]
    fn a_tool_result_resolves_the_newest_matching_call() {
        // A long session reuses tool names constantly; only the id is unique,
        // and an older open call with the same id must not steal the result.
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        for event in [
            Event::ToolStarted {
                id: "a".into(),
                name: "Bash".into(),
                detail: None,
            },
            Event::ToolStarted {
                id: "b".into(),
                name: "Bash".into(),
                detail: None,
            },
            Event::ToolFinished {
                id: "b".into(),
                ok: false,
            },
        ] {
            app.observe(SessionId(1), &event);
        }
        let entries = &app.selected().unwrap().entries;
        assert!(matches!(entries[0], Entry::Tool { ok: None, .. }));
        assert!(matches!(
            entries[1],
            Entry::Tool {
                ok: Some(false),
                ..
            }
        ));
    }

    #[test]
    fn a_result_for_a_call_nobody_saw_is_recorded_rather_than_dropped() {
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        app.observe(
            SessionId(1),
            &Event::ToolFinished {
                id: "orphan".into(),
                ok: true,
            },
        );
        assert_eq!(app.selected().unwrap().entries.len(), 1);
    }

    #[test]
    fn a_subagents_work_is_marked_as_its_own() {
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        app.observe(SessionId(1), &Event::Said("mine".into()));
        app.observe(
            SessionId(1),
            &Event::Delegated {
                parent: "toolu_1".into(),
                inner: Box::new(Event::Said("theirs".into())),
            },
        );
        let pane = app.selected().unwrap();
        assert_eq!(pane.entries.len(), 2);
        // Index 1 and not 0: the delegated line is the second one.
        assert_eq!(pane.delegated, vec![1]);
    }

    #[test]
    fn two_usage_reports_in_one_turn_are_not_added_together() {
        // Both Claude and Codex report cumulative counts. Summing them makes a
        // session appear to have spent twice what it did.
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        app.observe(
            SessionId(1),
            &Event::Usage {
                input: 100,
                output: 5,
            },
        );
        app.observe(
            SessionId(1),
            &Event::Usage {
                input: 180,
                output: 9,
            },
        );
        let pane = app.selected().unwrap();
        assert_eq!((pane.input_tokens, pane.output_tokens), (180, 9));
    }

    #[test]
    fn a_pane_is_named_after_its_checkout_not_its_vendor() {
        // Two Claude Code sessions on two repositories are told apart by the
        // repository. The harness name is the same on both.
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work/ai-builders-hub/".into());
        assert_eq!(app.panes[0].title(), "ai-builders-hub");
    }

    #[test]
    fn selection_wraps_in_both_directions_and_never_panics_when_empty() {
        let mut app = App::new();
        // The empty case is the first frame of every run.
        app.select_next();
        app.select_previous();
        assert_eq!(app.selected, 0);

        for id in 1..=3 {
            app.opened(SessionId(id), Kind::Claude, "/work".into());
        }
        app.select_previous();
        assert_eq!(app.selected, 2);
        app.select_next();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn sending_a_prompt_shows_it_and_marks_the_session_working() {
        // Otherwise the pane reads `idle` through the whole of the model's
        // first think, which looks like a prompt that never arrived.
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        app.observe(SessionId(1), &Event::Idle);
        app.sent("do the thing");
        let pane = app.selected().unwrap();
        assert_eq!(pane.state, State::Busy);
        assert_eq!(
            pane.entries.last(),
            Some(&Entry::Note("› do the thing".into()))
        );
    }

    #[test]
    fn trouble_sorts_ahead_of_everything_that_is_merely_working() {
        // A developer scanning nine agents is looking for the one that stopped.
        let mut states = [State::Gone, State::Busy, State::Trouble, State::Idle];
        states.sort();
        assert_eq!(states[0], State::Trouble);
    }
}

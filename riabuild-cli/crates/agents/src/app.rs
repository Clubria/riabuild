//! What is on screen, and how an event changes it.
//!
//! Pure: no terminal, no process, no IO. Everything here is driven by
//! [`Event`]s, which is what lets the whole of the interface be tested against
//! transcripts three real harnesses produced — see `riabuild_harness::testing`.
//! A renderer that owned this state would only be testable by drawing it.
//!
//! History and live output arrive the same way. Reopening a session replays its
//! spool through the same decoder that reads a running turn, so what a pane
//! shows tomorrow is what it showed when the work happened rather than a
//! reconstruction of it.

use riabuild_harness::{Event, Kind};

use crate::account::{Account, Accounts};

/// Where a session has got to.
///
/// Not stored — computed from two facts that are each answerable on their own:
/// whether a turn holds the lock, and whether the last thing that happened was
/// trouble. A `state` field would be a third copy of that, and the copy is what
/// goes stale when a window reopens onto a turn that is already running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// Something went wrong and has not been acted on.
    Trouble,
    /// Waiting for a person.
    Idle,
    /// A turn is in flight.
    Busy,
}

impl State {
    /// The one-glyph mark a dense list has room for.
    ///
    /// Unicode only where the terminal can be trusted with it; the caller passes
    /// what `riabuild-ui` already decided about this terminal, rather than each
    /// widget guessing again.
    pub fn mark(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (State::Trouble, true) => "▲",
            (State::Trouble, false) => "!",
            (State::Idle, true) => "●",
            (State::Idle, false) => "*",
            (State::Busy, true) => "◐",
            (State::Busy, false) => "~",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            State::Trouble => "trouble",
            State::Idle => "idle",
            State::Busy => "working",
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
    /// riabuild's own words — a prompt going out.
    Note(String),
}

/// One session, as the screen understands it.
#[derive(Debug, Clone)]
pub struct Pane {
    /// The store's id. A directory name, stable across windows and reboots.
    pub id: String,
    pub kind: Kind,
    /// Which of that harness's nine sign-ins this session runs under, 1-based:
    /// `claude-2` is `kind` Claude and `account` 2. Read off the record rather
    /// than recomputed, for the reason the home is — a session is only
    /// resumable under the account that made it.
    pub account: usize,
    /// The first prompt, which is what tells two sessions apart. Sessions are
    /// scoped to one checkout, so the directory never could.
    pub title: String,
    pub thread: Option<String>,
    pub model: Option<String>,
    /// Whether a turn holds the session's lock right now.
    pub running: bool,
    /// Sticky until the next prompt. A session that goes quietly green the
    /// instant after it failed is the one bug this screen exists to prevent, so
    /// the turn ending is not what clears this — asking it something else is.
    pub troubled: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub entries: Vec<Entry>,
    /// Entries produced by a subagent rather than by this session, by index.
    /// Kept beside `entries` rather than inside `Entry` so that every variant
    /// does not carry a flag only two of them ever set.
    pub delegated: Vec<usize>,
    /// How far into the spool this pane has read.
    pub offset: u64,
    /// How far into the error log this pane has read.
    ///
    /// A second offset rather than one, because they are two files with two
    /// writers: the harness's own stream, and the wrapper saying what it could
    /// not do. Sharing a counter would have one silently skip the other.
    pub trouble_offset: u64,
}

impl Pane {
    /// A pane on a harness's first account, which is what a window opens
    /// with. A restored session and one started from the chooser say otherwise
    /// by setting [`Pane::account`], the way they already set the thread id.
    pub fn new(id: String, kind: Kind, title: String) -> Self {
        Self {
            id,
            kind,
            account: 1,
            title,
            thread: None,
            model: None,
            running: false,
            troubled: false,
            input_tokens: 0,
            output_tokens: 0,
            entries: Vec::new(),
            delegated: Vec::new(),
            offset: 0,
            trouble_offset: 0,
        }
    }

    pub fn state(&self) -> State {
        if self.running {
            State::Busy
        } else if self.troubled {
            State::Trouble
        } else {
            State::Idle
        }
    }

    /// The sign-in this session runs under, spelled the way its launcher is:
    /// `claude-2`, `grok-1`. What the list shows instead of the bare harness,
    /// because with nine accounts each "claude" no longer identifies anything.
    pub fn account_name(&self) -> String {
        format!("{}-{}", self.kind.tag(), self.account)
    }

    /// The name this session goes by in the list.
    ///
    /// "new session" rather than "new claude" for one nobody has spoken to yet:
    /// the row beside it already says `claude-1`, and a list of `claude-1 new
    /// claude` reads as a stutter.
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            "new session".to_string()
        } else {
            self.title.clone()
        }
    }

    fn push(&mut self, entry: Entry, delegated: bool) {
        if delegated {
            self.delegated.push(self.entries.len());
        }
        self.entries.push(entry);
    }

    /// Applies one event.
    ///
    /// Public because rehydration replays a spool straight into a pane before
    /// the window exists, which is the same operation the live tail performs.
    pub fn observe(&mut self, event: &Event) {
        match event {
            Event::Delegated { inner, .. } => self.apply(inner, true),
            other => self.apply(other, false),
        }
    }

    /// The body, with `delegated` set when the event arrived wrapped in
    /// [`Event::Delegated`].
    fn apply(&mut self, event: &Event, delegated: bool) {
        match event {
            Event::Ready { thread, model } => {
                if thread.is_some() {
                    self.thread = thread.clone();
                }
                if model.is_some() {
                    self.model = model.clone();
                }
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
                //
                // Across turns it is a floor rather than a total, which is the
                // honest thing a per-turn harness can report: each turn starts
                // its own count, and adding them would double every cached read.
                self.input_tokens = self.input_tokens.max(*input);
                self.output_tokens = self.output_tokens.max(*output);
            }
            Event::Trouble(text) => {
                self.push(Entry::Trouble(text.clone()), delegated);
                self.troubled = true;
            }
            // The turn saying it is done. Not what decides whether this pane is
            // busy — the lock does, because a turn can also end by being killed,
            // and nothing is emitted then.
            Event::Idle => {}
            // Unwrapped by `App::observe` before it gets here.
            Event::Delegated { .. } => {}
        }
    }
}

/// Which part of the screen the keyboard is talking to.
///
/// Reading is the resting state, not choosing. A developer spends the whole of
/// a session watching one transcript go by and switches session occasionally,
/// so the arrow keys move *within* what is being read and the session column is
/// somewhere you go — left — rather than what the arrows always mean.
///
/// That is also what makes `PageUp` unnecessary. Scrolling the transcript used
/// to be the one thing only those two keys did, and on a laptop keyboard they
/// are a chord: `Fn` plus an arrow. A screen whose main gesture needs a key half
/// the keyboards in the room do not have is a screen nobody scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Reading the selected session. Up and down scroll it.
    Transcript,
    /// The session column, reached with the left arrow. Up and down pick a
    /// session; right, enter or escape go back to reading it.
    Sessions,
    /// Typing a prompt for the selected session.
    Compose,
    /// Choosing which sign-in a new session runs under.
    Picker,
}

/// The whole interface.
pub struct App {
    pub panes: Vec<Pane>,
    pub selected: usize,
    /// Every sign-in a new session can be started under. Resolved once by the
    /// caller and carried here so the chooser is drawable and testable without
    /// a filesystem.
    pub accounts: Accounts,
    /// Which row the chooser is on, while it is open.
    pub picking: usize,
    pub focus: Focus,
    pub composing: String,
    pub quit: bool,
    /// How far the transcript is scrolled from the bottom, in lines. Zero
    /// follows the newest output.
    pub scrollback: u16,
    /// Advanced on every redraw tick, for the one spinner.
    pub tick: usize,
}

impl App {
    /// A window offering these sign-ins.
    ///
    /// Takes them rather than defaulting to one per harness: an empty list is a
    /// machine with no accounts, which the chooser says out loud, and inventing
    /// a plausible one here would start sessions under a home nobody has.
    pub fn new(accounts: Accounts) -> Self {
        Self {
            panes: Vec::new(),
            selected: 0,
            accounts,
            picking: 0,
            focus: Focus::Transcript,
            composing: String::new(),
            quit: false,
            scrollback: 0,
            tick: 0,
        }
    }

    pub fn add(&mut self, pane: Pane) {
        self.panes.push(pane);
    }

    /// Opens the chooser, on the account the selected session is already
    /// running under.
    ///
    /// Starting there rather than at the top is what makes "another one of
    /// these" a single keypress, which is the thing a developer asks for most:
    /// a second Claude on the same sign-in, beside the one that is busy.
    pub fn open_picker(&mut self) {
        self.picking = self
            .selected()
            .and_then(|pane| self.accounts.position(pane.kind, pane.account))
            .unwrap_or(0);
        self.focus = Focus::Picker;
    }

    /// The account the chooser is on.
    pub fn picked(&self) -> Option<&Account> {
        self.accounts.get(self.picking)
    }

    pub fn pick_next(&mut self) {
        if !self.accounts.is_empty() {
            self.picking = (self.picking + 1) % self.accounts.len();
        }
    }

    pub fn pick_previous(&mut self) {
        if !self.accounts.is_empty() {
            self.picking = self
                .picking
                .checked_sub(1)
                .unwrap_or(self.accounts.len() - 1);
        }
    }

    pub fn selected(&self) -> Option<&Pane> {
        self.panes.get(self.selected)
    }

    pub fn pane_mut(&mut self, id: &str) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.id == id)
    }

    /// Records what a session said.
    pub fn observe(&mut self, id: &str, event: &Event) {
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == id) {
            pane.observe(event);
        }
    }

    /// Whether a turn holds this session's lock.
    pub fn set_running(&mut self, id: &str, running: bool) {
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == id) {
            pane.running = running;
        }
    }

    /// Marks the selected session busy, for the moment between a developer
    /// pressing Enter and the wrapper taking the lock.
    ///
    /// Without it the pane stays idle through the whole of a process start,
    /// which reads as a prompt that was not delivered.
    pub fn sent(&mut self, text: &str) {
        if let Some(pane) = self.panes.get_mut(self.selected) {
            pane.push(Entry::Note(format!("› {text}")), false);
            pane.running = true;
            // A new question is the developer acting on whatever went wrong.
            pane.troubled = false;
            if pane.title.is_empty() {
                pane.title = crate::store::title_of(text);
            }
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
        self.panes.iter().filter(|pane| pane.running).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_harness::testing;

    fn play(kind: Kind, transcript: &str) -> App {
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), kind, "the first prompt".into()));
        for event in testing::decode(kind, transcript) {
            app.observe("s1", &event);
        }
        app
    }

    #[test]
    fn a_real_claude_session_renders_as_a_turn_that_finished() {
        let app = play(Kind::Claude, testing::CLAUDE);
        let pane = app.selected().unwrap();
        assert_eq!(pane.state(), State::Idle);
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
        // Codex's real 401 transcript. The pane must keep saying trouble: an
        // agent that reports green after failing is the failure this screen
        // exists to prevent.
        let app = play(Kind::Codex, testing::CODEX);
        let pane = app.selected().unwrap();
        assert_eq!(pane.state(), State::Trouble);
        assert!(
            pane.entries
                .iter()
                .any(|entry| matches!(entry, Entry::Trouble(_)))
        );
    }

    #[test]
    fn asking_it_something_else_is_what_clears_trouble() {
        // Not the turn ending, which is the harness's opinion. The developer
        // having seen it and moved on is the only thing that means it is dealt
        // with.
        let mut app = play(Kind::Codex, testing::CODEX);
        assert_eq!(app.selected().unwrap().state(), State::Trouble);
        app.sent("try again");
        assert!(!app.selected().unwrap().troubled);
        assert_eq!(app.selected().unwrap().state(), State::Busy);
    }

    #[test]
    fn grok_reports_that_nobody_is_signed_in() {
        let app = play(Kind::Grok, testing::GROK);
        let pane = app.selected().unwrap();
        assert_eq!(pane.state(), State::Trouble);
        let Some(Entry::Trouble(text)) = pane.entries.first() else {
            panic!("expected trouble, got {:?}", pane.entries);
        };
        assert!(text.contains("grok login"), "{text}");
    }

    #[test]
    fn busy_is_the_lock_and_not_the_harnesss_opinion() {
        // A turn can end by being killed, and nothing is emitted then. Deriving
        // busy from `Event::Idle` would leave such a session spinning for ever;
        // deriving it from the lock cannot.
        let mut app = play(Kind::Claude, testing::CLAUDE);
        assert_eq!(app.selected().unwrap().state(), State::Idle);
        app.set_running("s1", true);
        assert_eq!(app.selected().unwrap().state(), State::Busy);
        app.set_running("s1", false);
        assert_eq!(app.selected().unwrap().state(), State::Idle);
    }

    #[test]
    fn a_tool_result_resolves_the_newest_matching_call() {
        // A long session reuses tool names constantly; only the id is unique,
        // and an older open call with the same id must not steal the result.
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
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
            app.observe("s1", &event);
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
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        app.observe(
            "s1",
            &Event::ToolFinished {
                id: "orphan".into(),
                ok: true,
            },
        );
        assert_eq!(app.selected().unwrap().entries.len(), 1);
    }

    #[test]
    fn a_subagents_work_is_marked_as_its_own() {
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        app.observe("s1", &Event::Said("mine".into()));
        app.observe(
            "s1",
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
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        app.observe(
            "s1",
            &Event::Usage {
                input: 100,
                output: 5,
            },
        );
        app.observe(
            "s1",
            &Event::Usage {
                input: 180,
                output: 9,
            },
        );
        let pane = app.selected().unwrap();
        assert_eq!((pane.input_tokens, pane.output_tokens), (180, 9));
    }

    #[test]
    fn a_session_is_named_by_what_it_was_asked() {
        // Every session in the list is in the same checkout, so the directory
        // cannot tell two apart. The first prompt can.
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        assert_eq!(app.selected().unwrap().label(), "new session");
        app.sent("fix the flaky test");
        assert_eq!(app.selected().unwrap().label(), "fix the flaky test");
        // and the title is not rewritten by the second prompt
        app.sent("now ship it");
        assert_eq!(app.selected().unwrap().label(), "fix the flaky test");
    }

    #[test]
    fn selection_wraps_in_both_directions_and_never_panics_when_empty() {
        let mut app = App::new(Accounts::default());
        // The empty case is the first frame of every run.
        app.select_next();
        app.select_previous();
        assert_eq!(app.selected, 0);

        for index in 1..=3 {
            app.add(Pane::new(format!("s{index}"), Kind::Claude, String::new()));
        }
        app.select_previous();
        assert_eq!(app.selected, 2);
        app.select_next();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn sending_a_prompt_shows_it_and_marks_the_session_working() {
        // Otherwise the pane reads idle through the whole of a process start,
        // which looks like a prompt that never arrived.
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        app.sent("do the thing");
        let pane = app.selected().unwrap();
        assert_eq!(pane.state(), State::Busy);
        assert_eq!(
            pane.entries.last(),
            Some(&Entry::Note("› do the thing".into()))
        );
    }

    #[test]
    fn trouble_sorts_ahead_of_everything_that_is_merely_working() {
        // A developer scanning nine agents is looking for the one that stopped.
        let mut states = [State::Busy, State::Trouble, State::Idle];
        states.sort();
        assert_eq!(states[0], State::Trouble);
    }
}

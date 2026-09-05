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
//!
//! # A session and an offer are not the same thing
//!
//! The rail holds both, and telling them apart is the whole of why there is a
//! [`Row`]. A **session** is a directory with a spool, a lock and a
//! conversation in it. An **offer** is a sign-in a new one *could* be started
//! under, and it is nothing else — no directory, no process, nothing to count.
//! The first prompt is what turns one into the other.
//!
//! Conflating the two is what made a window that had been asked nothing report
//! "3 sessions" on its first frame, and it cost more than a wrong number: three
//! directories were created on disk before a developer had typed anything.

use riabuild_harness::{Event, Kind};

use crate::account::{Account, Accounts};
use crate::compose::Compose;

/// What riabuild knows about one sign-in.
///
/// Three states and not two, and the third is the one that matters: **absent**
/// is "nobody has answered yet", which is rendered as nothing. Asking a harness
/// who is signed in costs a subprocess each, so the window opens before any of
/// them have replied — and a missing answer drawn as "signed out" would accuse
/// every account of being logged out for the first second of every run, and
/// would refuse to start a session under one that was fine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signin {
    /// Signed in, as this address.
    In(String),
    /// Signed out. A session started here has nowhere to go.
    Out,
}

/// Whether a harness said, in its own words, that it is not signed in.
///
/// The one thing riabuild can do about an expired OAuth session is name it.
/// Claude Code's wording is the case this exists for — it exits non-zero with
/// `Failed to authenticate: OAuth session expired and could not be refreshed`,
/// which reaches a pane through `errors.log` as one more line of red text among
/// however many the turn produced, indistinguishable from a compile error.
///
/// Matched on the *phrases* rather than the whole sentence, because the whole
/// sentence is a vendor's and changes without notice, and matched
/// case-insensitively for the same reason.
///
/// Every phrase here names authentication and nothing else. `401` and
/// `unauthorized` are deliberately **not** among them: an agent that ran a
/// `curl` against a staging API prints both, and a window that answered a tool
/// result by telling the developer to sign in again would be worse than one
/// that said nothing.
pub fn reads_as_signed_out(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "oauth session expired",
        "failed to authenticate",
        "not logged in",
        "not authenticated",
        "please run /login",
        "authentication_error",
        "invalid api key",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

/// What to tell a developer whose sign-in has nowhere to go.
///
/// One sentence, in one place, because it is said in three: on the rail's
/// splash, in the notice when Enter is refused, and in the transcript when a
/// turn came back saying it. Three wordings of one fact read as three problems.
pub fn signed_out_hint(name: &str) -> String {
    format!("{name} is not signed in \u{2014} run `{name} auth login` in a terminal.")
}

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
    /// The session that delegated this one, by store id, or `None` for one a
    /// developer started themselves.
    ///
    /// The rail draws a pane with this set as a child of the pane above it —
    /// see `draw::rail_lines` — and `store::arrange` is what guarantees the one
    /// above it is the right pane.
    pub parent: Option<String>,
    pub model: Option<String>,
    /// Whether a turn holds the session's lock right now.
    pub running: bool,
    /// Sticky until the next prompt. A session that goes quietly green the
    /// instant after it failed is the one bug this screen exists to prevent, so
    /// the turn ending is not what clears this — asking it something else is.
    pub troubled: bool,
    /// Whether the last failure was this session's harness saying it is not
    /// signed in.
    ///
    /// Sticky like [`Pane::troubled`] and cleared by the same thing — the next
    /// prompt — because a developer who has signed in again finds out by asking
    /// for something, and nothing else riabuild can watch changes in between.
    pub signed_out: bool,
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
            parent: None,
            model: None,
            running: false,
            troubled: false,
            signed_out: false,
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
    /// Only reachable now for a session written by an older riabuild: a session
    /// is created by its first prompt, and that prompt is what titles it.
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            "untitled".to_string()
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
                // Said once, in riabuild's own words, under the vendor's. The
                // harness's sentence stays — it is the evidence — and this is
                // the line that says what to do about it, which no wording of
                // "Failed to authenticate" ever does.
                if reads_as_signed_out(text) && !self.signed_out {
                    self.signed_out = true;
                    let name = self.account_name();
                    self.push(
                        Entry::Trouble(format!(
                            "{name} is not signed in \u{2014} run `{name} auth login` in a \
                             terminal, then send this again."
                        )),
                        false,
                    );
                }
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

/// One row of the rail.
///
/// Sessions first, then offers, which is why both are an index rather than a
/// payload: the order is arithmetic and the rail never has to be rebuilt to be
/// asked what the cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// A session, by index into [`App::panes`].
    Session(usize),
    /// A sign-in a new session could be started under, by index into
    /// [`App::offers`].
    Offer(usize),
}

/// Which part of the screen the keyboard is talking to.
///
/// Two places, not three. Reading a session and writing to it used to be
/// separate, and the cost was a developer pressing Enter twice to say anything:
/// once to reach the session, once to reach its box. There is nothing between
/// the two worth a keypress — the pane has one text field, so being in the pane
/// *is* being in the field, and the arrows keep their meanings around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The rail. Up and down move between sessions and offers; right or enter
    /// go into the one under the cursor.
    List,
    /// A session. Typed characters go into its box, `↑↓` scroll the transcript,
    /// and `←` at the start of the line — or escape — comes back to the rail.
    Session,
    /// Choosing which of the twenty-seven sign-ins a new session runs under.
    Picker,
}

/// The whole interface.
pub struct App {
    pub panes: Vec<Pane>,
    /// The sign-ins the rail offers a new session under.
    ///
    /// One per harness to begin with, and whatever the chooser has added since.
    /// Deliberately not one per account: twenty-seven rows is a list nobody
    /// reads, for a developer using two of them.
    pub offers: Vec<Account>,
    /// Which rail row the cursor is on — sessions first, then offers.
    pub cursor: usize,
    /// Every sign-in a new session can be started under. Resolved once by the
    /// caller and carried here so the chooser is drawable and testable without
    /// a filesystem.
    pub accounts: Accounts,
    /// What riabuild knows about each sign-in, as it learns it.
    ///
    /// Beside [`Account`] rather than inside it: an account is compared by
    /// identity all over this crate, and an email that arrives half a second
    /// after the window opened would make two of the same account unequal.
    ///
    /// A sign-in nobody has answered for is **absent** rather than [`Signin::Out`]
    /// — see the type. That distinction is what keeps the window from refusing
    /// to start anything during the second it takes the probes to come back.
    logins: Vec<(Kind, usize, Signin)>,
    /// Which row the chooser is on, while it is open.
    pub picking: usize,
    pub focus: Focus,
    pub compose: Compose,
    pub quit: bool,
    /// How far the transcript is scrolled from the bottom, in lines. Zero
    /// follows the newest output.
    pub scrollback: u16,
    /// Advanced on every redraw tick, for the one spinner.
    pub tick: usize,
    /// One line the window has to say about the last key, shown where the hints
    /// are and gone on the next keypress.
    ///
    /// It exists because Ctrl-V can succeed at nothing: an empty clipboard, a
    /// laptop with no clipboard tool installed, a `wl-paste` that would not
    /// run. None of those is worth taking the window down for, and all of them
    /// are worse as silence — a key that does nothing and says nothing is
    /// indistinguishable from one that is not bound.
    pub notice: Option<String>,
}

impl App {
    /// A window offering these sign-ins.
    ///
    /// Takes them rather than defaulting to one per harness: an empty list is a
    /// machine with no accounts, which the chooser says out loud, and inventing
    /// a plausible one here would start sessions under a home nobody has.
    pub fn new(accounts: Accounts) -> Self {
        // A harness with no accounts at all still gets an offer, under no home
        // — which is what every session ran under before accounts were offered.
        // Leaving the harness out instead would answer a setup problem by
        // hiding a tool riabuild installed.
        let offers = Kind::ALL
            .into_iter()
            .map(|kind| {
                accounts
                    .first(kind)
                    .cloned()
                    .unwrap_or_else(|| Account::new(kind, 1, None))
            })
            .collect();
        Self {
            panes: Vec::new(),
            offers,
            cursor: 0,
            accounts,
            logins: Vec::new(),
            picking: 0,
            // The rail, because that is where a window with nothing running
            // has something to say. Reading an empty transcript is not a
            // resting state.
            focus: Focus::List,
            compose: Compose::default(),
            quit: false,
            scrollback: 0,
            tick: 0,
            notice: None,
        }
    }

    pub fn add(&mut self, pane: Pane) {
        self.panes.push(pane);
    }

    /// How many rows the rail has.
    pub fn rows(&self) -> usize {
        self.panes.len() + self.offers.len()
    }

    /// What the cursor is on.
    pub fn row(&self) -> Option<Row> {
        match self.cursor.checked_sub(self.panes.len()) {
            Some(offer) if offer < self.offers.len() => Some(Row::Offer(offer)),
            Some(_) => None,
            None => Some(Row::Session(self.cursor)),
        }
    }

    /// The session under the cursor, if it is on one at all.
    pub fn selected(&self) -> Option<&Pane> {
        match self.row()? {
            Row::Session(index) => self.panes.get(index),
            Row::Offer(_) => None,
        }
    }

    /// The sign-in under the cursor, if it is on an offer.
    pub fn offered(&self) -> Option<&Account> {
        match self.row()? {
            Row::Offer(index) => self.offers.get(index),
            Row::Session(_) => None,
        }
    }

    /// Turns the offer under the cursor into a session.
    ///
    /// Called once the store has made the directory, which is what the first
    /// prompt does — nothing here writes anything.
    pub fn begin(&mut self, id: String, account: &Account) {
        let mut pane = Pane::new(id, account.kind, String::new());
        pane.account = account.number;
        self.panes.push(pane);
        self.cursor = self.panes.len() - 1;
        self.scrollback = 0;
    }

    /// Puts a sign-in on the rail and moves the cursor to it.
    ///
    /// What the chooser does. It offers rather than opens: a directory made
    /// before anybody typed anything is the "3 sessions" bug written down.
    pub fn offer(&mut self, account: Account) {
        let at = self
            .offers
            .iter()
            .position(|held| held.kind == account.kind && held.number == account.number)
            .unwrap_or_else(|| {
                self.offers.push(account);
                self.offers.len() - 1
            });
        self.cursor = self.panes.len() + at;
        self.scrollback = 0;
    }

    /// Records what riabuild has learned about a sign-in.
    pub fn set_login(&mut self, kind: Kind, number: usize, signin: Signin) {
        match self
            .logins
            .iter_mut()
            .find(|(held, at, _)| *held == kind && *at == number)
        {
            Some(entry) => entry.2 = signin,
            None => self.logins.push((kind, number, signin)),
        }
    }

    /// What riabuild knows about a sign-in, or `None` where nobody has said
    /// yet — which is rendered as nothing rather than as a claim either way.
    pub fn signin_of(&self, kind: Kind, number: usize) -> Option<&Signin> {
        self.logins
            .iter()
            .find(|(held, at, _)| *held == kind && *at == number)
            .map(|(_, _, signin)| signin)
    }

    /// The address a sign-in belongs to, where riabuild knows one.
    pub fn login_of(&self, kind: Kind, number: usize) -> Option<&str> {
        match self.signin_of(kind, number) {
            Some(Signin::In(email)) => Some(email.as_str()),
            _ => None,
        }
    }

    /// Whether riabuild has been *told* this sign-in is signed out.
    ///
    /// False for an account nobody has answered for, which is the whole reason
    /// [`Signin`] has no `Unknown`: silence is not an accusation.
    pub fn is_signed_out(&self, kind: Kind, number: usize) -> bool {
        matches!(self.signin_of(kind, number), Some(Signin::Out))
    }

    /// The sentence to show instead of starting a session under a sign-in that
    /// has nowhere to go, or `None` where there is nothing in the way.
    ///
    /// Asked by the keymap *before* the box is emptied, so a developer who has
    /// just typed a paragraph into a signed-out account still has it. Only an
    /// offer is refused: a session that already exists has a conversation in it,
    /// and the harness's own answer is a better report than a guess made here.
    pub fn blocked_offer(&self) -> Option<String> {
        let account = self.offered()?;
        self.is_signed_out(account.kind, account.number)
            .then(|| signed_out_hint(&account.name()))
    }

    /// Opens the chooser, on the sign-in the cursor is already on.
    ///
    /// Starting there rather than at the top is what makes "another one of
    /// these" a single keypress, which is the thing a developer asks for most:
    /// a second Claude on the same sign-in, beside the one that is busy.
    pub fn open_picker(&mut self) {
        let at = match self.row() {
            Some(Row::Session(index)) => self
                .panes
                .get(index)
                .and_then(|pane| self.accounts.position(pane.kind, pane.account)),
            Some(Row::Offer(index)) => self
                .offers
                .get(index)
                .and_then(|account| self.accounts.position(account.kind, account.number)),
            None => None,
        };
        self.picking = at.unwrap_or(0);
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

    /// Marks the session under the cursor busy, for the moment between a
    /// developer pressing Enter and the wrapper taking the lock.
    ///
    /// Without it the pane stays idle through the whole of a process start,
    /// which reads as a prompt that was not delivered.
    pub fn sent(&mut self, text: &str) {
        if let Some(Row::Session(index)) = self.row()
            && let Some(pane) = self.panes.get_mut(index)
        {
            pane.push(Entry::Note(format!("› {text}")), false);
            pane.running = true;
            // A new question is the developer acting on whatever went wrong,
            // sign-in included: they cannot have fixed it any other way riabuild
            // could see, so asking again is what re-tests it.
            pane.troubled = false;
            pane.signed_out = false;
            if pane.title.is_empty() {
                pane.title = crate::store::title_of(text);
            }
        }
        self.scrollback = 0;
    }

    pub fn select_next(&mut self) {
        if self.rows() > 0 {
            self.cursor = (self.cursor + 1) % self.rows();
            self.scrollback = 0;
        }
    }

    pub fn select_previous(&mut self) {
        if self.rows() > 0 {
            self.cursor = self.cursor.checked_sub(1).unwrap_or(self.rows() - 1);
            self.scrollback = 0;
        }
    }

    /// Moves the cursor to the offer for a harness, for the digit keys.
    pub fn jump_to_offer(&mut self, kind: Kind) {
        if let Some(at) = self.offers.iter().position(|offer| offer.kind == kind) {
            self.cursor = self.panes.len() + at;
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
    fn an_expired_oauth_session_is_named_rather_than_left_as_red_text() {
        // Claude Code's own wording, verbatim, which reaches a pane through
        // `errors.log` as one more red line among however many the turn
        // produced. Without this it is indistinguishable from a compile error,
        // and the developer spends the afternoon on their code.
        let mut app = play(Kind::Claude, "");
        app.observe(
            "s1",
            &Event::Trouble(
                "Claude Code exited 1: Failed to authenticate: OAuth session expired \
                 and could not be refreshed"
                    .into(),
            ),
        );
        let pane = app.selected().unwrap();
        assert!(pane.signed_out);
        assert_eq!(pane.state(), State::Trouble);

        let said: Vec<&str> = pane
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Trouble(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        // The harness's sentence stays — it is the evidence — and riabuild's is
        // under it, naming the account and the command.
        assert!(
            said.iter()
                .any(|text| text.contains("OAuth session expired"))
        );
        assert!(
            said.iter().any(|text| text.contains("claude-1 auth login")),
            "{said:#?}"
        );

        // Said once, however many more times the harness says it: a turn that
        // retries three times must not stack three copies of riabuild's advice.
        app.observe("s1", &Event::Trouble("Failed to authenticate".into()));
        let again = app.selected().unwrap();
        assert_eq!(
            again
                .entries
                .iter()
                .filter(
                    |entry| matches!(entry, Entry::Trouble(text) if text.contains("auth login"))
                )
                .count(),
            1
        );

        // and asking again is what clears it, because signing in is a thing
        // riabuild cannot watch happen.
        app.cursor = 0;
        app.sent("try again");
        assert!(!app.selected().unwrap().signed_out);
    }

    #[test]
    fn a_tool_result_that_merely_mentions_a_401_is_not_a_sign_in_problem() {
        // The false positive worth refusing: an agent that ran a `curl` against
        // a staging API prints both `401` and `unauthorized`, and a window that
        // answered that by telling the developer to sign in again would be
        // worse than one that said nothing.
        assert!(!reads_as_signed_out("HTTP/2 401 Unauthorized"));
        assert!(!reads_as_signed_out(
            "thread 'main' panicked at src/lib.rs:12"
        ));
        // and the ones that are, however they are cased
        assert!(reads_as_signed_out(
            "Failed to authenticate: OAuth session expired and could not be refreshed"
        ));
        assert!(reads_as_signed_out(
            "Error: Not logged in. Please run /login"
        ));
        assert!(reads_as_signed_out("invalid api key"));
    }

    #[test]
    fn a_sign_in_nobody_has_answered_for_is_neither_in_nor_out() {
        // Three states, and the third is the one the window depends on: the
        // probes take a second and a half to come back, and silence read as
        // "signed out" would accuse every account on the way in.
        let mut app = App::new(Accounts::default());
        assert!(app.signin_of(Kind::Claude, 1).is_none());
        assert!(!app.is_signed_out(Kind::Claude, 1));
        assert_eq!(app.login_of(Kind::Claude, 1), None);

        app.set_login(Kind::Claude, 1, Signin::Out);
        assert!(app.is_signed_out(Kind::Claude, 1));
        // and a signed-out account has no address to show, rather than a stale
        // one from before it expired
        assert_eq!(app.login_of(Kind::Claude, 1), None);

        app.set_login(Kind::Claude, 1, Signin::In("ada@clubria.com".into()));
        assert!(!app.is_signed_out(Kind::Claude, 1));
        assert_eq!(app.login_of(Kind::Claude, 1), Some("ada@clubria.com"));
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
        app.sent("fix the flaky test");
        assert_eq!(app.selected().unwrap().label(), "fix the flaky test");
        // and the title is not rewritten by the second prompt
        app.sent("now ship it");
        assert_eq!(app.selected().unwrap().label(), "fix the flaky test");
    }

    #[test]
    fn a_window_that_has_been_asked_nothing_has_no_sessions_at_all() {
        // The bug in one assertion: three offers and a count of zero. It used
        // to be three panes, three directories on disk, and a header saying
        // "3 sessions" before anybody had typed a word.
        let app = App::new(Accounts::default());
        assert!(app.panes.is_empty());
        assert_eq!(app.offers.len(), 3);
        assert_eq!(app.rows(), 3);
        assert_eq!(app.busy_count(), 0);
        // and the cursor starts on the first of them, so the window opens with
        // something under it rather than on an empty transcript
        assert_eq!(app.row(), Some(Row::Offer(0)));
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn a_prompt_is_what_turns_an_offer_into_a_session() {
        let mut app = App::new(Accounts::default());
        let account = app.offered().cloned().unwrap();
        assert_eq!(account.kind, Kind::Claude);
        app.begin("s1".into(), &account);
        assert_eq!(app.row(), Some(Row::Session(0)));
        // The offer stays: "another Claude" is still one row away.
        assert_eq!(app.offers.len(), 3);
        assert_eq!(app.rows(), 4);
        app.sent("do the thing");
        assert_eq!(app.selected().unwrap().state(), State::Busy);
    }

    #[test]
    fn the_cursor_runs_over_sessions_and_then_offers() {
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        assert_eq!(app.rows(), 4);
        assert_eq!(app.row(), Some(Row::Session(0)));
        app.select_next();
        assert_eq!(app.row(), Some(Row::Offer(0)));
        assert!(app.selected().is_none());
        assert!(app.offered().is_some());
        // and it wraps in both directions
        app.select_previous();
        assert_eq!(app.row(), Some(Row::Session(0)));
        app.select_previous();
        assert_eq!(app.row(), Some(Row::Offer(2)));
    }

    #[test]
    fn choosing_a_sign_in_offers_it_rather_than_opening_it() {
        // Nothing is written until a prompt is typed, so a developer who opens
        // the chooser, picks `claude-4` and changes their mind has left no
        // directory behind.
        let mut app = App::new(Accounts::default());
        app.offer(Account::new(Kind::Claude, 4, None));
        assert!(app.panes.is_empty());
        assert_eq!(app.offers.len(), 4);
        assert_eq!(
            app.offered().map(Account::name).as_deref(),
            Some("claude-4")
        );
        // and choosing it twice does not put it on the rail twice
        app.offer(Account::new(Kind::Claude, 4, None));
        assert_eq!(app.offers.len(), 4);
    }

    #[test]
    fn an_email_arriving_late_does_not_change_what_an_account_is() {
        // `Account` is compared by identity all over this crate. An email
        // stored inside one would make the same sign-in unequal to itself for
        // the half second before `claude auth status` answers.
        let mut app = App::new(Accounts::default());
        assert_eq!(app.login_of(Kind::Claude, 1), None);
        app.set_login(Kind::Claude, 1, Signin::In("ada@clubria.com".into()));
        assert_eq!(app.login_of(Kind::Claude, 1), Some("ada@clubria.com"));
        // Re-signing in replaces rather than appends.
        app.set_login(Kind::Claude, 1, Signin::In("grace@clubria.com".into()));
        assert_eq!(app.login_of(Kind::Claude, 1), Some("grace@clubria.com"));
        assert_eq!(app.login_of(Kind::Claude, 2), None);
        assert_eq!(app.login_of(Kind::Codex, 1), None);
    }

    #[test]
    fn trouble_sorts_ahead_of_everything_that_is_merely_working() {
        // A developer scanning nine agents is looking for the one that stopped.
        let mut states = [State::Busy, State::Trouble, State::Idle];
        states.sort();
        assert_eq!(states[0], State::Trouble);
    }
}

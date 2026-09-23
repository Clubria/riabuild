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

use std::collections::HashMap;

use riabuild_harness::{Event, Kind};

use crate::account::{Account, SignedIn};
use crate::compose::Compose;

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

/// Whether a harness said, in its own words, that an account is out of usage.
///
/// The complaint this exists for: Codex kept reporting itself as "working"
/// through repeated retries after the developer's subscription ran out, with
/// nothing distinguishing that from an ordinary turn in progress — the same
/// blind spot `reads_as_signed_out` closes for an expired sign-in, one
/// category over. Only Claude Code says this in a structured field
/// (`rate_limit_event`, folded into `Event::Trouble` as `"rate limited:
/// …"` by `claude::rate_limit`); Codex and Grok Build have no field for it at
/// all, so all three are read the same way riabuild already reads a sign-in
/// failure — by the vendor's own sentence, matched on phrases rather than
/// whole wording because the wording is the vendor's and moves without
/// notice, and case-insensitively for the same reason.
///
/// None of these phrases names a plain rate-limited HTTP call the way `401` or
/// `unauthorized` would have named an ordinary tool failure for
/// [`reads_as_signed_out`] — each is specific to a quota or a billing plan,
/// which is what keeps a `curl` against someone else's rate-limited API from
/// being read as the developer's own usage running out.
pub fn reads_as_usage_limit(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "rate limit",
        "rate-limit",
        "usage limit",
        "usage cap",
        "quota",
        "429",
        "too many requests",
        "resource_exhausted",
        "resource exhausted",
        "credit balance",
        "out of usage",
        "plan limit",
        "subscription limit",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

/// What to tell a developer whose account has run out of usage.
///
/// One sentence, said once per trouble the same way the signed-out sentence is,
/// under the harness's own wording rather than instead of it.
pub fn usage_limit_hint(name: &str, plan: &str) -> String {
    format!("{name} looks to have hit a usage limit \u{2014} check your {plan} plan.")
}

/// What `turn::one_turn` writes to `errors.log` when a developer's Esc
/// caught up with it.
///
/// Matched whole rather than by phrase, unlike [`reads_as_signed_out`] and
/// [`reads_as_usage_limit`] just above — both of those are guessing at a
/// vendor's sentence, which is free to move without notice. This one is
/// riabuild's own, written by riabuild's own code in exactly this spelling,
/// so nothing a harness says could ever produce it by coincidence and an
/// exact match costs nothing a phrase match would have bought.
pub const INTERRUPTED: &str = "Stopped \u{2014} you pressed Esc.";

/// Whether a trouble line is riabuild saying a developer stopped the turn on
/// purpose, rather than a harness saying something went wrong.
///
/// [`Pane::apply`] uses this to route the line to [`Entry::Note`] instead of
/// [`Entry::Trouble`] and to leave [`Pane::troubled`] false — stopping a turn
/// on purpose is not a failure, and a pane that read as failed after being
/// stopped on purpose would be the bug `troubled`'s stickiness exists to
/// prevent elsewhere, arriving from the opposite direction.
pub fn reads_as_interrupted(text: &str) -> bool {
    text == INTERRUPTED
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
    /// Whether the last trouble was the harness saying this account is out of
    /// usage.
    ///
    /// Sticky like [`Pane::signed_out`] and cleared the same way, and unlike
    /// [`Pane::troubled`] it *does* override [`Pane::state`] while `running` is
    /// still true: this is the one trouble that outlives the turn that raised
    /// it. Codex answers an exhausted subscription with retries rather than
    /// with an ending, so the lock can stay held — and a spinner is a session
    /// that is thinking, not one nobody can hear from until a person notices
    /// the plan is empty.
    pub usage_limited: bool,
    /// Every token the finished turns read and wrote — see [`Pane::tokens`].
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// What the turn in progress has reported so far.
    turn_input: u64,
    turn_output: u64,
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
    /// with. A restored session and one started from an offer say otherwise
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
            usage_limited: false,
            input_tokens: 0,
            output_tokens: 0,
            turn_input: 0,
            turn_output: 0,
            entries: Vec::new(),
            delegated: Vec::new(),
            offset: 0,
            trouble_offset: 0,
        }
    }

    /// Tokens in and out over the whole session, the turn in progress
    /// included.
    ///
    /// A total, and it has to be assembled: every harness reports usage
    /// *cumulative for the turn*, so within a turn the larger figure wins and
    /// across turns they add. Each turn re-reads the conversation, and that
    /// re-read is tokens the session really did spend.
    pub fn tokens(&self) -> (u64, u64) {
        (
            self.input_tokens + self.turn_input,
            self.output_tokens + self.turn_output,
        )
    }

    /// Folds the turn just finished into the session's total.
    ///
    /// Called on both edges of a turn — its `Ready` and its `Idle` — because a
    /// turn that was killed says nothing at its end, and folding twice is
    /// harmless: the second fold adds zero.
    fn close_turn(&mut self) {
        self.input_tokens += std::mem::take(&mut self.turn_input);
        self.output_tokens += std::mem::take(&mut self.turn_output);
    }

    pub fn state(&self) -> State {
        if self.usage_limited {
            State::Trouble
        } else if self.running {
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
                if !delegated {
                    self.close_turn();
                }
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
            // A subagent's count belongs to the subagent's own session.
            Event::Usage { .. } if delegated => {}
            Event::Usage { input, output } => {
                // Cumulative for the turn, so the larger figure wins rather than
                // being added: two `Usage` events in one turn are two reports of
                // the same tokens, and summing them doubles the count. Across
                // turns they add — see `close_turn`.
                self.turn_input = self.turn_input.max(*input);
                self.turn_output = self.turn_output.max(*output);
            }
            Event::Trouble(text) => {
                // Said on purpose, not gone wrong — see `reads_as_interrupted`.
                // Routed here rather than through the ordinary `Trouble` entry
                // so a session a developer stopped on Esc does not read as one
                // that failed.
                if reads_as_interrupted(text) {
                    self.push(Entry::Note(text.clone()), delegated);
                    return;
                }
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
                if reads_as_usage_limit(text) && !self.usage_limited {
                    self.usage_limited = true;
                    let name = self.account_name();
                    self.push(
                        Entry::Trouble(usage_limit_hint(&name, self.kind.short())),
                        false,
                    );
                }
            }
            // The turn saying it is done. Not what decides whether this pane is
            // busy — the lock does, because a turn can also end by being killed,
            // and nothing is emitted then.
            // It does close the turn's token count.
            Event::Idle => {
                if !delegated {
                    self.close_turn();
                }
            }
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

/// Which rail row a half-written prompt belongs to.
///
/// By identity rather than by [`Row`], because a row's index moves whenever a
/// session is created or a subagent arrives, and a draft keyed on an index
/// would turn up under whichever row slid into that position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Draft {
    Session(String),
    Offer(Kind, usize),
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
}

/// The whole interface.
pub struct App {
    pub panes: Vec<Pane>,
    /// The sign-ins the rail offers a new session under: every one riabuild
    /// found signed in when the window opened, and no other.
    ///
    /// Empty until [`App::signed_in`] is called, which is once, when every
    /// harness has answered. A sign-in that is not signed in is not here at
    /// all — offering one would be offering a session with nowhere to go, and
    /// signing it in is the developer's to do outside this window.
    pub offers: Vec<Account>,
    /// Which rail row the cursor is on — sessions first, then offers.
    pub cursor: usize,
    /// Whether riabuild is still finding out which sign-ins are signed in.
    ///
    /// The window opens before the answer does: asking Claude Code costs a
    /// subprocess per account, and a blank terminal while they run reads as a
    /// window that did not start.
    pub checking: bool,
    /// The address each signed-in sign-in belongs to, where one is known.
    ///
    /// Beside [`Account`] rather than inside it, because an account is compared
    /// by identity all over this crate. Kept for sessions as well as offers: a
    /// session's status line names the address of the sign-in it runs under.
    emails: Vec<(Kind, usize, String)>,
    pub focus: Focus,
    /// The box of the row under the cursor.
    ///
    /// Every other row's box is in [`App::drafts`], and moving the cursor swaps
    /// the two — so a prompt half-written to one session stays with it, and is
    /// never sent to whatever the cursor moved on to.
    pub compose: Compose,
    /// The half-written prompts of every row the cursor is *not* on.
    drafts: HashMap<Draft, Compose>,
    /// Why a new session could not be started under a sign-in, shown in the
    /// middle of that offer's pane until a session there starts.
    failures: Vec<(Kind, usize, String)>,
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
    /// A window that has not yet found out which sign-ins are signed in.
    pub fn new() -> Self {
        Self {
            panes: Vec::new(),
            offers: Vec::new(),
            cursor: 0,
            checking: true,
            emails: Vec::new(),
            // The rail, because that is where a window with nothing running
            // has something to say. Reading an empty transcript is not a
            // resting state.
            focus: Focus::List,
            compose: Compose::default(),
            drafts: HashMap::new(),
            failures: Vec::new(),
            quit: false,
            scrollback: 0,
            tick: 0,
            notice: None,
        }
    }

    /// A window offering exactly these sign-ins, for tests.
    #[cfg(test)]
    pub(crate) fn offering(signed_in: Vec<SignedIn>) -> Self {
        let mut app = Self::new();
        app.signed_in(signed_in);
        app
    }

    /// What riabuild found signed in, which becomes NEW SESSION.
    ///
    /// Called once, with every answer at the same time and in the caller's
    /// order, rather than a row at a time as each harness replies — Codex and
    /// Grok Build answer from a file and Claude Code from a subprocess, so rows
    /// arriving one by one would land out of order under a cursor that had
    /// already settled on one of them.
    ///
    /// A cursor on a session stays there, because offers come after sessions. A
    /// cursor on nothing — a window with no sessions — lands on the first offer.
    pub fn signed_in(&mut self, signed_in: Vec<SignedIn>) {
        self.checking = false;
        for SignedIn { account, email } in signed_in {
            if let Some(email) = email {
                self.emails.push((account.kind, account.number, email));
            }
            self.offers.push(account);
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
    ///
    /// The new session goes at the **top**, which is where it will be every
    /// time the window is opened after this: the rail is newest-created first,
    /// and a session that sat at the bottom until the next reopen and then
    /// jumped to the top was a list whose order depended on something nobody
    /// could see.
    pub fn begin(&mut self, id: String, account: &Account) {
        self.stash();
        let mut pane = Pane::new(id, account.kind, String::new());
        pane.account = account.number;
        self.panes.insert(0, pane);
        self.failures
            .retain(|(kind, number, _)| (*kind, *number) != (account.kind, account.number));
        self.cursor = 0;
        self.load();
    }

    /// Takes back a session whose first turn could not be started, and says
    /// why in the middle of the offer it came from.
    ///
    /// A session nothing ran in is not a conversation — leaving it on the rail
    /// would be an empty row saying a session exists — so it goes, the cursor
    /// goes back to the offer, and the prompt goes back in that offer's box so
    /// Enter tries again with nothing retyped.
    pub fn abandon(&mut self, id: &str, account: &Account, why: String, text: &str) {
        self.drafts.remove(&Draft::Session(id.to_string()));
        if let Some(at) = self.panes.iter().position(|pane| pane.id == id) {
            self.panes.remove(at);
        }
        self.failed(account, why, text);
    }

    /// Records that a new session could not be started under `account`, and
    /// puts the cursor back on its offer with the prompt still in the box.
    pub fn failed(&mut self, account: &Account, why: String, text: &str) {
        self.failures
            .retain(|(kind, number, _)| (*kind, *number) != (account.kind, account.number));
        self.failures.push((account.kind, account.number, why));
        if let Some(at) = self
            .offers
            .iter()
            .position(|held| held.kind == account.kind && held.number == account.number)
        {
            // Not `move_to`: the box the cursor is leaving is the one that was
            // just emptied by sending, and it is being given the text back.
            self.cursor = self.panes.len() + at;
            self.compose = Compose::holding(text);
            self.scrollback = 0;
        }
    }

    /// Why the last attempt to start a session under this sign-in failed, if
    /// it did.
    pub fn failure_of(&self, kind: Kind, number: usize) -> Option<&str> {
        self.failures
            .iter()
            .find(|(held, at, _)| *held == kind && *at == number)
            .map(|(_, _, why)| why.as_str())
    }

    /// Which draft the row under the cursor owns.
    fn draft_key(&self) -> Option<Draft> {
        match self.row()? {
            Row::Session(index) => self
                .panes
                .get(index)
                .map(|pane| Draft::Session(pane.id.clone())),
            Row::Offer(index) => self
                .offers
                .get(index)
                .map(|account| Draft::Offer(account.kind, account.number)),
        }
    }

    /// Puts the box away under the row the cursor is on.
    fn stash(&mut self) {
        let compose = std::mem::take(&mut self.compose);
        if let Some(key) = self.draft_key()
            && !compose.text().is_empty()
        {
            self.drafts.insert(key, compose);
        }
    }

    /// Takes out the box of the row the cursor is now on, caret at the end.
    fn load(&mut self) {
        self.compose = self
            .draft_key()
            .and_then(|key| self.drafts.remove(&key))
            .unwrap_or_default();
        self.compose.end();
        self.scrollback = 0;
    }

    /// Moves the cursor to a rail row, taking each row's draft with it.
    ///
    /// The one way the cursor changes rows. It used to be a bare assignment, and
    /// the box stayed where it was while the row changed under it — so `Tab`
    /// from inside a pane quietly retargeted a half-written prompt at another
    /// session, or at an offer that would start a new one.
    pub fn move_to(&mut self, cursor: usize) {
        self.stash();
        self.cursor = cursor;
        self.load();
    }

    /// Goes into the pane under the cursor, caret at the end of its draft.
    pub fn enter(&mut self) {
        self.focus = Focus::Session;
        self.compose.end();
    }

    /// The address a sign-in belongs to, where riabuild knows one.
    pub fn login_of(&self, kind: Kind, number: usize) -> Option<&str> {
        self.emails
            .iter()
            .find(|(held, at, _)| *held == kind && *at == number)
            .map(|(_, _, email)| email.as_str())
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
            pane.usage_limited = false;
            if pane.title.is_empty() {
                pane.title = crate::store::title_of(text);
            }
        }
        self.scrollback = 0;
    }

    pub fn select_next(&mut self) {
        if self.rows() > 0 {
            self.move_to((self.cursor + 1) % self.rows());
        }
    }

    pub fn select_previous(&mut self) {
        if self.rows() > 0 {
            self.move_to(self.cursor.checked_sub(1).unwrap_or(self.rows() - 1));
        }
    }

    /// How many sessions are working right now, for the header.
    pub fn busy_count(&self) -> usize {
        self.panes.iter().filter(|pane| pane.running).count()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::first_of_each;
    use riabuild_harness::testing;

    fn play(kind: Kind, transcript: &str) -> App {
        let mut app = App::new();
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
    fn a_session_out_of_usage_says_so_even_while_it_still_holds_the_lock() {
        // The complaint this exists for: Codex answers an exhausted
        // subscription with retries rather than with an ending, so `running`
        // can stay true for as long as the developer is looking at the
        // screen. A trouble that only overrode the state while idle would
        // never be seen — the pane would just spin.
        let mut app = play(Kind::Codex, "");
        app.set_running("s1", true);
        assert_eq!(app.selected().unwrap().state(), State::Busy);

        app.observe(
            "s1",
            &Event::Trouble("Rate limit reached for your account".into()),
        );
        let pane = app.selected().unwrap();
        assert!(pane.usage_limited);
        // Still busy underneath — the lock is untouched — but shown as
        // trouble, which is the one thing a developer scanning for a stuck
        // session is looking for.
        assert!(pane.running);
        assert_eq!(pane.state(), State::Trouble);

        let said: Vec<&str> = pane
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Trouble(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(said.iter().any(|text| text.contains("Rate limit reached")));
        assert!(
            said.iter().any(|text| text.contains("codex-1")
                && text.contains("usage limit")
                && text.contains("Codex")),
            "{said:#?}"
        );

        // Said once, however many times the harness repeats it — Codex's own
        // comment records that these arrive in bursts.
        app.observe(
            "s1",
            &Event::Trouble("Rate limit reached for your account".into()),
        );
        assert_eq!(
            app.selected()
                .unwrap()
                .entries
                .iter()
                .filter(
                    |entry| matches!(entry, Entry::Trouble(text) if text.contains("usage limit"))
                )
                .count(),
            1
        );

        // and asking again is what clears it, same as a sign-in problem: a
        // usage window resetting is not something riabuild can watch happen.
        app.cursor = 0;
        app.sent("try again");
        assert!(!app.selected().unwrap().usage_limited);
        assert_eq!(app.selected().unwrap().state(), State::Busy);
    }

    #[test]
    fn every_provider_names_its_own_wording_of_out_of_usage() {
        // None of the three have a structured field for this — Claude Code is
        // the only one with anything close, and even that folds into the same
        // `Event::Trouble` text every other harness's plain-text error does.
        for (kind, text) in [
            (Kind::Claude, "rate limited: rejected"),
            (
                Kind::Codex,
                "You've exceeded your current quota, please check your plan",
            ),
            (Kind::Grok, "429 Too Many Requests: usage cap reached"),
        ] {
            let mut app = play(kind, "");
            app.observe("s1", &Event::Trouble(text.into()));
            assert!(app.selected().unwrap().usage_limited, "{kind:?}");
        }
    }

    #[test]
    fn a_turn_a_developer_stopped_reads_as_a_note_rather_than_a_failure() {
        // `turn::one_turn` writes `INTERRUPTED` to `errors.log` the same way it
        // writes any other trouble, so this is exactly the line a harness's own
        // failure would take — and the whole point is that it must not read
        // like one.
        let mut app = play(Kind::Codex, "");
        app.observe("s1", &Event::Trouble(INTERRUPTED.into()));

        let pane = app.selected().unwrap();
        assert!(!pane.troubled);
        assert_eq!(pane.state(), State::Idle);
        assert!(
            pane.entries
                .iter()
                .any(|entry| matches!(entry, Entry::Note(text) if text == INTERRUPTED)),
            "{:#?}",
            pane.entries
        );
        assert!(
            !pane
                .entries
                .iter()
                .any(|entry| matches!(entry, Entry::Trouble(_)))
        );
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
    fn nothing_is_offered_until_riabuild_knows_what_is_signed_in() {
        // The window opens before the answer does. Until then NEW SESSION is
        // empty rather than a guess, and the guess it would have been — every
        // sign-in riabuild keeps — is the list this replaced.
        let mut app = App::new();
        assert!(app.checking);
        assert!(app.offers.is_empty());
        assert_eq!(app.row(), None);

        app.signed_in(vec![
            SignedIn::new(
                Account::new(Kind::Claude, 2, None),
                Some("ada@clubria.com".into()),
            ),
            SignedIn::new(Account::new(Kind::Grok, 1, None), None),
        ]);
        assert!(!app.checking);
        // Exactly what was signed in, one row each, in the order given.
        let names: Vec<String> = app.offers.iter().map(Account::name).collect();
        assert_eq!(names, ["claude-2", "grok-1"]);
        assert_eq!(app.login_of(Kind::Claude, 2), Some("ada@clubria.com"));
        assert_eq!(app.login_of(Kind::Grok, 1), None);
        // and a window with no sessions lands on the first of them
        assert_eq!(app.row(), Some(Row::Offer(0)));
    }

    #[test]
    fn a_machine_with_nothing_signed_in_offers_nothing() {
        let mut app = App::new();
        app.signed_in(Vec::new());
        assert!(!app.checking);
        assert_eq!(app.rows(), 0);
        assert_eq!(app.row(), None);
        assert!(app.offered().is_none());
    }

    #[test]
    fn a_session_keeps_the_cursor_when_the_sign_ins_arrive() {
        // Offers come after sessions, so the answer arriving under a developer
        // who is already reading a session moves nothing.
        let mut app = App::new();
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        app.signed_in(first_of_each());
        assert_eq!(app.row(), Some(Row::Session(0)));
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        assert_eq!(app.selected().unwrap().tokens(), (180, 9));
    }

    #[test]
    fn two_turns_are_added_together() {
        // The figure on the status line is the session's total. It used to be
        // the largest single turn, which is a floor shown as though it were a
        // sum.
        let mut app = App::new();
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        for (input, output) in [(100, 5), (250, 20)] {
            app.observe(
                "s1",
                &Event::Ready {
                    thread: None,
                    model: None,
                },
            );
            app.observe("s1", &Event::Usage { input, output });
            app.observe("s1", &Event::Idle);
        }
        assert_eq!(app.selected().unwrap().tokens(), (350, 25));

        // A turn killed before it said it was done is still counted, once:
        // the next turn's `Ready` closes it.
        app.observe(
            "s1",
            &Event::Usage {
                input: 7,
                output: 1,
            },
        );
        app.observe(
            "s1",
            &Event::Ready {
                thread: None,
                model: None,
            },
        );
        assert_eq!(app.selected().unwrap().tokens(), (357, 26));
    }

    #[test]
    fn a_session_is_named_by_what_it_was_asked() {
        // Every session in the list is in the same checkout, so the directory
        // cannot tell two apart. The first prompt can.
        let mut app = App::new();
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
        let app = App::offering(first_of_each());
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
        let mut app = App::offering(first_of_each());
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
    fn a_new_session_goes_at_the_top_where_it_will_stay() {
        // Newest created first, which is the order a reopened window lists them
        // in — so a session does not sit at the bottom until the next reopen
        // and then jump.
        let mut app = App::new();
        app.add(Pane::new("older".into(), Kind::Claude, "older".into()));
        let account = Account::new(Kind::Codex, 1, None);
        app.begin("newer".into(), &account);
        assert_eq!(app.panes[0].id, "newer");
        assert_eq!(app.row(), Some(Row::Session(0)));
    }

    #[test]
    fn a_session_that_could_not_start_gives_back_the_prompt_and_says_why() {
        let mut app = App::offering(first_of_each());
        let account = app.offered().cloned().unwrap();
        app.begin("s1".into(), &account);
        app.sent("do the thing");
        app.abandon(
            "s1",
            &account,
            "could not start the session:\nno such file".into(),
            "do the thing",
        );
        // No empty session left behind, the cursor back on the offer, and the
        // prompt in its box with the caret at the end, ready to send again.
        assert!(app.panes.is_empty());
        assert_eq!(app.row(), Some(Row::Offer(0)));
        assert_eq!(app.compose.text(), "do the thing");
        assert_eq!(app.compose.caret(), "do the thing".len());
        assert_eq!(
            app.failure_of(account.kind, account.number),
            Some("could not start the session:\nno such file")
        );

        // and a session that does start there is what clears it
        app.begin("s2".into(), &account);
        assert_eq!(app.failure_of(account.kind, account.number), None);
    }

    #[test]
    fn the_cursor_runs_over_sessions_and_then_offers() {
        let mut app = App::offering(first_of_each());
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
    fn an_email_does_not_change_what_an_account_is() {
        // `Account` is compared by identity all over this crate, so the address
        // lives beside it rather than inside it.
        let mut app = App::offering(vec![SignedIn::new(
            Account::new(Kind::Claude, 1, None),
            Some("ada@clubria.com".into()),
        )]);
        assert_eq!(app.offers[0], Account::new(Kind::Claude, 1, None));
        assert_eq!(app.login_of(Kind::Claude, 1), Some("ada@clubria.com"));
        assert_eq!(app.login_of(Kind::Claude, 2), None);
        assert_eq!(app.login_of(Kind::Codex, 1), None);
        app.cursor = 0;
        assert!(app.offered().is_some());
    }

    #[test]
    fn trouble_sorts_ahead_of_everything_that_is_merely_working() {
        // A developer scanning nine agents is looking for the one that stopped.
        let mut states = [State::Busy, State::Trouble, State::Idle];
        states.sort();
        assert_eq!(states[0], State::Trouble);
    }
}

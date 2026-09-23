//! What a session is doing right now, in one of four answers.
//!
//! A turn is **launching** until its harness has said anything at all,
//! **thinking** while it is writing and no tool call is open, and **running a
//! tool** from the moment it issues a call until that call's result arrives.
//! The fourth answer is that no turn is running, which the pane says by saying
//! nothing: an idle session has no indicator, and that absence *is* the state.
//!
//! There is deliberately no fifth. "Waiting for the first reply" used to stand
//! in the transcript of a session with no entries, and it was wrong both ways:
//! said before the process had started, it claimed an agent was working on a
//! prompt that had not reached one; left on a session whose turn had failed, it
//! promised a reply that was never coming.
//!
//! Everything here is derived from two facts the window already has — the
//! events a turn's stream decoded to, and whether a turn holds the lock — plus
//! the prompts this window has queued behind the one in flight. No state is
//! read from a harness that its stream did not say.

use std::path::PathBuf;

use crate::app::{Entry, Pane};

/// How many ticks a prompt that was sent may go without a turn holding the
/// session's lock before the window stops saying it is launching.
///
/// Twenty-five of the window's 120ms ticks, about three seconds. It exists
/// because the window marks a session busy the moment Enter is pressed, and the
/// detached wrapper takes the lock a moment later — a tick that lands in between
/// reads "not running", and without a grace the line would vanish and come back
/// on every prompt. It ends because a wrapper that died before taking the lock
/// and wrote nothing about it would otherwise be launching for ever.
const LAUNCH_GRACE: u8 = 25;

/// How many times a turn may look for a model and an effort its stream does not
/// carry. A few, not one, because the first look can land before the harness
/// has written them down; not unbounded, because a harness that stopped writing
/// them would otherwise be searched for on every tick of every turn.
const LOOKUPS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Progress {
    /// No turn has begun since the last one ended.
    #[default]
    Ended,
    /// A turn was asked for and its harness has said nothing yet.
    Launching,
    /// The harness has said something in this turn.
    Writing,
}

/// Where the current turn has got to.
#[derive(Debug, Clone, Default)]
pub struct Turn {
    progress: Progress,
    /// Prompts this window sent while a turn was already in flight, which the
    /// wrapper runs in order after it.
    queued: usize,
    /// The index into [`Pane::entries`] this turn began at. A tool call left
    /// open by an earlier turn — a result that never arrived — is history, not
    /// something running now.
    start: usize,
    /// Ticks spent launching without the lock held. See [`LAUNCH_GRACE`].
    quiet: u8,
    /// Looks left for this turn's model and effort, for a harness whose stream
    /// does not carry them. See `crate::rollout`.
    pub lookups: u8,
    /// Where that look found them last time, so the next turn does not walk
    /// the directory again.
    pub rollout: Option<PathBuf>,
}

impl Turn {
    fn begin(&mut self, at: usize) {
        self.progress = Progress::Launching;
        self.start = at;
        self.quiet = 0;
        self.lookups = LOOKUPS;
    }

    /// A prompt was sent from this window.
    pub fn asked(&mut self, at: usize) {
        if self.progress == Progress::Ended {
            self.begin(at);
        } else {
            self.queued += 1;
        }
    }

    /// The harness said something.
    pub fn heard(&mut self, at: usize) {
        if self.progress == Progress::Ended {
            // A turn this window did not start — another window's, or one
            // that took the lock between two ticks — is still a turn.
            self.start = at;
            self.lookups = LOOKUPS;
        }
        self.progress = Progress::Writing;
        self.quiet = 0;
    }

    /// The harness said its turn is over.
    pub fn idle(&mut self, at: usize) {
        if self.queued > 0 {
            self.queued -= 1;
            self.begin(at);
        } else {
            self.progress = Progress::Ended;
        }
    }

    /// riabuild's wrapper said the turn could not go on. It writes that only as
    /// the turn ends, so nothing after it belongs to the same turn.
    pub fn stopped(&mut self) {
        self.progress = Progress::Ended;
    }

    /// Whether a turn holds the lock, from one tick to the next.
    fn locked(&mut self, was: bool, now: bool, at: usize) {
        if now {
            self.quiet = 0;
            // Only on the edge: after a turn's last event the wrapper holds the
            // lock a moment longer to write the record down, and a turn that
            // has ended must not read as the next one launching.
            if !was && self.progress == Progress::Ended {
                self.queued = self.queued.saturating_sub(1);
                self.begin(at);
            }
            return;
        }
        match self.progress {
            // Ended without saying so — killed, or a harness that exited
            // mid-stream. The lock is the only witness, and it has spoken.
            Progress::Writing => self.progress = Progress::Ended,
            Progress::Launching => {
                self.quiet = self.quiet.saturating_add(1);
                if self.quiet >= LAUNCH_GRACE {
                    self.progress = Progress::Ended;
                    self.queued = 0;
                }
            }
            Progress::Ended => {}
        }
    }

    /// Whether this turn still wants its model and effort looked up.
    pub fn wants_lookup(&self) -> bool {
        self.progress == Progress::Writing && self.lookups > 0
    }
}

/// What a pane's indicator says. `None` from [`Pane::activity`] is the fourth
/// state: nothing is running, and nothing is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity<'a> {
    /// The turn was started and its harness has not said anything yet.
    Launching,
    /// The harness is writing and no tool call is open.
    Thinking,
    /// A tool call was issued and its result has not arrived. The newest one,
    /// when several are open: that is the one the agent is waiting on.
    Tool {
        name: &'a str,
        detail: Option<&'a str>,
    },
}

impl Pane {
    /// Which of the four states this session is in.
    pub fn activity(&self) -> Option<Activity<'_>> {
        match self.turn.progress {
            Progress::Ended => None,
            // Shown through the grace as well as with the lock held — see
            // `LAUNCH_GRACE`.
            Progress::Launching => Some(Activity::Launching),
            Progress::Writing if !self.running => None,
            Progress::Writing => Some(
                self.entries
                    .get(self.turn.start..)
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .find_map(|entry| match entry {
                        Entry::Tool {
                            name,
                            detail,
                            ok: None,
                            ..
                        } => Some(Activity::Tool {
                            name,
                            detail: detail.as_deref(),
                        }),
                        _ => None,
                    })
                    .unwrap_or(Activity::Thinking),
            ),
        }
    }

    /// Whether a turn holds this session's lock.
    pub fn set_running(&mut self, running: bool) {
        let at = self.entries.len();
        self.turn.locked(self.running, running, at);
        self.running = running;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_harness::{Event, Kind, testing};

    fn pane(kind: Kind) -> Pane {
        Pane::new("s1".into(), kind, "a prompt".into())
    }

    /// What `App::sent` does to a pane, without an `App` around it.
    fn ask(pane: &mut Pane) {
        pane.turn.asked(pane.entries.len());
        pane.running = true;
    }

    fn tool(id: &str, name: &str, detail: &str) -> Event {
        Event::ToolStarted {
            id: id.into(),
            name: name.into(),
            detail: Some(detail.into()),
        }
    }

    #[test]
    fn a_session_nobody_has_asked_anything_shows_nothing() {
        assert_eq!(pane(Kind::Claude).activity(), None);
    }

    #[test]
    fn a_turn_walks_through_launching_thinking_a_tool_and_back_to_nothing() {
        let mut pane = pane(Kind::Claude);
        ask(&mut pane);
        assert_eq!(pane.activity(), Some(Activity::Launching));

        // The wrapper has the lock and the harness is running its hooks, which
        // decode to nothing: still launching.
        pane.set_running(true);
        assert_eq!(pane.activity(), Some(Activity::Launching));

        pane.observe(&Event::Ready {
            thread: Some("t".into()),
            model: Some("claude-opus-5".into()),
            effort: None,
        });
        assert_eq!(pane.activity(), Some(Activity::Thinking));

        pane.observe(&tool("c1", "Bash", "pnpm test login"));
        assert_eq!(
            pane.activity(),
            Some(Activity::Tool {
                name: "Bash",
                detail: Some("pnpm test login"),
            })
        );

        pane.observe(&Event::ToolFinished {
            id: "c1".into(),
            ok: true,
        });
        assert_eq!(pane.activity(), Some(Activity::Thinking));

        pane.observe(&Event::Said("done".into()));
        pane.observe(&Event::Idle);
        // The wrapper still holds the lock while it writes the record down.
        // A turn that has ended must not read as the next one launching.
        pane.set_running(true);
        assert_eq!(pane.activity(), None);
        pane.set_running(false);
        assert_eq!(pane.activity(), None);
    }

    #[test]
    fn a_real_transcript_ends_with_nothing_running() {
        for (kind, transcript) in testing::every_harness() {
            let mut pane = pane(kind);
            ask(&mut pane);
            pane.set_running(true);
            for event in testing::decode(kind, transcript) {
                pane.observe(&event);
            }
            pane.set_running(false);
            assert_eq!(pane.activity(), None, "{kind:?}");
        }
    }

    #[test]
    fn the_newest_open_call_is_the_one_shown() {
        // A Claude `Task` stays open while the subagent it started runs its
        // own tools; the innermost is what the agent is actually waiting on.
        let mut pane = pane(Kind::Claude);
        ask(&mut pane);
        pane.set_running(true);
        pane.observe(&tool("task", "Task", "write a test"));
        pane.observe(&Event::Delegated {
            parent: "task".into(),
            inner: Box::new(tool("inner", "Read", "src/login.ts")),
        });
        assert_eq!(
            pane.activity(),
            Some(Activity::Tool {
                name: "Read",
                detail: Some("src/login.ts"),
            })
        );
        pane.observe(&Event::Delegated {
            parent: "task".into(),
            inner: Box::new(Event::ToolFinished {
                id: "inner".into(),
                ok: true,
            }),
        });
        assert_eq!(
            pane.activity(),
            Some(Activity::Tool {
                name: "Task",
                detail: Some("write a test"),
            })
        );
    }

    #[test]
    fn a_call_an_earlier_turn_left_open_is_not_running_now() {
        let mut pane = pane(Kind::Codex);
        ask(&mut pane);
        pane.set_running(true);
        pane.observe(&tool("old", "shell", "sleep 100"));
        pane.set_running(false);

        ask(&mut pane);
        pane.set_running(true);
        pane.observe(&Event::Said("on it".into()));
        assert_eq!(pane.activity(), Some(Activity::Thinking));
    }

    #[test]
    fn the_moment_before_the_wrapper_takes_the_lock_is_still_launching() {
        let mut pane = pane(Kind::Codex);
        ask(&mut pane);
        // The first tick after Enter often lands before the detached wrapper
        // has the lock. The line must not blink off and back on.
        pane.set_running(false);
        assert_eq!(pane.activity(), Some(Activity::Launching));
        pane.set_running(true);
        assert_eq!(pane.activity(), Some(Activity::Launching));
    }

    #[test]
    fn a_wrapper_that_never_took_the_lock_stops_launching_eventually() {
        let mut pane = pane(Kind::Grok);
        ask(&mut pane);
        for _ in 0..LAUNCH_GRACE {
            pane.set_running(false);
        }
        assert_eq!(pane.activity(), None);
    }

    #[test]
    fn a_turn_killed_mid_stream_shows_nothing() {
        // Nothing is written when a turn is killed; the lock going is the only
        // sign, and it has to be enough.
        let mut pane = pane(Kind::Claude);
        ask(&mut pane);
        pane.set_running(true);
        pane.observe(&tool("c1", "Bash", "cargo build"));
        pane.set_running(false);
        assert_eq!(pane.activity(), None);
    }

    #[test]
    fn a_prompt_queued_behind_a_turn_launches_when_that_turn_ends() {
        let mut pane = pane(Kind::Claude);
        ask(&mut pane);
        pane.set_running(true);
        pane.observe(&Event::Said("working".into()));
        ask(&mut pane);
        // Still the first turn: the second prompt is waiting its turn.
        assert_eq!(pane.activity(), Some(Activity::Thinking));

        pane.observe(&Event::Idle);
        assert_eq!(pane.activity(), Some(Activity::Launching));
        pane.observe(&Event::Said("second".into()));
        pane.observe(&Event::Idle);
        pane.set_running(false);
        assert_eq!(pane.activity(), None);
    }

    #[test]
    fn a_line_no_decoder_understood_still_ends_launching() {
        // Grok's documented frames and Claude's hook notices decode to nothing,
        // and a turn that is writing them is not still starting.
        let mut app = crate::app::App::new();
        app.begin(
            "s1".into(),
            &crate::account::Account::new(Kind::Grok, 1, None),
        );
        app.sent("hello");
        app.set_running("s1", true);
        assert_eq!(app.panes[0].activity(), Some(Activity::Launching));
        app.heard("s1");
        assert_eq!(app.panes[0].activity(), Some(Activity::Thinking));
    }

    #[test]
    fn a_turn_another_window_started_is_seen_launching() {
        let mut pane = pane(Kind::Codex);
        pane.set_running(true);
        assert_eq!(pane.activity(), Some(Activity::Launching));
    }

    #[test]
    fn a_failure_the_wrapper_wrote_ends_the_turn() {
        // `errors.log` is written as the turn ends, so a launch that failed is
        // red text and nothing else — not red text under "launching".
        let mut pane = pane(Kind::Grok);
        ask(&mut pane);
        pane.observe(&Event::Trouble("grok exited 2".into()));
        pane.turn.stopped();
        assert_eq!(pane.activity(), None);
    }
}

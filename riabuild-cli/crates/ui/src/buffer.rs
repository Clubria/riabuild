//! A `Ui` that records what it was asked to print instead of printing it.
//!
//! The engine runs the independent tasks of one dependency wave at the same
//! time, and a terminal has one cursor. Two tasks printing into it interleave
//! their lines, and worse: [`crate::Ui::working`] leaves a status line on the
//! row without a newline for [`crate::Ui::applied`] to cover with `\r`, so a
//! second task printing between the two covers *its* line instead and the
//! first never resolves.
//!
//! So a task that runs concurrently is handed a `Ui` that prints nothing. It
//! records the calls — the calls, not the rendered text — and the engine
//! replays them onto the real `Ui` when that task's turn comes round in
//! declaration order. Replaying calls rather than bytes is what keeps this
//! honest: `--quiet`, the folding to the terminal's width, the `\r` covering
//! and the test recorders are all decided by the real `Ui` at the moment it
//! actually prints, exactly as they were when the run was sequential. A buffer
//! of pre-rendered strings would have had to reproduce every one of those
//! rules, and would have drifted from them.
//!
//! The consequence worth stating out loud: **a buffered `Ui` cannot ask a
//! question.** Nobody is looking at a prompt that has not been printed yet, so
//! [`crate::Ui::buffered`] reports `interactive() == false`, and a task that
//! needs the developer declares `Task::interactive()` and is run against the
//! real `Ui` instead. See `engine::wave`.

use crate::wrap::Detail;

/// Where a `Ui`'s lines go.
pub(crate) enum Sink {
    /// The developer's terminal, immediately.
    Terminal,
    /// A list, until somebody replays it. `std::sync::Mutex` rather than a
    /// `RefCell` because `Ui` is `Sync` and has to stay that way: a `Ctx` is
    /// held across await points in every task.
    Buffer(std::sync::Mutex<Vec<Recorded>>),
}

/// One call to a `Ui` printing method, kept whole so it can be made again.
pub(crate) enum Recorded {
    Banner(String),
    Heading(String),
    Satisfied(String),
    Working(String, String),
    Applied(String),
    Blank,
    Note(String),
    NoteValue(String, String),
    Warn(String),
    Unresolved {
        title: String,
        outcome: String,
        detail: Vec<OwnedDetail>,
    },
    Info(String),
}

/// [`Detail`] with its strings owned, because a recorded call outlives the
/// borrow the caller made it with.
pub(crate) enum OwnedDetail {
    Prose(String),
    Verbatim(String),
}

impl OwnedDetail {
    pub(crate) fn of(detail: &Detail<'_>) -> Self {
        match detail {
            Detail::Prose(text) => Self::Prose((*text).to_string()),
            Detail::Verbatim(text) => Self::Verbatim((*text).to_string()),
        }
    }

    fn borrowed(&self) -> Detail<'_> {
        match self {
            Self::Prose(text) => Detail::Prose(text),
            Self::Verbatim(text) => Detail::Verbatim(text),
        }
    }
}

impl Recorded {
    /// Makes this call again, on a `Ui` that prints.
    ///
    /// Every rule about *how* a line appears lives on the far side of these
    /// methods, which is the whole reason this replays calls: nothing here
    /// decides anything.
    pub(crate) fn replay(self, ui: &crate::Ui) {
        match self {
            Self::Banner(org) => ui.banner(&org),
            Self::Heading(text) => ui.heading(&text),
            Self::Satisfied(title) => ui.satisfied(&title),
            Self::Working(title, reason) => ui.working(&title, &reason),
            Self::Applied(title) => ui.applied(&title),
            Self::Blank => ui.blank(),
            Self::Note(text) => ui.note(&text),
            Self::NoteValue(text, value) => ui.note_value(&text, &value),
            Self::Warn(text) => ui.warn(&text),
            Self::Unresolved {
                title,
                outcome,
                detail,
            } => {
                let borrowed: Vec<Detail<'_>> = detail.iter().map(OwnedDetail::borrowed).collect();
                ui.unresolved(&title, &outcome, &borrowed);
            }
            Self::Info(text) => ui.info(&text),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Ui;

    /// Nothing a buffered `Ui` is told reaches the real one until it is
    /// flushed. Asserted through the recorders, which are filled at the moment
    /// a line is actually printed.
    #[test]
    fn a_buffered_ui_says_nothing_until_it_is_flushed() {
        let real = Ui::new(false);
        let fork = real.buffered();

        fork.note("something happened");
        fork.warn("and something went wrong");
        assert!(real.noted().is_empty(), "{:?}", real.noted());
        assert!(real.warned().is_empty(), "{:?}", real.warned());

        fork.flush_into(&real);
        assert_eq!(real.noted(), vec!["something happened"]);
        assert_eq!(real.warned(), vec!["and something went wrong"]);
    }

    /// In the order the calls were made, across the two streams: a fork's
    /// output is one task's, and a task's lines are a sequence.
    #[test]
    fn a_flush_replays_in_the_order_the_calls_were_made() {
        let real = Ui::new(false);
        let fork = real.buffered();

        for line in ["first", "second", "third"] {
            fork.note(line);
        }
        fork.flush_into(&real);

        assert_eq!(real.noted(), vec!["first", "second", "third"]);
    }

    /// Draining rather than copying: a second flush has nothing left to say.
    /// The engine flushes each fork exactly once, and a fork that replayed
    /// twice would print one task's work under two headings.
    #[test]
    fn a_flush_empties_the_buffer() {
        let real = Ui::new(false);
        let fork = real.buffered();
        fork.note("once");

        fork.flush_into(&real);
        fork.flush_into(&real);

        assert_eq!(real.noted(), vec!["once"]);
    }

    /// The property `Task::interactive` exists to protect: a fork cannot ask,
    /// so it does not claim it can — whatever the terminal underneath says.
    #[test]
    fn a_buffered_ui_never_claims_it_can_ask() {
        let real = Ui::new(false).assume_prompts_work(true);
        assert!(real.interactive());
        assert!(!real.buffered().interactive());
    }

    /// `--quiet` is still decided by the `Ui` that prints, at the moment it
    /// prints — so a quiet run stays quiet through a fork, and a warning still
    /// gets through, exactly as it does without one.
    #[test]
    fn quiet_is_decided_at_replay_and_not_at_record() {
        let real = Ui::new(true);
        let fork = real.buffered();

        fork.note("silenced");
        fork.warn("not silenced");
        fork.flush_into(&real);

        assert!(real.noted().is_empty(), "{:?}", real.noted());
        assert_eq!(real.warned(), vec!["not silenced"]);
    }

    /// A `Ui` that prints has nothing to flush, so the engine does not have to
    /// know which kind it is holding.
    #[test]
    fn flushing_a_terminal_ui_does_nothing() {
        let real = Ui::new(false);
        let other = Ui::new(false);
        real.flush_into(&other);
        assert!(other.noted().is_empty());
    }

    /// The width and the palette come from the run rather than being measured
    /// again — a fork that asked the terminal for itself would lay one run's
    /// output out at two widths across a resize.
    #[test]
    fn a_fork_inherits_the_layout_the_run_measured() {
        let real = Ui::new(false);
        let fork = real.buffered();
        assert_eq!(fork.theme(), real.theme());
        assert_eq!(fork.colour(), real.colour());
    }
}

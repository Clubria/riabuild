//! Terminal output.
//!
//! The error shape is the important part: every failure says what was being
//! attempted in the developer's words, the exact command and its stderr, one
//! concrete next action, and whether re-running is safe. A provisioner that
//! fails vaguely is worse than one that does not run.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct — but the exemption is `test` and nothing else, and must stay that
// way.
//
// It read `any(test, feature = "testing")`, which switched the lint off for
// this crate's *production* code under the one command that enforces it.
// `cargo clippy --workspace --all-targets` resolves dev-dependencies, a
// dev-dependency somewhere in the workspace asks for `testing`, and features
// unify onto the lib target — so the whole crate compiled with the allow on.
// With `test` alone the lib target is linted again, and the unit-test target
// that keeps the allow holds no production code the lib target does not.
//
// Scaffolding behind `feature = "testing"` carries its own allow where it is
// defined, which is a hole the size of a module rather than of a crate.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

mod art;
/// A `Ui` that records instead of printing, so the tasks of one dependency
/// wave can run at the same time and still report one at a time.
mod buffer;
use buffer::{Recorded, Sink};
// Re-exported because "can this terminal draw the block glyphs?" now has a
// second caller — `riabuild agents` picks its marks by the same answer. The
// module stays private: the mark and the banner are this crate's to draw, and
// only the decision underneath them is shared.
pub use art::glyphs_render;
use riabuild_theme::{Role, Theme};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Whether anybody is on the other end of the terminal, and on which stream.
/// The four sites that used to ask `is_terminal()` for themselves — two here,
/// two in `prompt`, one in `status_bar` — all read it from one place now.
pub mod tty;

/// `Ui::ask`, `Ui::confirm`, their must-be-answered counterparts, and the pure
/// rules behind them — the interactive half of this module, split out because
/// it and the status/failure rendering below it are two different concerns
/// that both happened to fit under 300 lines only while one of them didn't
/// exist yet.
mod prompt;

/// Reading one secret with the terminal's echo off, over `/dev/tty` rather
/// than stdin and stdout. Its own file because it shares neither of those
/// with `prompt` above: the caller's stdout is a pipe `ssh` reads a password
/// from, so a prompt written there would *be* the answer.
pub mod secret;

/// One line pinned to a fixed row of a terminal a full-screen program owns —
/// the shape the clipboard channel's supervisor needs, and the one thing `Ui`
/// cannot be: it prints, and printing into a raw-mode terminal somebody else
/// is drawing on is what produced the staircase of ruined newlines this
/// module exists to end.
mod status_bar;
pub use status_bar::StatusBar;

/// Folding riabuild's prose to the terminal's width, and the indents every
/// multi-line message shares. Its own file because the layout rules are pure
/// and worth asserting on their own, and `Ui` is not.
mod wrap;
pub use wrap::Detail;
mod failure;
pub use failure::Failure;
mod report;
mod words;
pub use words::{duration_words, plural};

/// Text riabuild did not write, on its way to a terminal — a repository's
/// description from GitHub, a shared server's from riabuild-web. Its own file
/// because it is a boundary rather than a formatter: what it removes is the
/// escape sequences that would let somebody else's sentence redraw the box it
/// is printed in.
mod foreign;
pub use foreign::one_line;

pub struct Ui {
    /// The Clubria palette, bound to what this terminal can render. Every
    /// colour riabuild prints comes from here, so there is one place to change
    /// the scheme and no way for a call site to invent its own.
    theme: Theme,
    /// The terminal, or a list of calls waiting to be replayed onto it.
    ///
    /// Every printing method below reads this *first*, ahead of `quiet` and
    /// ahead of the status-line bookkeeping, so a buffered `Ui` reaches none of
    /// it: what `quiet` silences and what covers what are decided once, by the
    /// real `Ui`, at replay. See `buffer`.
    sink: Sink,
    quiet: bool,
    /// Whether there is a developer on the other end to answer a question.
    ///
    /// riabuild's own prompts are not the only thing this gates. `gh auth
    /// login --web` runs a device-code flow: it prints a code and waits for a
    /// human to finish in a browser. Handed no terminal it does not fail — it
    /// waits, silently, forever. Every command riabuild hands the terminal to
    /// has to read the same flag riabuild's own prompts do, or the two answers
    /// disagree and one of them is wrong.
    interactive: bool,
    /// Columns of a status line left on screen without a newline, so whatever
    /// replaces it can cover the whole thing. Zero means nothing is pending.
    pending: AtomicUsize,
    /// Columns riabuild folds its own prose to, measured once at startup.
    ///
    /// Once, rather than per message, because a window resized mid-run would
    /// otherwise leave a block laid out at two widths — and the measurement is
    /// a syscall on a path that prints a line at a time.
    width: usize,
    /// Blank lines printed by [`Ui::blank`], for the tests that pin the spacing
    /// around a handoff. Behind the `testing` feature like the recorders below
    /// it, because the spacing it pins is decided in `riabuild-cli` and
    /// `riabuild-remote` — a bare `cfg(test)` here is off in every crate that
    /// has anything to assert.
    #[cfg(any(test, feature = "testing"))]
    blanks: AtomicUsize,
    /// Answers a test feeds to `ask` in place of a terminal.
    #[cfg(any(test, feature = "testing"))]
    answers: std::sync::Mutex<std::collections::VecDeque<String>>,
    /// Every question actually put to the developer.
    ///
    /// Recorded because the question text is the whole safety guarantee of a
    /// destructive prompt, and it is the one part of it `info` cannot carry:
    /// `info` is silent under `--quiet` and `ask` is not. Without this a
    /// subcommand could name the account in lines nobody sees and still pass.
    #[cfg(any(test, feature = "testing"))]
    asked: std::sync::Mutex<Vec<String>>,
    /// Every warning actually printed.
    ///
    /// Recorded for the same reason as `noted`, and for one more: a warning is
    /// what riabuild says *instead of* stopping. A step downgraded from a
    /// failure to a warning still returns `Ok`, so without this a test could
    /// only assert that nothing went wrong — which is equally true of a step
    /// that silently did nothing and told the developer nothing either.
    #[cfg(any(test, feature = "testing"))]
    warned: std::sync::Mutex<Vec<String>>,
    /// Every note actually printed.
    ///
    /// Recorded for the same reason as `asked`, one step further along: a note
    /// is a claim about the machine — "Signed out you@example.com" — and a claim
    /// printed whether or not the thing happened is the failure mode worth
    /// pinning. Filled after the `--quiet` return, so this says the developer
    /// saw it rather than that someone called `note`.
    #[cfg(any(test, feature = "testing"))]
    noted: std::sync::Mutex<Vec<String>>,
}

/// Locks one of the recorders above — or a buffered `Ui`'s list of pending
/// calls — and never panics doing it.
///
/// The recorders are `testing`-gated but they are *not* test code: they are
/// compiled into the lib target of every crate that turns the feature on, so
/// `unwrap_used` applies to them like any other production line. The buffer in
/// `Sink::Buffer` is not gated at all. A poisoned mutex here means a task
/// panicked while holding one — the run has already failed, and a
/// `PoisonError` raised on top of it would replace the real message with one
/// about locking. `into_inner` takes the data anyway, which is safe for a list
/// of things riabuild printed: there is no invariant a half-finished `push`
/// could have broken.
pub(crate) fn recorded<T>(cell: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Default for Ui {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Ui {
    pub fn new(quiet: bool) -> Self {
        Self {
            theme: Theme::detect(tty::can_paint()),
            sink: Sink::Terminal,
            interactive: tty::attended(),
            quiet,
            pending: AtomicUsize::new(0),
            width: wrap::wrap_width(wrap::terminal_columns()),
            #[cfg(any(test, feature = "testing"))]
            blanks: AtomicUsize::new(0),
            #[cfg(any(test, feature = "testing"))]
            answers: Default::default(),
            #[cfg(any(test, feature = "testing"))]
            asked: Default::default(),
            #[cfg(any(test, feature = "testing"))]
            warned: Default::default(),
            #[cfg(any(test, feature = "testing"))]
            noted: Default::default(),
        }
    }

    /// Every question this `Ui` put, in order.
    #[cfg(any(test, feature = "testing"))]
    pub fn asked(&self) -> Vec<String> {
        recorded(&self.asked).clone()
    }

    /// Every note this `Ui` printed, in order.
    #[cfg(any(test, feature = "testing"))]
    pub fn noted(&self) -> Vec<String> {
        recorded(&self.noted).clone()
    }

    /// Every warning this `Ui` printed, in order.
    #[cfg(any(test, feature = "testing"))]
    pub fn warned(&self) -> Vec<String> {
        recorded(&self.warned).clone()
    }

    /// A `Ui` that answers its own questions, for tests.
    #[cfg(any(test, feature = "testing"))]
    pub fn scripted<'a>(answers: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            interactive: true,
            answers: std::sync::Mutex::new(answers.into_iter().map(ToString::to_string).collect()),
            ..Self::new(false)
        }
    }

    /// Overrides terminal detection.
    ///
    /// Tests model a developer sitting at a terminal, but `cargo test` hands
    /// them no tty — and `interactive` is hard-false under `cfg!(test)` anyway
    /// — so without this every test would silently take the unattended path and
    /// stop covering the interactive one. Test-only on purpose: production must
    /// read the real terminal, never be told.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn assume_prompts_work(mut self, yes: bool) -> Self {
        self.interactive = yes;
        self
    }

    /// A `Ui` for one task of a concurrent wave: it records, and prints
    /// nothing until [`Ui::flush_into`] replays it.
    ///
    /// `theme` and `width` are *copied* rather than re-detected. Both are
    /// measured once at startup on purpose — a window resized mid-run would
    /// otherwise leave one run's output laid out at two widths — and a fork
    /// that measured for itself would be exactly that bug, arriving once per
    /// task instead of once per resize.
    ///
    /// `interactive` is false whatever the real terminal says, because it is
    /// the honest answer: a question this `Ui` asked would be recorded rather
    /// than printed, and the developer would be answering a prompt they had
    /// not been shown. A task that needs them declares `Task::interactive()`
    /// and never reaches this.
    #[must_use]
    pub fn buffered(&self) -> Ui {
        Ui {
            theme: self.theme,
            sink: Sink::Buffer(std::sync::Mutex::new(Vec::new())),
            quiet: self.quiet,
            interactive: false,
            width: self.width,
            // The rest is a fresh `Ui`'s: the status-line counter and, under
            // `testing`, the recorders. Every field this fork actually carries
            // is named above it.
            ..Self::new(self.quiet)
        }
    }

    /// Replays everything this `Ui` recorded onto `target`, in order, and
    /// empties it.
    ///
    /// A no-op on a `Ui` that prints, so the caller does not have to know which
    /// kind it holds.
    pub fn flush_into(&self, target: &Ui) {
        let Sink::Buffer(lines) = &self.sink else {
            return;
        };
        for line in std::mem::take(&mut *recorded(lines)) {
            line.replay(target);
        }
    }

    /// Records `line` if this `Ui` buffers, and answers whether it did.
    ///
    /// Every printing method calls this before doing anything else, and returns
    /// on `true`.
    fn record(&self, line: Recorded) -> bool {
        match &self.sink {
            Sink::Terminal => false,
            Sink::Buffer(lines) => {
                recorded(lines).push(line);
                true
            }
        }
    }

    /// Claims the pending status line, so it is only covered once.
    fn take_pending(&self) -> usize {
        self.pending.swap(0, Ordering::Relaxed)
    }

    /// Ends a status line left on screen, so the next thing printed cannot land
    /// on the end of it.
    ///
    /// [`Ui::applied`] and [`Ui::unresolved`] do not need this: they *replace*
    /// that line, and cover it. This is for everything that prints past a task
    /// without resolving it — and a warning raised from inside one is the case
    /// that made it necessary, because it is written to stderr and so cannot
    /// carry the `\r` that covers stdout. Left out, the run rendered as
    /// `◐ Authorised — installing the key  ▲ riabuild's key is already…`.
    fn end_status_line(&self) {
        if self.take_pending() > 0 {
            println!();
            let _ = std::io::stdout().flush();
        }
    }

    /// Whether this terminal gets colour.
    ///
    /// Exposed because the environment shell's banner and prompt are printed by
    /// a generated rcfile rather than by `Ui`, and that file has to bake in the
    /// same `NO_COLOR` decision this made.
    pub fn colour(&self) -> bool {
        self.theme.enabled()
    }

    /// The palette this terminal gets.
    ///
    /// Exposed for the same reason as [`Ui::colour`]: the environment shell's
    /// banner and the accounts box are rendered into a generated rcfile rather
    /// than printed by `Ui`, and they have to bake in this terminal's depth —
    /// not merely whether colour is on at all.
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Whether there is a developer on the other end to answer a question.
    ///
    /// Exposed so a caller can tell "nobody to ask" apart from "asked, and they
    /// chose the default" — `ask` returns `None` for both. A destructive
    /// subcommand needs that distinction: an empty answer at a real prompt is a
    /// deliberate no, while no terminal at all means the choice was never
    /// offered and should be refused rather than silently taken either way.
    ///
    /// Read it before delegating to anything that waits on a person, too — `gh
    /// auth login --web` and the rest. A `false` here is not a reason to prompt
    /// more quietly: it means the answer can never arrive, so the only honest
    /// move is to say so and stop.
    pub fn interactive(&self) -> bool {
        self.interactive
    }

    fn paint(&self, role: Role, text: &str) -> String {
        self.theme.paint(role, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_one_answer_to_whether_a_person_is_here() {
        // Two flags — one for riabuild's own prompts, one for commands riabuild
        // hands the terminal to — is the bug this collapse fixes: they
        // disagreed, and the disagreement compiled. `assume_prompts_work` is
        // the only thing that may move it, and only in a test.
        assert!(!Ui::new(false).interactive());
        assert!(Ui::new(false).assume_prompts_work(true).interactive());
        assert!(!Ui::new(false).assume_prompts_work(false).interactive());
        // `scripted` models a developer answering, so it is interactive too.
        assert!(Ui::scripted(["y"]).interactive());
    }
}

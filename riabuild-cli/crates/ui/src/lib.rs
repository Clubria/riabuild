//! Terminal output.
//!
//! The error shape is the important part: every failure says what was being
//! attempted in the developer's words, the exact command and its stderr, one
//! concrete next action, and whether re-running is safe. A provisioner that
//! fails vaguely is worse than one that does not run.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct. The `feature = "testing"` half matters as much as the `test` half:
// when a downstream crate turns the feature on, this crate is compiled as a
// dependency and `cfg(test)` is false, so the exemption would not apply.
#![cfg_attr(any(test, feature = "testing"), allow(clippy::unwrap_used))]

mod art;
use riabuild_theme::{Role, Theme};
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// Folding riabuild's prose to the terminal's width, and the indents every
/// multi-line message shares. Its own file because the layout rules are pure
/// and worth asserting on their own, and `Ui` is not.
mod wrap;
pub use wrap::Detail;

pub struct Ui {
    /// The Clubria palette, bound to what this terminal can render. Every
    /// colour riabuild prints comes from here, so there is one place to change
    /// the scheme and no way for a call site to invent its own.
    theme: Theme,
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

/// Spaces needed for `line` to cover a status line `previous` columns wide.
fn cover(previous: usize, line: &str) -> usize {
    previous.saturating_sub(line.chars().count())
}

/// A note whose tail is the part the developer has to act on, painted as two
/// spans rather than one.
///
/// The two cannot be nested. `Theme::paint` closes with `\x1b[0m`, which resets
/// every attribute rather than the one it opened, so a `Strong` value formatted
/// *into* a `Muted` line would end the dim at the value and leave everything
/// after it undimmed. Painting the prose and the value separately is what makes
/// the emphasis local to the value.
///
/// Pure, and taking its `Theme`, so a test can assert what each rung of the
/// ladder receives without owning a terminal — the same reason `depth_for` is
/// split out of `Theme::detect`.
fn value_line(theme: Theme, text: &str, value: &str) -> String {
    format!(
        "    {} {}",
        theme.paint(Role::Muted, text),
        theme.paint(Role::Strong, value)
    )
}

/// `1 commit`, `2 commits`.
///
/// Regular English `-s` only, which covers every noun riabuild counts. Worth a
/// function because `commit(s)` is exactly the sort of detail that makes a
/// tool read as unfinished, and it had spread to four separate messages.
pub fn plural(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("{count} {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// A count of minutes as something a person can judge at a glance.
///
/// A brokered credential lasts around a month, and "43199 more minute(s)" is a
/// number nobody can convert in their head — it reads as an error rather than
/// as "this is fine for weeks". Zero components are dropped so short durations
/// stay short.
pub fn duration_words(minutes: u64) -> String {
    if minutes == 0 {
        return "less than a minute".to_string();
    }
    let parts = [
        (minutes / (60 * 24), "day"),
        ((minutes / 60) % 24, "hour"),
        (minutes % 60, "minute"),
    ];
    parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, unit)| plural(*count, unit))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Default for Ui {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Ui {
    pub fn new(quiet: bool) -> Self {
        Self {
            theme: Theme::detect(std::io::stdout().is_terminal()),
            // Both halves matter. A piped stdin with a terminal stdout is the
            // shape a CI job has, and a question asked there blocks until
            // something times out. `cfg!(test)` is the same hazard indoors: a
            // test run inherits the terminal `cargo test` was started from.
            interactive: !cfg!(any(test, feature = "testing"))
                && std::io::stdin().is_terminal()
                && std::io::stdout().is_terminal(),
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
        self.asked.lock().unwrap().clone()
    }

    /// Every note this `Ui` printed, in order.
    #[cfg(any(test, feature = "testing"))]
    pub fn noted(&self) -> Vec<String> {
        self.noted.lock().unwrap().clone()
    }

    /// Every warning this `Ui` printed, in order.
    #[cfg(any(test, feature = "testing"))]
    pub fn warned(&self) -> Vec<String> {
        self.warned.lock().unwrap().clone()
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

    /// The mark, the wordmark, and what this invocation is about to work on.
    pub fn banner(&self, org: &str) {
        if self.quiet {
            return;
        }
        println!();
        for line in art::banner(
            self.theme,
            art::glyphs_render(),
            org,
            riabuild_version::VERSION,
        ) {
            println!("{line}");
        }
        println!();
    }

    pub fn heading(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("\n{}", self.paint(Role::Strong, text));
    }

    /// A task that needed nothing.
    pub fn satisfied(&self, title: &str) {
        if self.quiet {
            return;
        }
        println!(
            "  {} {}",
            self.paint(Role::Ok, "●"),
            self.paint(Role::Muted, title)
        );
    }

    /// A task about to run, with the reason it is running.
    pub fn working(&self, title: &str, reason: &str) {
        if self.quiet {
            return;
        }
        // Measured without the colour escapes, which occupy no columns.
        self.pending.store(
            format!("  ◐ {title} — {reason}").chars().count(),
            Ordering::Relaxed,
        );
        print!(
            "  {} {} {}",
            self.paint(Role::Busy, "◐"),
            title,
            self.paint(Role::Muted, &format!("— {reason}")),
        );
        let _ = std::io::stdout().flush();
    }

    pub fn applied(&self, title: &str) {
        if self.quiet {
            return;
        }
        // This line is written over the status line, which is longer: it also
        // carried the reason the task ran. Padding has to cover whatever was
        // there, not a fixed guess — a fixed ten spaces left the tail of
        // "— first run" behind, so finished tasks read "● GitHub CLI    un".
        let line = format!("  ● {title}");
        let padding = " ".repeat(cover(self.take_pending(), &line));
        println!("\r  {} {}{}", self.paint(Role::Ok, "●"), title, padding);
    }

    /// One deliberate blank line, at a handoff.
    ///
    /// Spacing across a handoff is nobody's by default, and that is the bug it
    /// exists to fix: `ssh` prints `Connection to … closed.` the moment the
    /// remote command ends, `mosh` prints `[mosh is exiting.]` when it lets go,
    /// and the environment shell prints an accounts box the second it starts.
    /// None of them know what riabuild printed a line earlier, so the rule is
    /// riabuild's to keep: **one blank line before handing the terminal to a
    /// child that prints its own lines, and one after taking it back**.
    ///
    /// The other half of the rule is that whatever runs on the far side prints
    /// none of its own — see `provision::open_shell`, where a laptop separates
    /// its own shell and a server does not, because the laptop that opened the
    /// connection already did.
    ///
    /// Counted rather than recorded in tests: what a spacing regression looks
    /// like is a blank line missing or doubled, never one with the wrong text
    /// in it.
    pub fn blank(&self) {
        if self.quiet {
            return;
        }
        // A blank line ends the status line, exactly as a note does.
        self.take_pending();
        #[cfg(any(test, feature = "testing"))]
        self.blanks.fetch_add(1, Ordering::Relaxed);
        println!();
    }

    /// How many blank lines this `Ui` printed.
    #[cfg(any(test, feature = "testing"))]
    pub fn blanks(&self) -> usize {
        self.blanks.load(Ordering::Relaxed)
    }

    pub fn note(&self, text: &str) {
        if self.quiet {
            return;
        }
        // A note is written on the end of the status line and ends it, so
        // there is nothing left for `applied` to cover.
        self.take_pending();
        #[cfg(any(test, feature = "testing"))]
        self.noted.lock().unwrap().push(text.to_string());
        // Recorded whole and printed folded: a test asserting what the
        // developer was told should not have to know where the terminal
        // happened to break the sentence.
        for line in wrap::fold(text, self.width.saturating_sub(wrap::INDENT.len())) {
            println!("{}{}", wrap::INDENT, self.paint(Role::Muted, &line));
        }
    }

    /// A note ending in something the developer has to read off the screen and
    /// type somewhere else: a device code, a one-time value.
    ///
    /// The prose stays `Muted` like every other note; the value does not. A
    /// device code is transcribed by hand into a browser on another machine,
    /// and printing it dim makes the one line that has to be legible the least
    /// legible thing on the screen — under `Signing … in to riabuild` there are
    /// three dim lines and no way to tell which one is the work.
    ///
    /// `Strong` rather than a hue: this is emphasis, not a status. `Brand` and
    /// `Danger` share `1;31` on a sixteen-colour terminal, so a brand-coloured
    /// code would read as an error message on exactly the terminals — an old
    /// server over SSH — where this flow is the entire interface.
    pub fn note_value(&self, text: &str, value: &str) {
        if self.quiet {
            return;
        }
        // A note ends the status line; see `note`.
        self.take_pending();
        #[cfg(any(test, feature = "testing"))]
        self.noted.lock().unwrap().push(format!("{text} {value}"));
        println!("{}", value_line(self.theme, text, value));
    }

    pub fn warn(&self, text: &str) {
        // Deliberately not gated on `quiet`, and on stderr: a warning is what
        // riabuild says in place of stopping, so it is the one line a run
        // asked to be silent still has to produce.
        #[cfg(any(test, feature = "testing"))]
        self.warned.lock().unwrap().push(text.to_string());
        self.end_status_line();
        for (index, line) in wrap::fold(text, self.width.saturating_sub(wrap::INDENT.len()))
            .iter()
            .enumerate()
        {
            if index == 0 {
                eprintln!("  {} {line}", self.paint(Role::Warn, "▲"));
            } else {
                // Under the first word, never under the mark: a hanging indent
                // is what keeps the block reading as one warning.
                eprintln!("{}{line}", wrap::INDENT);
            }
        }
    }

    /// A task that could not be finished, and that did not stop the run.
    ///
    /// The `▲` counterpart of [`Ui::applied`]. It covers the busy line the same
    /// way, so the task resolves instead of sitting at `◐` for the rest of the
    /// run, and it carries the outcome where the reason for running was. The
    /// explanation follows beneath it, folded and dimmed — one mark for the
    /// whole block, because a second `▲` under the first says nothing the first
    /// did not.
    ///
    /// Two streams, on purpose. The mark and the outcome belong to the task
    /// ladder, which is on stdout and is what the busy line has to be covered
    /// on; the explanation is a warning and joins the rest of them on stderr. A
    /// run asked to be quiet printed no ladder to cover, so there it goes to
    /// stderr with the explanation — a warning is the one thing `--quiet` does
    /// not silence.
    pub fn unresolved(&self, title: &str, outcome: &str, detail: &[Detail]) {
        // Recorded as one warning, not as a title and some lines: a test
        // asserting that a downgraded path told the developer what happened
        // should not have to know how the block was split up to print it.
        #[cfg(any(test, feature = "testing"))]
        self.warned.lock().unwrap().push(
            std::iter::once(format!("{title} — {outcome}"))
                .chain(detail.iter().map(|line| line.text().to_string()))
                .collect::<Vec<_>>()
                .join(" "),
        );
        // Measured without the colour escapes, exactly as `applied` does.
        let plain = format!("  ▲ {title} — {outcome}");
        let padding = " ".repeat(cover(self.take_pending(), &plain));
        let painted = format!(
            "  {} {} {}",
            self.paint(Role::Warn, "▲"),
            title,
            self.paint(Role::Muted, &format!("— {outcome}"))
        );
        if self.quiet {
            eprintln!("{painted}");
        } else {
            println!("\r{painted}{padding}");
            let _ = std::io::stdout().flush();
        }
        for line in wrap::detail_lines(self.theme, self.width, detail) {
            eprintln!("{line}");
        }
    }

    pub fn info(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("{text}");
    }

    /// The four things every failure must say.
    pub fn failure(&self, failure: &Failure) {
        self.end_status_line();
        eprintln!();
        eprintln!(
            "  {} {}",
            self.paint(Role::Danger, "riabuild stopped:"),
            failure.attempting
        );
        if let Some(command) = &failure.command {
            eprintln!("    {} {}", self.paint(Role::Muted, "ran"), command);
        }
        let body = self.width.saturating_sub(wrap::INDENT.len());
        for line in failure
            .detail
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(8)
            .flat_map(|line| wrap::fold(line, body))
        {
            eprintln!("{}{}", wrap::INDENT, self.paint(Role::Muted, &line));
        }
        // The label is folded *with* the sentence rather than printed in front
        // of it, so the first line is measured including the nine columns it
        // occupies. An action is the longest thing a failure carries — the
        // remedy for a stale host key names a file, a host and two commands —
        // and it is the line the developer has to act on.
        let mut paragraphs = failure.action.split('\n');
        let opening = paragraphs.next().unwrap_or_default().trim();
        for line in wrap::fold(&format!("do this: {opening}"), body) {
            match line.strip_prefix("do this:") {
                Some(rest) => eprintln!(
                    "{}{}{rest}",
                    wrap::INDENT,
                    self.paint(Role::Strong, "do this:")
                ),
                None => eprintln!("{}{line}", wrap::INDENT),
            }
        }
        // Anything past the first paragraph is a line to copy — the public key
        // in `authorise`'s paste-it-by-hand remedy is the only one today — and
        // gets the same treatment as a warning's. That is what a `\n` in an
        // action means, and the only thing it means: `Failure` is a plain
        // struct built at a hundred call sites, so the alternative is asking
        // each of them to classify a paragraph none of them has.
        let rest: Vec<Detail> = paragraphs.map(Detail::Verbatim).collect();
        for line in wrap::detail_lines(self.theme, self.width, &rest) {
            eprintln!("{line}");
        }
        eprintln!(
            "    {}",
            self.paint(
                Role::Muted,
                if failure.safe_to_rerun {
                    "running `riabuild` again is safe once that is done"
                } else {
                    "do not re-run riabuild until that is done"
                },
            )
        );
        eprintln!();
    }
}

/// A failure a developer can act on.
#[derive(Debug, Clone)]
pub struct Failure {
    /// What riabuild was trying to do, in the developer's words.
    pub attempting: String,
    /// The exact command that failed, if there was one.
    pub command: Option<String>,
    /// stderr, or whatever else explains it.
    pub detail: String,
    /// One concrete next action.
    pub action: String,
    pub safe_to_rerun: bool,
}

impl Failure {
    pub fn new(attempting: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            attempting: attempting.into(),
            command: None,
            detail: String::new(),
            action: action.into(),
            safe_to_rerun: true,
        }
    }

    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.attempting, self.action)
    }
}

impl std::error::Error for Failure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_task_covers_the_status_line_it_replaces() {
        // "  ◐ GitHub CLI — first run" is 26 columns and "  ● GitHub CLI" is
        // 14. The ten fixed spaces this used to print reached column 24 and
        // left the last two behind, so a satisfied task rendered as
        // "● GitHub CLI          un".
        assert_eq!("  ◐ GitHub CLI — first run".chars().count(), 26);
        assert!(cover(26, "  ● GitHub CLI") > 10);
        assert_eq!(cover(26, "  ● GitHub CLI"), 12);
    }

    #[test]
    fn a_longer_finished_line_is_not_padded_backwards() {
        assert_eq!(cover(8, "  ● Node and pnpm"), 0);
    }

    #[test]
    fn a_note_ends_the_status_line_so_nothing_is_left_to_cover() {
        let ui = Ui::new(false);
        ui.working("Infisical CLI", "first run");
        ui.note("Installing infisical with Homebrew…");
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn a_device_code_is_not_dimmed_along_with_the_words_in_front_of_it() {
        use riabuild_theme::Depth;
        // The dim run closes before the code opens. Written the obvious way —
        // a `Strong` value formatted into a `Muted` line — the inner reset
        // would end the dim at the code and leave the rest of the line
        // undimmed, emphasising everything except the thing to emphasise.
        let line = value_line(
            Theme::with_depth(Depth::TrueColor),
            "Enter code",
            "DHNT-ZSDM",
        );
        assert_eq!(line, "    \x1b[2mEnter code\x1b[0m \x1b[1mDHNT-ZSDM\x1b[0m");
    }

    #[test]
    fn a_highlighted_value_still_reads_as_a_sentence_without_colour() {
        // NO_COLOR, a pipe, a CI log: the emphasis is gone and the line has to
        // survive being nothing but its words.
        let line = value_line(Theme::plain(), "Enter code", "DHNT-ZSDM");
        assert_eq!(line, "    Enter code DHNT-ZSDM");
        assert!(!line.contains('\x1b'));
    }

    #[test]
    fn a_highlighted_value_is_recorded_as_one_note() {
        // Painted as two spans, recorded as one sentence: a test asserting on
        // what the developer was told should not have to know the line was
        // split to colour it.
        let ui = Ui::new(false);
        ui.note_value("Enter code", "DHNT-ZSDM");
        assert_eq!(ui.noted(), vec!["Enter code DHNT-ZSDM"]);
    }

    #[test]
    fn a_warning_ends_the_status_line_it_interrupts() {
        // The reported bug: `warn` writes to stderr, so it cannot carry the
        // `\r` that covers stdout, and it left the busy line unterminated —
        // "◐ Authorised — installing the key  ▲ riabuild's key is already…"
        // on one line, with the task never resolving.
        let ui = Ui::new(false);
        ui.working("Authorised", "installing the key");
        ui.warn("something to say about it");
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn an_unresolved_task_covers_the_busy_line_like_a_finished_one() {
        let ui = Ui::new(false);
        ui.working("Authorised", "installing the key");
        ui.unresolved("Authorised", "the server refuses it", &[]);
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn an_unresolved_task_is_recorded_as_one_warning() {
        // `Ok(())` on its own is indistinguishable from a step that silently
        // did nothing, so the recorder is what a test asserts a downgraded
        // path actually spoke up — and it should not have to know the block
        // was split into a title and three paragraphs to print it.
        let ui = Ui::new(false);
        ui.unresolved(
            "Authorised",
            "the server refuses it",
            &[
                Detail::Prose("It is already in the file."),
                Detail::Verbatim("ssh-ed25519 AAAA riabuild"),
            ],
        );
        assert_eq!(
            ui.warned(),
            vec![
                "Authorised — the server refuses it It is already in the file. ssh-ed25519 AAAA riabuild"
            ]
        );
    }

    #[test]
    fn a_status_line_is_only_covered_once() {
        let ui = Ui::new(false);
        ui.working("GitHub CLI", "first run");
        ui.applied("GitHub CLI");
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn a_months_worth_of_minutes_reads_as_days() {
        // The number that prompted this: a 30-day Infisical credential, one
        // minute in, rendered as "43199 more minute(s)".
        assert_eq!(duration_words(43_199), "29 days 23 hours 59 minutes");
        assert_eq!(duration_words(43_200), "30 days");
    }

    #[test]
    fn empty_components_are_left_out() {
        assert_eq!(duration_words(1), "1 minute");
        assert_eq!(duration_words(59), "59 minutes");
        assert_eq!(duration_words(60), "1 hour");
        assert_eq!(duration_words(90), "1 hour 30 minutes");
        assert_eq!(duration_words(1440), "1 day");
        assert_eq!(duration_words(1500), "1 day 1 hour");
    }

    #[test]
    fn an_expired_credential_does_not_read_as_zero_minutes() {
        assert_eq!(duration_words(0), "less than a minute");
    }

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

    #[test]
    fn a_failure_carries_all_four_parts() {
        let failure = Failure::new("checking your GitHub sign-in", "run `gh auth login`")
            .command("gh auth status")
            .detail("You are not logged into any GitHub hosts.");

        assert!(failure.command.is_some());
        assert!(!failure.detail.is_empty());
        assert!(failure.safe_to_rerun);
        assert_eq!(
            failure.to_string(),
            "checking your GitHub sign-in — run `gh auth login`"
        );
    }
}

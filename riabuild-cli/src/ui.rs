//! Terminal output.
//!
//! The error shape is the important part: every failure says what was being
//! attempted in the developer's words, the exact command and its stderr, one
//! concrete next action, and whether re-running is safe. A provisioner that
//! fails vaguely is worse than one that does not run.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

/// `Ui::ask`, `Ui::confirm`, their must-be-answered counterparts, and the pure
/// rules behind them — the interactive half of this module, split out because
/// it and the status/failure rendering below it are two different concerns
/// that both happened to fit under 300 lines only while one of them didn't
/// exist yet.
mod prompt;

pub struct Ui {
    colour: bool,
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
    /// Answers a test feeds to `ask` in place of a terminal.
    #[cfg(test)]
    answers: std::sync::Mutex<std::collections::VecDeque<String>>,
    /// Every question actually put to the developer.
    ///
    /// Recorded because the question text is the whole safety guarantee of a
    /// destructive prompt, and it is the one part of it `info` cannot carry:
    /// `info` is silent under `--quiet` and `ask` is not. Without this a
    /// subcommand could name the account in lines nobody sees and still pass.
    #[cfg(test)]
    asked: std::sync::Mutex<Vec<String>>,
    /// Every note actually printed.
    ///
    /// Recorded for the same reason as `asked`, one step further along: a note
    /// is a claim about the machine — "Signed out you@example.com" — and a claim
    /// printed whether or not the thing happened is the failure mode worth
    /// pinning. Filled after the `--quiet` return, so this says the developer
    /// saw it rather than that someone called `note`.
    #[cfg(test)]
    noted: std::sync::Mutex<Vec<String>>,
}

/// Spaces needed for `line` to cover a status line `previous` columns wide.
fn cover(previous: usize, line: &str) -> usize {
    previous.saturating_sub(line.chars().count())
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
        let colour = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self {
            colour,
            // Both halves matter. A piped stdin with a terminal stdout is the
            // shape a CI job has, and a question asked there blocks until
            // something times out. `cfg!(test)` is the same hazard indoors: a
            // test run inherits the terminal `cargo test` was started from.
            interactive: !cfg!(test)
                && std::io::stdin().is_terminal()
                && std::io::stdout().is_terminal(),
            quiet,
            pending: AtomicUsize::new(0),
            #[cfg(test)]
            answers: Default::default(),
            #[cfg(test)]
            asked: Default::default(),
            #[cfg(test)]
            noted: Default::default(),
        }
    }

    /// Every question this `Ui` put, in order.
    #[cfg(test)]
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    /// Every note this `Ui` printed, in order.
    #[cfg(test)]
    pub fn noted(&self) -> Vec<String> {
        self.noted.lock().unwrap().clone()
    }

    /// A `Ui` that answers its own questions, for tests.
    #[cfg(test)]
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
    #[cfg(test)]
    #[must_use]
    pub fn assume_prompts_work(mut self, yes: bool) -> Self {
        self.interactive = yes;
        self
    }

    /// Claims the pending status line, so it is only covered once.
    fn take_pending(&self) -> usize {
        self.pending.swap(0, Ordering::Relaxed)
    }

    /// Whether this terminal gets colour.
    ///
    /// Exposed because the environment shell's banner and prompt are printed by
    /// a generated rcfile rather than by `Ui`, and that file has to bake in the
    /// same `NO_COLOR` decision this made.
    pub fn colour(&self) -> bool {
        self.colour
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

    fn paint(&self, code: &str, text: &str) -> String {
        if self.colour {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn banner(&self, org: &str) {
        if self.quiet {
            return;
        }
        println!();
        println!(
            "{} {}",
            self.paint("1;34", "riabuild"),
            self.paint("2", &format!("· {org} environment")),
        );
    }

    pub fn heading(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("\n{}", self.paint("1", text));
    }

    /// A task that needed nothing.
    pub fn satisfied(&self, title: &str) {
        if self.quiet {
            return;
        }
        println!("  {} {}", self.paint("32", "●"), self.paint("2", title));
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
            self.paint("33", "◐"),
            title,
            self.paint("2", &format!("— {reason}")),
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
        println!("\r  {} {}{}", self.paint("32", "●"), title, padding);
    }

    pub fn note(&self, text: &str) {
        if self.quiet {
            return;
        }
        // A note is written on the end of the status line and ends it, so
        // there is nothing left for `applied` to cover.
        self.take_pending();
        #[cfg(test)]
        self.noted.lock().unwrap().push(text.to_string());
        println!("    {}", self.paint("2", text));
    }

    pub fn warn(&self, text: &str) {
        eprintln!("  {} {}", self.paint("33", "▲"), text);
    }

    pub fn info(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("{text}");
    }

    /// The four things every failure must say.
    pub fn failure(&self, failure: &Failure) {
        eprintln!();
        eprintln!(
            "  {} {}",
            self.paint("1;31", "riabuild stopped:"),
            failure.attempting
        );
        if let Some(command) = &failure.command {
            eprintln!("    {} {}", self.paint("2", "ran"), command);
        }
        for line in failure
            .detail
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(8)
        {
            eprintln!("    {}", self.paint("2", line));
        }
        eprintln!("    {} {}", self.paint("1", "do this:"), failure.action);
        eprintln!(
            "    {}",
            self.paint(
                "2",
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

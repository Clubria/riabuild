//! Terminal output.
//!
//! The error shape is the important part: every failure says what was being
//! attempted in the developer's words, the exact command and its stderr, one
//! concrete next action, and whether re-running is safe. A provisioner that
//! fails vaguely is worse than one that does not run.

use anyhow::Result;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Ui {
    colour: bool,
    quiet: bool,
    /// Columns of a status line left on screen without a newline, so whatever
    /// replaces it can cover the whole thing. Zero means nothing is pending.
    pending: AtomicUsize,
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

/// What a typed answer means, given a default. Pure, so the rules are testable
/// without a terminal.
pub fn answer_or_default(input: &str, default: Option<&str>) -> Option<String> {
    let typed = input.trim();
    if !typed.is_empty() {
        return Some(typed.to_string());
    }
    default.map(str::to_string)
}

/// Only an explicit yes is a yes. Pressing return through a prompt nobody read
/// must not trust a host key.
pub fn is_yes(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
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
            quiet,
            pending: AtomicUsize::new(0),
        }
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

    /// Asks for one value, showing the default in brackets.
    ///
    /// Blocking stdio like the rest of this file (the documented exception to
    /// the async-IO rule). Refuses outright when stdin is not a terminal
    /// rather than attempting a read: an open pipe with nothing written yet
    /// blocks on read rather than returning EOF, so `IsTerminal` — "is a
    /// human plausibly there" — is checked before any read is attempted.
    #[allow(dead_code)] // consumed by Task 21
    pub fn ask(&self, label: &str, default: Option<&str>) -> Result<String> {
        if !std::io::stdin().is_terminal() {
            return Err(Failure::new(
                format!("asking you for {label}"),
                "Pass the server as `riabuild remote <user>@<host>:<port>` — \
                 there is no terminal here to ask in.",
            )
            .into());
        }
        loop {
            match default {
                Some(value) => print!("  {label} [{value}] "),
                None => print!("  {label} "),
            }
            std::io::stdout().flush()?;

            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                // stdin closed mid-prompt.
                return Err(Failure::new(
                    format!("asking you for {label}"),
                    "Run `riabuild remote` again from a terminal.",
                )
                .into());
            }
            if let Some(answer) = answer_or_default(&line, default) {
                return Ok(answer);
            }
        }
    }

    /// Asks a yes/no question. Defaults to no: an empty answer, or no
    /// terminal at all, must never read as consent — the caller this exists
    /// for is trusting a host key nobody has looked at yet.
    #[allow(dead_code)] // consumed by Task 15
    pub fn confirm(&self, question: &str) -> Result<bool> {
        if !std::io::stdin().is_terminal() {
            return Err(Failure::new(
                format!("asking you to confirm: {question}"),
                "Run `riabuild remote` from a terminal, where you can answer this.",
            )
            .into());
        }
        print!("  {question} [y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(is_yes(&line))
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
    fn an_empty_answer_takes_the_default_and_a_typed_one_wins() {
        assert_eq!(answer_or_default("", Some("22")), Some("22".into()));
        assert_eq!(answer_or_default("  \n", Some("22")), Some("22".into()));
        assert_eq!(answer_or_default("2222\n", Some("22")), Some("2222".into()));
        assert_eq!(answer_or_default("  ada  ", None), Some("ada".into()));
        // No default and no answer is not an answer.
        assert_eq!(answer_or_default("", None), None);
    }

    #[test]
    fn confirmation_defaults_to_no() {
        // The fingerprint prompt is the one this exists for. Anything other than an
        // explicit yes has to mean no, or a developer pressing return through a
        // prompt they did not read trusts a host key they have never seen.
        assert!(is_yes("y"));
        assert!(is_yes("Y\n"));
        assert!(is_yes("yes"));
        assert!(!is_yes(""));
        assert!(!is_yes("\n"));
        assert!(!is_yes("n"));
        assert!(!is_yes("sure"));
    }

    // Proves the guard against `std::io::stdin()` itself, not just the pure
    // helpers above: redirects the process's real fd 0 to `/dev/null` so the
    // assertion holds everywhere, not only when the test binary happens to
    // be launched non-interactively (true in CI, not guaranteed at a
    // developer's terminal) — that ambient dependence is exactly what could
    // hide a hang.
    #[cfg(unix)]
    #[test]
    fn a_closed_stdin_returns_an_error_instead_of_blocking() {
        use std::os::fd::AsRawFd;

        unsafe extern "C" {
            fn dup(fd: i32) -> i32;
            fn dup2(oldfd: i32, newfd: i32) -> i32;
            fn close(fd: i32) -> i32;
        }

        let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");

        // SAFETY: POSIX calls over raw fds this process owns; stdin is
        // restored before the test returns, on every path.
        let saved_stdin = unsafe { dup(0) };
        assert!(saved_stdin >= 0, "failed to save the real stdin fd");
        let redirected = unsafe { dup2(devnull.as_raw_fd(), 0) };
        assert_eq!(redirected, 0, "failed to point stdin at /dev/null");

        let ask_result = Ui::new(true).ask("host", None);
        let confirm_result = Ui::new(true).confirm("proceed?");

        unsafe {
            dup2(saved_stdin, 0);
            close(saved_stdin);
        }

        let ask_err = ask_result.expect_err("ask must refuse a non-terminal stdin");
        assert!(ask_err.to_string().contains("host"));
        let confirm_err = confirm_result.expect_err("confirm must refuse a non-terminal stdin");
        assert!(confirm_err.to_string().contains("proceed?"));
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

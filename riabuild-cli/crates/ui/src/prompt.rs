//! Interactive prompts, and the pure rules behind them.
//!
//! The property that matters is refusing to hang. riabuild runs in CI, in
//! scripts, and over SSH, where stdin may not be a terminal or may be
//! closed. `IsTerminal` is checked before any read is attempted, because it
//! answers "is a human plausibly there" — an EOF check alone cannot: an open
//! pipe with nothing written to it yet blocks on `read_line` rather than
//! returning, so waiting to discover that from a read is exactly the hang
//! this exists to prevent.
//!
//! There are two pairs here, and the difference between them is what happens
//! when nobody is there:
//!
//! * `ask` and `confirm` return `None`. They are how riabuild *offers a
//!   choice*, so every caller already has a default and an unattended run
//!   simply takes it. This is the crate-wide rule — see "Every prompt has a
//!   default" in `riabuild-cli/CLAUDE.md`.
//! * `ask_required` and `confirm_required` return `Err`. They are for
//!   `riabuild remote`, where the value — a hostname, or consent to trust a
//!   host key nobody has looked at — has no default that could be safely
//!   assumed. Failing with an explanation is the only alternative to inventing
//!   an answer.
//!
//! The first pair reads `Ui::interactive`, which a test can force. The second
//! re-checks the real terminal at the point of use and deliberately does not:
//! `assume_prompts_work(true)` is how the rest of the suite models a developer
//! being present, and trusting it here would turn a test into a blocking read
//! on the terminal `cargo test` was launched from.

use super::{Failure, Ui};
use anyhow::Result;
use std::io::{IsTerminal, Write};

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

impl Ui {
    /// Reads one line from the developer.
    ///
    /// `None` means there is no answer — they pressed Enter or ^D, or there is
    /// no terminal to ask. Callers must therefore always have a default:
    /// asking is how riabuild offers a choice, never how it obtains a value it
    /// cannot otherwise get. When there is no default, `ask_required` below is
    /// the one to reach for.
    pub fn ask(&self, question: &str) -> Option<String> {
        if !self.interactive {
            return None;
        }
        // The question is written on its own line rather than on the end of
        // any pending status line, which is already long enough to carry the
        // reason a task is running.
        self.take_pending();
        let answer = self.read_answer(question)?;
        let answer = answer.trim().to_string();
        (!answer.is_empty()).then_some(answer)
    }

    #[cfg(not(any(test, feature = "testing")))]
    fn read_answer(&self, question: &str) -> Option<String> {
        println!();
        print!(
            "    {} ",
            self.paint(riabuild_theme::Role::Strong, question)
        );
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            // Zero bytes is ^D, which reads as "just use the default".
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }

    #[cfg(any(test, feature = "testing"))]
    fn read_answer(&self, question: &str) -> Option<String> {
        self.asked.lock().unwrap().push(question.to_string());
        self.answers.lock().unwrap().pop_front()
    }

    /// Asks a yes/no question, defaulting to yes.
    ///
    /// `None` means the question could not be put — `--quiet`, or nobody on the
    /// other end — which is a different answer from "no" and has to stay
    /// distinguishable: a caller that treats it as a refusal silently skips
    /// work, and one that treats it as consent runs `sudo` in a CI job.
    ///
    /// Built on `read_answer` rather than reading stdin directly, so it obeys
    /// the same `interactive` rule as `ask` and can be driven by `scripted` in
    /// a test. ^D reads as "could not ask" rather than as no: it is the absence
    /// of an answer, and the caller already knows what to do without one.
    pub fn confirm(&self, question: &str) -> Option<bool> {
        if self.quiet || !self.interactive {
            return None;
        }
        self.take_pending();
        let answer = self.read_answer(&format!("{question} [Y/n]"))?;
        let answer = answer.trim().to_lowercase();
        Some(answer.is_empty() || answer == "y" || answer == "yes")
    }

    /// Asks for one value that has no usable default, showing the suggestion
    /// in brackets.
    ///
    /// Blocking stdio like the rest of this file (the documented exception to
    /// the async-IO rule). Refuses outright when stdin is not a terminal
    /// rather than attempting a read: an open pipe with nothing written yet
    /// blocks on read rather than returning EOF, so `IsTerminal` — "is a
    /// human plausibly there" — is checked before any read is attempted.
    pub fn ask_required(&self, label: &str, default: Option<&str>) -> Result<String> {
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

    /// Asks a yes/no question that has to be answered. Defaults to no: an
    /// empty answer, or no terminal at all, must never read as consent — the
    /// caller this exists for is trusting a host key nobody has looked at yet.
    ///
    /// Distinct from `confirm` above, which defaults to *yes* and is allowed
    /// to give up. That default is right for "shall I upgrade?" and wrong for
    /// every question here.
    pub fn confirm_required(&self, question: &str) -> Result<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_terminal_means_no_answer() {
        // Every caller must have a default: riabuild runs in CI and over pipes,
        // where a blocking read would hang until something times out.
        assert_eq!(Ui::new(false).ask("Where should this live?"), None);
    }

    #[test]
    fn an_answer_is_returned_trimmed() {
        let ui = Ui::scripted(["  ~/work/hub \n"]);
        assert_eq!(ui.ask("Where?").as_deref(), Some("~/work/hub"));
    }

    #[test]
    fn an_empty_answer_means_the_default() {
        // Enter accepts whatever was offered.
        assert_eq!(Ui::scripted([""]).ask("Where?"), None);
    }

    #[test]
    fn running_out_of_answers_means_no_answer() {
        // Stands in for the developer pressing ^D.
        assert_eq!(Ui::scripted([] as [&str; 0]).ask("Where?"), None);
    }

    #[test]
    fn a_question_ends_the_status_line_it_was_asked_under() {
        let ui = Ui::scripted(["~/work/hub"]);
        ui.working("Project checkout", "first run");
        ui.ask("Where?");
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn enter_accepts_a_confirmation() {
        // The prompt says [Y/n], so Enter has to mean yes. Getting this
        // backwards makes riabuild refuse its own upgrade for anyone who
        // answers the way the prompt tells them to.
        assert_eq!(Ui::scripted([""]).confirm("Upgrade?"), Some(true));
        assert_eq!(Ui::scripted(["y"]).confirm("Upgrade?"), Some(true));
        assert_eq!(Ui::scripted(["YES\n"]).confirm("Upgrade?"), Some(true));
    }

    #[test]
    fn anything_else_declines() {
        for answer in ["n", "no", "nope", "later"] {
            assert_eq!(
                Ui::scripted([answer]).confirm("Upgrade?"),
                Some(false),
                "{answer}"
            );
        }
    }

    #[test]
    fn a_question_that_cannot_be_put_is_not_a_no() {
        // `None` is what tells update.rs to print the command instead of
        // running sudo. Collapsing it into `Some(false)` would silently skip a
        // mandatory upgrade; collapsing it into `Some(true)` would run sudo in
        // a CI job with nobody there to type a password.
        assert_eq!(Ui::new(false).confirm("Upgrade?"), None);
        // --quiet, even with someone there and an answer waiting.
        let quiet = Ui {
            quiet: true,
            ..Ui::scripted(["y"])
        };
        assert_eq!(quiet.confirm("Upgrade?"), None);
        // ^D is the absence of an answer, not a refusal.
        assert_eq!(Ui::scripted([] as [&str; 0]).confirm("Upgrade?"), None);
    }

    /// The two pairs must not converge. `confirm` defaults to yes and gives up
    /// quietly; `confirm_required` defaults to no and refuses. Swapping one for
    /// the other at a call site is how a host-key prompt nobody read becomes a
    /// trusted key.
    #[test]
    fn the_two_confirmations_default_opposite_ways() {
        assert_eq!(Ui::scripted([""]).confirm("Upgrade?"), Some(true));
        assert!(!is_yes(""));
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

    // --- stdin redirection below: fd 0 is process-global, and `cargo test`
    // runs tests in parallel threads within one process. No other test in
    // this crate may read `std::io::stdin()` while a `StdinGuard` is alive —
    // none currently do (checked before adding this). The `ask`/`confirm`
    // tests above are not an exception: under `cfg(test)` they go through the
    // scripted `read_answer`, which never touches fd 0. If that ever changes,
    // this file needs to know about it.

    unsafe extern "C" {
        fn dup(fd: i32) -> i32;
        fn dup2(oldfd: i32, newfd: i32) -> i32;
        fn close(fd: i32) -> i32;
    }

    /// Points fd 0 at a given file for its lifetime, and restores the real
    /// stdin on drop — including on unwind, so a failed assertion inside the
    /// guarded block can never leave fd 0 pointed at a redirected file for
    /// the rest of the test binary process.
    struct StdinGuard {
        saved: i32,
    }

    impl StdinGuard {
        fn redirect(file: &std::fs::File) -> Self {
            use std::os::fd::AsRawFd;
            // SAFETY: POSIX calls over raw fds this process owns. `saved` and
            // the redirect result are checked before use.
            let saved = unsafe { dup(0) };
            assert!(saved >= 0, "failed to save the real stdin fd");
            let redirected = unsafe { dup2(file.as_raw_fd(), 0) };
            assert_eq!(redirected, 0, "failed to redirect stdin");
            Self { saved }
        }
    }

    impl Drop for StdinGuard {
        fn drop(&mut self) {
            // SAFETY: restores the fd this process owned before `redirect`,
            // on every path, including unwind.
            unsafe {
                dup2(self.saved, 0);
                close(self.saved);
            }
        }
    }

    // What this proves, and what it doesn't: redirecting fd 0 to `/dev/null`
    // makes any `read` on it return EOF immediately, so this shows the
    // *result* — a prompt returns `Err` promptly rather than hanging — but
    // not the *reason*. A hypothetical "read first, treat EOF as
    // not-a-terminal" ordering would also pass this test, by taking the
    // "stdin closed mid-prompt" branch instead of the `IsTerminal` one. It
    // does NOT exercise the actual hang risk this file's doc comment
    // describes: an open pipe with no data yet, where a naive read blocks
    // instead of returning. That case is deliberately left uncovered — a
    // deterministic test for it would need either a timeout (flaky: too
    // short gives false confidence on a regression, too long slows every
    // run, and either way it only shows "didn't block within N ms", not
    // "never blocks") or a spawned thread whose blocking read is never
    // joined (bounds the test, but proves nothing more than the timeout
    // version would, and normalizes leaking a thread on every run). Both
    // trade a false sense of proof for coverage, so neither is here. The
    // ordering itself was verified by direct code reading instead:
    // `is_terminal()` is the first statement in both `ask_required` and
    // `confirm_required`, before any `print!` or `read_line`.
    #[cfg(unix)]
    #[test]
    fn a_closed_stdin_returns_an_error_instead_of_blocking() {
        let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
        let _guard = StdinGuard::redirect(&devnull);

        let ask_err = Ui::new(true)
            .ask_required("host", None)
            .expect_err("ask_required must refuse a non-terminal stdin");
        assert!(ask_err.to_string().contains("host"));

        let confirm_err = Ui::new(true)
            .confirm_required("proceed?")
            .expect_err("confirm_required must refuse a non-terminal stdin");
        assert!(confirm_err.to_string().contains("proceed?"));
    }
}

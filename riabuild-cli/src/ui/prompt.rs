//! Interactive prompts: `Ui::ask` and `Ui::confirm`.
//!
//! The property that matters is refusing to hang. riabuild runs in CI, in
//! scripts, and over SSH, where stdin may not be a terminal or may be
//! closed. `IsTerminal` is checked before any read is attempted, because it
//! answers "is a human plausibly there" — an EOF check alone cannot: an open
//! pipe with nothing written to it yet blocks on `read_line` rather than
//! returning, so waiting to discover that from a read is exactly the hang
//! this exists to prevent.

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
#[allow(dead_code)] // consumed by Task 21, via Ui::confirm
pub fn is_yes(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

impl Ui {
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
    #[allow(dead_code)] // consumed by Task 21, via identity::trust_host
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

#[cfg(test)]
mod tests {
    use super::*;

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
    // none currently do (checked before adding this). If that ever changes,
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
    // `is_terminal()` is the first statement in both `ask` and `confirm`,
    // before any `print!` or `read_line`.
    #[cfg(unix)]
    #[test]
    fn a_closed_stdin_returns_an_error_instead_of_blocking() {
        let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
        let _guard = StdinGuard::redirect(&devnull);

        let ask_err = Ui::new(true)
            .ask("host", None)
            .expect_err("ask must refuse a non-terminal stdin");
        assert!(ask_err.to_string().contains("host"));

        let confirm_err = Ui::new(true)
            .confirm("proceed?")
            .expect_err("confirm must refuse a non-terminal stdin");
        assert!(confirm_err.to_string().contains("proceed?"));
    }
}

//! Whether anybody is on the other end, and on which stream.
//!
//! Four call sites used to answer this for themselves — `Ui::new` twice,
//! `prompt`'s two `_required` guards, and `StatusBar::on_second_line` — each
//! with its own `std::io::…().is_terminal()` and, in two cases, its own copy of
//! the paragraph explaining why `cfg!(test)` alone is not enough. They are not
//! all the same question, which is exactly why they need naming rather than
//! collapsing: stdin decides whether a blocking read has anyone to block on,
//! stdout decides whether colour is worth emitting, stderr is the stream the
//! status bar pins a line to, and "attended" is the conjunction that gates a
//! question. A fifth site written from scratch would pick one of them by
//! accident.
//!
//! `riabuild-runner`'s `pty::available` is deliberately not one of these. It
//! asks the same thing of the same two descriptors, but `riabuild-runner`
//! depends on `riabuild-theme` alone and reaching this module would be a new
//! edge in the crate graph — which the layout in `CLAUDE.md` treats as a
//! structural decision, not a tidy-up.

use std::io::IsTerminal;

/// Whether this binary is a test binary.
///
/// Both halves matter. `cfg!(test)` alone is false in *this* crate while a
/// downstream crate's tests run — `riabuild-ui` is compiled as an ordinary
/// dependency there — and `cargo test` inherits the real terminal of the shell
/// that started it. So without the feature half, a test in `riabuild-remote`
/// would find a live tty, paint a status bar over the developer's screen and
/// leave it there, or block waiting for an answer nobody is going to type.
pub fn under_test() -> bool {
    cfg!(any(test, feature = "testing"))
}

/// Whether there is a developer here who can answer a question.
///
/// Both descriptors, not either: a piped stdin with a terminal stdout is the
/// shape a CI job has, and a question asked there blocks until something times
/// out.
pub fn attended() -> bool {
    !under_test() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Whether a blocking read of stdin has anyone to block on.
///
/// Narrower than [`attended`] on purpose, and the difference is load-bearing:
/// this is what `ask_required` and `confirm_required` check, and they must
/// refuse rather than hang. An open pipe with nothing written yet blocks on
/// read instead of returning EOF, so the question is asked of the descriptor
/// before any read is attempted.
pub fn stdin_answers() -> bool {
    std::io::stdin().is_terminal()
}

/// Whether stdout is a terminal, and so whether colour is worth emitting.
pub fn can_paint() -> bool {
    std::io::stdout().is_terminal()
}

/// Whether stderr can hold a pinned line — the surface [`crate::StatusBar`]
/// owns. Not stdout: the bar sits beside output riabuild is still writing.
pub fn can_pin_a_line() -> bool {
    !under_test() && std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_binary_is_never_attended_and_never_paints_a_bar() {
        // `cargo test` inherits the terminal of the shell that started it, so
        // on a developer's laptop the descriptors below really are ttys. These
        // two must still be false, and that is the whole of what `under_test`
        // buys — running this suite from a terminal is the only way to observe
        // it, and CI, where the descriptors are pipes, cannot.
        assert!(under_test());
        assert!(!attended());
        assert!(!can_pin_a_line());
    }
}

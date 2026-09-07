//! A question that takes its default when nobody answers it.
//!
//! `Ui::ask` waits for ever, which is right for every question riabuild puts
//! *because something has gone wrong* and wrong for the one it puts on every
//! single run. The repository picker is that one: a developer who is going to
//! press Enter should not have to, and a developer who wants a different
//! repository has to be able to say so before riabuild moves on. Both are the
//! same requirement seen from either end — the wait is bounded, and typing
//! anything at all cancels it.
//!
//! Three departures from `prompt` next door, each load-bearing:
//!
//! - **The terminal reads a key at a time.** A canonical-mode `read_line`
//!   hands nothing over until Enter, so riabuild could not tell a developer
//!   half way through typing `payments` from one who has walked away — and
//!   would take the default out from under them mid-word. `ICANON` off is what
//!   makes "they have started typing" observable at all.
//! - **Echo is off and riabuild does the echoing.** The countdown and what is
//!   being typed occupy the same line, and the driver would put the two in
//!   whichever order the keystroke and the tick happened to arrive in.
//! - **The clock stops on the first keystroke.** It never restarts. A
//!   developer who is typing is answering, and a timer that could still expire
//!   under them would be riabuild racing somebody it can see is there.
//!
//! Nothing here relaxes "every prompt has a default" — it is that rule with
//! the wait made finite. [`Waited::Unanswered`] is the case the rule never had
//! to name before, and it is a different fact from Enter: it says nobody was
//! reading, so a caller with a *second* question to put knows not to put it.

use super::{Ui, words::plural};
use std::time::Duration;

/// What a question that would not wait for ever came back with.
///
/// Three cases where [`Ui::ask`] has two, and the third is the point.
/// `ask` collapses "they pressed Enter" and "nobody is there" into `None`
/// because it has no way to tell them apart; a bounded wait does, and a caller
/// that goes on to ask a second question has to know which it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    /// They typed something, and the clock stopped when they did.
    Typed(String),
    /// Enter, or ^D. An answer, and it means "the one you offered".
    Offered,
    /// The clock ran out with nothing typed. Also "the one you offered", but
    /// it is not an answer — there was nobody reading the question.
    Unanswered,
}

/// The scripted answer that models nobody typing anything before the clock ran
/// out.
///
/// A sentinel in the answer list rather than a flag on the whole `Ui`, because
/// walking away is a fact about *one question in a sequence* — the picker puts
/// two, back to back — and a flag could not say which of them was missed.
#[cfg(any(test, feature = "testing"))]
pub const NOBODY_ANSWERED: &str = "\u{1b}nobody-answered";

/// How long the countdown line claims is left.
///
/// Rounded up, so a five-second wait opens on `5` rather than on the `4` a
/// developer would read as having already missed a second, and so the last
/// tick reads `1 second` rather than `0 seconds`. Zero is the expiry itself
/// and is never drawn.
pub fn seconds_left(remaining: Duration) -> u64 {
    let millis: u64 = remaining.as_millis().try_into().unwrap_or(u64::MAX);
    millis.div_ceil(1_000)
}

/// The line the countdown redraws, in the words a developer reads.
///
/// `taking` arrives already painted, because what to colour is this crate's
/// decision and how to colour it is the `Theme`'s — keeping the wording pure
/// is what lets the plural be asserted without a terminal or a clock.
pub fn selecting(taking: &str, remaining: Duration) -> String {
    format!(
        "selecting {taking} in {}...",
        plural(seconds_left(remaining), "second")
    )
}

/// The line the countdown leaves behind once it has run out.
///
/// The same sentence with the clock taken out of it, rather than the last tick
/// frozen at `in 0 seconds...`: what a developer scrolls back to should say
/// what happened, and a countdown stopped at zero reads as one that was
/// interrupted.
pub fn selected(taking: &str) -> String {
    format!("selecting {taking}...")
}

impl Ui {
    /// Asks, and gives up waiting after `patience` if nothing has been typed.
    ///
    /// The same contract as [`Ui::ask`] in every other respect: there is no
    /// terminal, or no answer, and the caller takes the default it already
    /// had. What is added is that riabuild says out loud what it is about to
    /// take and how long is left to say otherwise — `taking` is that name, and
    /// it is the developer's word for the thing, not a slug or a row number.
    pub fn ask_within(&self, question: &str, taking: &str, patience: Duration) -> Waited {
        if !self.interactive {
            return Waited::Unanswered;
        }
        // The question goes on its own line rather than on the end of a status
        // line, which is already carrying the reason a task is running.
        self.take_pending();
        self.wait_for_answer(question, taking, patience)
    }
}

#[cfg(all(unix, not(any(test, feature = "testing"))))]
mod real {
    use super::{Ui, Waited, selected, selecting};
    use riabuild_theme::Role;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    /// Erase from the cursor to the end of the line. Every line here is
    /// redrawn from column zero, so what the previous draw left past the new
    /// end has to go — otherwise `5 seconds` leaves its `s` behind `1 second`.
    const CLEAR_TO_END: &str = "\x1b[K";

    impl Ui {
        pub(super) fn wait_for_answer(
            &self,
            question: &str,
            taking: &str,
            patience: Duration,
        ) -> Waited {
            println!();
            println!("    {}", self.paint(Role::Strong, question));
            let _ = std::io::stdout().flush();

            // A terminal riabuild cannot take out of canonical mode is one
            // where the countdown could not be cancelled by typing — so the
            // question is put the ordinary way instead of being put with a
            // clock nobody can beat. `interactive` says a person is there;
            // this says whether riabuild can watch them arrive.
            let Some(_raw) = RawKeys::on(libc::STDIN_FILENO) else {
                return match self.read_line_now() {
                    Some(typed) => Waited::Typed(typed),
                    None => Waited::Offered,
                };
            };

            let taking = self.paint(Role::Brand, taking);
            let started = Instant::now();
            let mut typed: Vec<u8> = Vec::new();
            // What is on screen, so the loop can tell a tick that changed the
            // line from the nine each second that did not. The read below
            // returns every 0.1s and the words change once a second, so
            // redrawing unconditionally would send the same sixty bytes ten
            // times a second down whatever the developer is connected over —
            // and riabuild's own remote mode is one of those.
            let mut shown = String::new();
            loop {
                // Once anything has been typed the clock is gone for good and
                // the line belongs to the developer: riabuild is echoing for a
                // terminal it told to stop, and a countdown drawn beside a
                // half-typed name would be a threat riabuild has already
                // decided not to carry out.
                let deadline = match typed.is_empty() {
                    false => None,
                    true => Some(patience.saturating_sub(started.elapsed())),
                };
                let line = match deadline {
                    Some(remaining) if remaining.is_zero() => {
                        self.draw(&selected(&taking));
                        println!();
                        return Waited::Unanswered;
                    }
                    Some(remaining) => selecting(&taking, remaining),
                    None => String::from_utf8_lossy(&typed).into_owned(),
                };
                if line != shown {
                    self.draw(&line);
                    shown = line;
                }

                // One byte at a time, so nothing past the newline is ever
                // consumed. A developer who types `2`, Enter, `y`, Enter
                // through both of the picker's questions would otherwise lose
                // the `y` to a read that took the whole burst, and the second
                // question would sit waiting for an answer already given.
                let mut byte = [0u8; 1];
                match std::io::stdin().read(&mut byte) {
                    // The 0.1s `VTIME` tick with nothing typed. That is the
                    // clock ticking, not the end of anything.
                    Ok(0) => continue,
                    Err(_) => return Waited::Offered,
                    Ok(_) => {}
                }
                match byte[0] {
                    b'\n' | b'\r' => {
                        println!();
                        let answer = String::from_utf8_lossy(&typed).trim().to_string();
                        return match answer.is_empty() {
                            true => Waited::Offered,
                            false => Waited::Typed(answer),
                        };
                    }
                    // ^D. With something typed it is the end of the line, and
                    // with nothing it is "just use the default" — the reading
                    // `ask` already gives it.
                    0x04 => {
                        println!();
                        let answer = String::from_utf8_lossy(&typed).trim().to_string();
                        return match answer.is_empty() {
                            true => Waited::Offered,
                            false => Waited::Typed(answer),
                        };
                    }
                    // Backspace, both spellings. Whole characters, not bytes:
                    // an accented name half-deleted would leave a fragment no
                    // further keystroke could clear.
                    0x7f | 0x08 => {
                        while matches!(typed.last(), Some(byte) if byte & 0xc0 == 0x80) {
                            typed.pop();
                        }
                        typed.pop();
                    }
                    // Every other control byte is dropped rather than echoed.
                    // An arrow key arrives as three of them, and riabuild does
                    // not edit a line — showing `^[[A` in the answer would be
                    // worse than ignoring the key.
                    byte if byte < 0x20 => {}
                    byte => typed.push(byte),
                }
            }
        }

        /// Redraws the one line under the question, from column zero.
        fn draw(&self, line: &str) {
            print!("\r    {line}{CLEAR_TO_END}");
            let _ = std::io::stdout().flush();
        }

        /// The ordinary blocking read, for a terminal that would not go into
        /// raw mode. `None` is Enter or ^D, exactly as in `ask`.
        fn read_line_now(&self) -> Option<String> {
            print!("    ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => {
                    let line = line.trim().to_string();
                    (!line.is_empty()).then_some(line)
                }
            }
        }
    }

    /// The terminal handing over one key at a time, with echo off, for as long
    /// as this is alive.
    ///
    /// The restore is in `Drop` and not on the happy path for the reason
    /// `secret::EchoOff` gives: every way out of the loop above — an answer, a
    /// read error, an expiry — has to put the terminal back, and a developer
    /// whose shell is left in raw mode has no way to know why.
    struct RawKeys {
        fd: std::os::unix::io::RawFd,
        original: libc::termios,
    }

    impl RawKeys {
        fn on(fd: std::os::unix::io::RawFd) -> Option<Self> {
            // SAFETY: `fd` is this process's stdin, open for the whole run,
            // and `termios` is a plain struct the call fills in.
            let mut original: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
                return None;
            }

            let mut raw = original;
            // `ISIG` is deliberately left alone: ^C during this question has
            // to kill riabuild exactly as it does during every other one, and
            // a handler of riabuild's own would be a second answer to a
            // question the shell already answers.
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            // Return from `read` after 0.1s with nothing, rather than blocking
            // until a key arrives. That tick is what redraws the countdown,
            // and it is why there is no timer thread here.
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 1;
            // `TCSAFLUSH` discards anything typed before the question
            // appeared, so a stray keystroke from the command that ran before
            // this one cannot cancel a countdown nobody has seen yet.
            if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
                return None;
            }
            Some(Self { fd, original })
        }
    }

    impl Drop for RawKeys {
        fn drop(&mut self) {
            // `TCSADRAIN`, not `TCSAFLUSH`: what is still unread is what the
            // developer typed ahead at the *next* question, and discarding it
            // would leave that question waiting for an answer already given.
            // Nothing useful can be done if it fails, and a panic in `Drop`
            // during unwinding aborts the process.
            unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.original) };
        }
    }
}

/// Windows, and any Unix without a controllable terminal at build time.
///
/// The wait is unbounded here rather than absent: taking the default without
/// asking would be riabuild deciding, and asking without a clock is what every
/// other question in this crate does anyway.
#[cfg(all(not(unix), not(any(test, feature = "testing"))))]
impl Ui {
    fn wait_for_answer(&self, question: &str, _taking: &str, _patience: Duration) -> Waited {
        match self.read_answer(question) {
            Some(line) => match line.trim() {
                "" => Waited::Offered,
                typed => Waited::Typed(typed.to_string()),
            },
            None => Waited::Offered,
        }
    }
}

/// The scripted half: no clock, no terminal, and the sentinel above standing
/// in for the developer who was not there.
///
/// The same split `prompt::read_answer` uses, and for the same reason — a test
/// that reached a real countdown would spend five seconds of every run
/// blocking on the keyboard `cargo test` was launched from.
#[cfg(any(test, feature = "testing"))]
impl Ui {
    fn wait_for_answer(&self, question: &str, _taking: &str, _patience: Duration) -> Waited {
        match self.read_answer(question) {
            Some(line) if line.trim() == NOBODY_ANSWERED => Waited::Unanswered,
            Some(line) => match line.trim() {
                "" => Waited::Offered,
                typed => Waited::Typed(typed.to_string()),
            },
            None => Waited::Offered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_five_second_wait_counts_five_four_three_two_one() {
        // Opening on `4` is the off-by-one this rounds away: a developer who
        // reads the first frame as having already lost a second is being told
        // riabuild is faster than it is.
        let shown: Vec<u64> = [5000u64, 4001, 4000, 2500, 1000, 999, 1]
            .iter()
            .map(|millis| seconds_left(Duration::from_millis(*millis)))
            .collect();
        assert_eq!(shown, [5, 5, 4, 3, 1, 1, 1]);
        assert_eq!(seconds_left(Duration::ZERO), 0);
    }

    #[test]
    fn the_last_tick_is_one_second_not_one_seconds() {
        assert_eq!(
            selecting("ai-builders-hub", Duration::from_secs(1)),
            "selecting ai-builders-hub in 1 second..."
        );
        assert_eq!(
            selecting("ai-builders-hub", Duration::from_secs(5)),
            "selecting ai-builders-hub in 5 seconds..."
        );
    }

    #[test]
    fn typing_something_is_an_answer_and_enter_is_the_one_offered() {
        let ui = Ui::scripted(["2"]);
        assert_eq!(
            ui.ask_within(
                "Which repository?",
                "ai-builders-hub",
                Duration::from_secs(5)
            ),
            Waited::Typed("2".into())
        );
        let ui = Ui::scripted([""]);
        assert_eq!(
            ui.ask_within(
                "Which repository?",
                "ai-builders-hub",
                Duration::from_secs(5)
            ),
            Waited::Offered
        );
    }

    #[test]
    fn nobody_reading_is_not_the_same_answer_as_enter() {
        // The whole reason this returns three cases rather than two. A caller
        // with a second question to put must be able to tell "they chose the
        // default" from "there was nobody there", and `ask` cannot.
        let ui = Ui::scripted([NOBODY_ANSWERED]);
        assert_eq!(
            ui.ask_within(
                "Which repository?",
                "ai-builders-hub",
                Duration::from_secs(5)
            ),
            Waited::Unanswered
        );
    }

    #[test]
    fn no_terminal_never_waits_at_all() {
        // CI, a pipe, a `--check`. Nobody is there to type, so there is
        // nothing to count down to.
        assert_eq!(
            Ui::new(false).ask_within("Which repository?", "hub", Duration::from_secs(5)),
            Waited::Unanswered
        );
    }

    #[test]
    fn a_question_ends_the_status_line_it_was_asked_under() {
        let ui = Ui::scripted(["payments"]);
        ui.working("Project checkout", "first run");
        ui.ask_within("Which repository?", "hub", Duration::from_secs(5));
        assert_eq!(ui.take_pending(), 0);
    }
}

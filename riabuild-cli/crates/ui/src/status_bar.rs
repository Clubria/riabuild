//! One line of state, pinned to a row of a terminal something else is drawing.
//!
//! Everything else in `ui/` prints: a line is written, the cursor moves down,
//! the terminal scrolls. That is the right shape for a run riabuild owns the
//! screen for, and the wrong one for the only background task riabuild has.
//! The clipboard channel's supervisor lives *beside* the developer's remote
//! shell — a mosh session, usually with a full-screen Claude Code inside it —
//! and prints from the laptop into a terminal that program is painting. Two
//! things go wrong there, and the first is the one developers report:
//!
//! - **The newlines come out ruined.** An interactive shell puts the terminal
//!   in raw mode, where `\n` moves down a row and does *not* return to column
//!   one. `println!` was never wrong about this; it simply assumes a terminal
//!   in the mode nobody is in during a remote session, so a folded warning
//!   arrives as a staircase down the right-hand side of the screen.
//! - **It lands in the middle of somebody else's output**, and stays there,
//!   because the program that owns the screen does not know a line appeared.
//!
//! So the channel gets a status bar instead of a message: one line, at a fixed
//! row, written with the cursor saved and put back, so nothing the developer is
//! doing moves. It is the shape mosh already uses for the same problem — its
//! own bar sits on row one, which is why riabuild's is on row two rather than
//! fighting it for the same cells.
//!
//! What this is not is a general printer. A bar holds one line and truncates
//! it; the folded prose, the detail and the next action all belong to the runs
//! that own their screen, and stay with `Ui`.

use crate::wrap;
use riabuild_theme::{Role, Theme};
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

/// The row the bar is drawn on, counting from one.
///
/// Row two, not row one, and that is the whole of the placement decision: mosh
/// draws its own "[mosh] Last contact 3 seconds ago" on row one, and two
/// programs writing the same cells produce whichever repainted last. One line
/// down is out of its way and still above everything the developer is reading.
const ROW: u16 = 2;

/// Save the cursor, including where it is and what it is painting with.
///
/// `ESC 7`, not `CSI s`. They do the same thing on xterm and its descendants,
/// and only this one is honoured by the terminals that predate them — which,
/// through mosh, is what the far end of a session may well be emulating.
const SAVE: &str = "\x1b7";
const RESTORE: &str = "\x1b8";

pub struct StatusBar {
    /// The developer's terminal, or `None` when there is not one to draw on —
    /// which is every run except a remote session, and is why every method
    /// here is a no-op rather than a failure.
    ///
    /// `/dev/tty` rather than stdout, for the reason `secret` opens it too: it
    /// is the terminal regardless of what the process's own streams were
    /// redirected to, and a supervisor started under a shell whose output is
    /// piped still has a developer looking at a screen.
    tty: Option<Mutex<File>>,
    theme: Theme,
    /// The line that is on the screen now, or `None` when the row is clear.
    ///
    /// Held rather than written and forgotten, because the program underneath
    /// repaints: a full-screen shell writing anything to row two erases the bar
    /// without anybody being told, and the only repair is to draw it again.
    /// [`repaint`](Self::repaint) is what a caller ticks to do that.
    ///
    /// It is also the lock every write is made under, which is what stops a
    /// repaint that has already read the line from painting it back *after* a
    /// clear — the one ordering that would leave a stale bar on the screen for
    /// the rest of the session.
    showing: Mutex<Option<String>>,
}

impl StatusBar {
    /// The bar a remote session's channel speaks through.
    ///
    /// Disabled — every call a no-op — when there is no terminal to hold a line
    /// on, and under `--quiet`, which asks for no decoration at all. A caller
    /// that finds it disabled prints the ordinary way instead; see
    /// `supervisor::report`.
    pub fn on_second_line(quiet: bool) -> Self {
        // The `testing` half is not belt and braces: `cfg!(test)` is false in
        // this crate while a *downstream* crate's tests run, and `cargo test`
        // inherits a real terminal from the shell that started it. Without it,
        // one of `remote`'s tests would paint a bar over the developer's
        // screen and leave it there.
        let usable =
            !quiet && !cfg!(any(test, feature = "testing")) && std::io::stderr().is_terminal();
        let tty = usable
            .then(|| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/tty")
                    .ok()
                    .map(Mutex::new)
            })
            .flatten();
        Self {
            theme: Theme::detect(tty.is_some()),
            tty,
            showing: Mutex::new(None),
        }
    }

    /// A bar with nowhere to draw. Every method is a no-op.
    pub fn disabled() -> Self {
        Self {
            tty: None,
            theme: Theme::detect(false),
            showing: Mutex::new(None),
        }
    }

    /// Whether there is a line to hold — and therefore whether a caller should
    /// say what it has to say here rather than by printing it.
    pub fn enabled(&self) -> bool {
        self.tty.is_some()
    }

    /// Puts `text` on the bar and leaves it there.
    pub fn show(&self, text: &str) {
        let Ok(mut showing) = self.showing.lock() else {
            return;
        };
        *showing = Some(text.to_string());
        self.paint(showing.as_deref());
    }

    /// Draws the current line again, for a caller that suspects the program
    /// underneath has painted over it. Nothing when the bar is clear.
    pub fn repaint(&self) {
        let Ok(showing) = self.showing.lock() else {
            return;
        };
        if showing.is_some() {
            self.paint(showing.as_deref());
        }
    }

    /// Takes the line off the screen. Idempotent, and safe on a bar that never
    /// showed anything.
    pub fn clear(&self) {
        let Ok(mut showing) = self.showing.lock() else {
            return;
        };
        if showing.take().is_some() {
            self.paint(None);
        }
    }

    /// The one write, called with `showing` held so that a repaint and a clear
    /// cannot interleave into a stale line.
    fn paint(&self, line: Option<&str>) {
        let Some(tty) = &self.tty else {
            return;
        };
        let painted = line.map(|line| self.theme.paint(Role::Warn, &fit(line, self.columns())));
        let out = sequence(ROW, painted.as_deref());

        if let Ok(mut tty) = tty.lock() {
            // One write, so a repaint cannot be interleaved with the terminal's
            // own output halfway through an escape sequence.
            let _ = tty.write_all(out.as_bytes());
            let _ = tty.flush();
        }
    }

    /// The terminal's real width, not the width riabuild folds prose to.
    ///
    /// [`wrap::wrap_width`] caps at 96 because prose that wide is hard to read
    /// over several lines. A bar is one line and truncating it at 96 columns of
    /// a 200-column window would throw away the end of a sentence there is
    /// plainly room for.
    fn columns(&self) -> usize {
        let fd = self
            .tty
            .as_ref()
            .and_then(|tty| tty.lock().ok())
            .map(|tty| std::os::unix::io::AsRawFd::as_raw_fd(&*tty));
        // Measured on the terminal being drawn on rather than on stdout, which
        // in a remote session may be a pipe with no width to report.
        fd.and_then(wrap::columns_of)
            .or_else(wrap::terminal_columns)
            .unwrap_or(80)
    }
}

/// The whole write, as one string.
///
/// Pure, and therefore assertable: this is the part that has to be exactly
/// right and the part no test can observe through a terminal. A missing
/// `RESTORE` moves the developer's cursor to row two and leaves it there, which
/// on a shell prompt means the next thing they type is drawn over the bar.
fn sequence(row: u16, painted: Option<&str>) -> String {
    let mut out = String::with_capacity(64);
    out.push_str(SAVE);
    // Absolute, so the bar lands on the same row whatever the cursor was doing,
    // and `\x1b[2K` rather than a row of spaces: spaces written to the last
    // column leave a pending wrap on terminals that auto-wrap, which would
    // scroll the screen the developer is reading by a line every repaint.
    out.push_str(&format!("\x1b[{row};1H\x1b[2K"));
    if let Some(painted) = painted {
        out.push_str(painted);
    }
    out.push_str(RESTORE);
    out
}

/// One line's worth of `text`, cut at a whole character.
///
/// One column is left spare on purpose. Writing the last cell of a row leaves
/// an auto-wrapping terminal with a pending wrap, and the next character
/// printed anywhere lands on a fresh line — a bar that scrolled the screen by
/// one row every fifteen seconds is worse than a bar with a word missing.
fn fit(text: &str, columns: usize) -> String {
    let room = columns.saturating_sub(1).max(1);
    if text.chars().count() <= room {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(room.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar with nowhere to draw must be usable, not merely survivable: every
    /// run except a remote session gets one, and the supervisor calls it on a
    /// path where a panic would take the developer's shell with it.
    #[test]
    fn a_disabled_bar_says_so_and_does_nothing() {
        let bar = StatusBar::disabled();
        assert!(!bar.enabled());
        bar.show("Clipboard channel — down");
        bar.repaint();
        bar.clear();
    }

    /// The cursor is put back. Without that the bar does not merely look
    /// wrong: it *moves* the developer, and the next thing typed at a prompt
    /// is drawn on row two.
    #[test]
    fn the_write_saves_the_cursor_addresses_row_two_and_puts_it_back() {
        let out = sequence(ROW, Some("Clipboard channel — down"));
        assert!(out.starts_with(SAVE), "{out:?}");
        assert!(out.ends_with(RESTORE), "{out:?}");
        assert!(out.contains("\x1b[2;1H"), "{out:?}");
        // The row is cleared, so a shorter line never leaves the tail of a
        // longer one behind it.
        assert!(out.contains("\x1b[2K"), "{out:?}");
        assert!(out.contains("Clipboard channel"), "{out:?}");
    }

    /// Clearing writes the same sequence with nothing in it, rather than
    /// nothing at all: the row has to be erased, and the cursor still has to
    /// come back from it.
    #[test]
    fn clearing_erases_the_row_and_still_restores_the_cursor() {
        let out = sequence(ROW, None);
        assert!(out.starts_with(SAVE) && out.ends_with(RESTORE), "{out:?}");
        assert!(out.contains("\x1b[2K"), "{out:?}");
        assert_eq!(out, format!("{SAVE}\x1b[2;1H\x1b[2K{RESTORE}"));
    }

    /// The bar is one line. A sentence longer than the terminal is cut rather
    /// than wrapped, because a wrapped bar is two rows and only one of them
    /// gets cleared.
    #[test]
    fn a_line_longer_than_the_terminal_is_cut_to_it() {
        let cut = fit(&"x".repeat(200), 20);
        assert_eq!(cut.chars().count(), 19, "{cut}");
        assert!(cut.ends_with('…'), "{cut}");
    }

    /// …and one that fits is left exactly as it was, ellipsis and all.
    #[test]
    fn a_line_that_fits_is_untouched() {
        assert_eq!(
            fit("Clipboard channel — down", 80),
            "Clipboard channel — down"
        );
    }

    /// Counted in characters, never bytes: the channel's own messages are full
    /// of em dashes, and cutting one in half writes a byte no terminal can
    /// render.
    #[test]
    fn a_line_is_cut_at_a_character_not_a_byte() {
        let cut = fit("————————", 5);
        assert!(cut.chars().count() <= 4, "{cut}");
        assert!(cut.is_char_boundary(cut.len()));
    }

    /// A terminal that reports nothing at all still gets a bar rather than a
    /// division by zero or a line cut to nothing.
    #[test]
    fn an_impossibly_narrow_terminal_still_gets_a_character() {
        assert!(!fit("something", 0).is_empty());
        assert!(!fit("something", 1).is_empty());
    }
}

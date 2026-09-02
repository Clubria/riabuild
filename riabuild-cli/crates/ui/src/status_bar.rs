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
//! # Two kinds of line
//!
//! Everything the channel says arrives here, and the two things it says have
//! different lifetimes:
//!
//! - **Standing** — the state of the channel. *Paste is off* stays true until
//!   something changes it, so [`show`](StatusBar::show) leaves it up until a
//!   caller takes it down.
//! - **Passing** — something the channel just did. *Opening a link on this
//!   laptop* is over in a second, and a developer who reads it a minute later
//!   learns something false. [`flash`](StatusBar::flash) and
//!   [`flash_warning`](StatusBar::flash_warning) stand for [`PASSING`] and then
//!   fall away, leaving whatever was underneath.
//!
//! A passing line cannot be a `show` followed by a `clear`, which is what makes
//! this two fields rather than one: the clear would take a standing failure off
//! the screen with it, and the developer would be left pasting into a session
//! that had stopped telling them paste was dead. Nor can it expire on a timer
//! of its own — this crate deliberately has no runtime, and everything else in
//! it is a `println!`. It expires on the next [`repaint`](StatusBar::repaint),
//! which the channel's own painter already ticks; see
//! `channel::supervisor::bar`.
//!
//! What this is not is a general printer. A bar holds one line and truncates
//! it; the folded prose, the detail and the next action all belong to the runs
//! that own their screen, and stay with `Ui`.

use crate::wrap;
use riabuild_theme::{Role, Theme};
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The row the bar is drawn on, counting from one.
///
/// Row two, not row one, and that is the whole of the placement decision: mosh
/// draws its own "[mosh] Last contact 3 seconds ago" on row one, and two
/// programs writing the same cells produce whichever repainted last. One line
/// down is out of its way and still above everything the developer is reading.
const ROW: u16 = 2;

/// How long a passing line stands before the bar falls back to what is
/// underneath it.
///
/// Long enough to be read by a developer whose eyes are on the middle of the
/// screen rather than on row two, short enough that "opening a link" has
/// stopped being a claim about now by the time it goes. A floor rather than a
/// promise: the line comes off on the first repaint after it expires, so a bar
/// ticked every couple of seconds holds it a little longer than this.
pub const PASSING: Duration = Duration::from_secs(6);

/// Save the cursor, including where it is and what it is painting with.
///
/// `ESC 7`, not `CSI s`. They do the same thing on xterm and its descendants,
/// and only this one is honoured by the terminals that predate them — which,
/// through mosh, is what the far end of a session may well be emulating.
const SAVE: &str = "\x1b7";
const RESTORE: &str = "\x1b8";

/// One line, and what it is painted as.
///
/// The glyph is carried rather than derived from the role, so that the two
/// constructors below are the whole vocabulary of the bar: a caller says what
/// kind of thing it has to say and gets riabuild's glyph for it — the same `▲`
/// and `◐` the reports in `report` use for the same two meanings.
struct Note {
    role: Role,
    glyph: &'static str,
    text: String,
}

impl Note {
    /// Something that is wrong.
    fn warning(text: &str) -> Self {
        Self {
            role: Role::Warn,
            glyph: "▲",
            text: text.to_string(),
        }
    }

    /// Something happening now.
    fn doing(text: &str) -> Self {
        Self {
            role: Role::Busy,
            glyph: "◐",
            text: text.to_string(),
        }
    }
}

/// What the bar has to say, in both of its lifetimes.
#[derive(Default)]
struct Line {
    /// The state of the channel: up until a caller takes it down.
    standing: Option<Note>,
    /// Something the channel just did, and the moment it stops being worth
    /// saying. Stands *over* `standing` rather than replacing it, which is what
    /// lets a link open in a session that is already warning about paste
    /// without either message losing the other.
    passing: Option<(Note, Instant)>,
    /// Whether the row has ink on it now.
    ///
    /// Held so that a bar with nothing to say writes nothing at all: without
    /// it, every repaint of an empty bar would erase a row nobody has written
    /// to, over and over, for the length of a session.
    inked: bool,
}

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
    /// What is on the screen now.
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
    line: Mutex<Line>,
    /// Every line this bar has put on the row, for a test to read back.
    ///
    /// `Some` only for [`recording`](Self::recording), and it is also what
    /// makes such a bar answer [`enabled`](Self::enabled): the whole of what a
    /// caller asks the bar is *is there a line to speak on, or should I print?*,
    /// and the callers that ask it — the supervisor and the channel's agent —
    /// have no terminal under `cargo test` and would otherwise only ever be
    /// tested down the branch that prints.
    #[cfg(any(test, feature = "testing"))]
    painted: Option<Mutex<Vec<String>>>,
}

impl StatusBar {
    /// The bar a remote session's channel speaks through.
    ///
    /// Disabled — every call a no-op — when there is no terminal to hold a line
    /// on, and under `--quiet`, which asks for no decoration at all. A caller
    /// that finds it disabled prints the ordinary way instead; see
    /// `supervisor::report`.
    pub fn on_second_line(quiet: bool) -> Self {
        let usable = !quiet && crate::tty::can_pin_a_line();
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
            line: Mutex::new(Line::default()),
            #[cfg(any(test, feature = "testing"))]
            painted: None,
        }
    }

    /// A bar with nowhere to draw. Every method is a no-op.
    pub fn disabled() -> Self {
        Self {
            tty: None,
            theme: Theme::detect(false),
            line: Mutex::new(Line::default()),
            #[cfg(any(test, feature = "testing"))]
            painted: None,
        }
    }

    /// A bar that answers `enabled()` and paints into memory instead of a
    /// terminal, for the tests of whatever speaks on it.
    #[cfg(any(test, feature = "testing"))]
    pub fn recording() -> Self {
        Self {
            tty: None,
            theme: Theme::detect(false),
            line: Mutex::new(Line::default()),
            painted: Some(Mutex::new(Vec::new())),
        }
    }

    /// Every line this bar has put on the row, in order and whole.
    ///
    /// Recorded before it is cut to the terminal, the way `Ui::note` records
    /// what it folds: a test asserting what the developer was told should not
    /// have to know how wide the window was.
    #[cfg(any(test, feature = "testing"))]
    pub fn painted(&self) -> Vec<String> {
        self.painted
            .as_ref()
            .and_then(|painted| painted.lock().ok())
            .map(|painted| painted.clone())
            .unwrap_or_default()
    }

    /// Whether there is a line to hold — and therefore whether a caller should
    /// say what it has to say here rather than by printing it.
    pub fn enabled(&self) -> bool {
        #[cfg(any(test, feature = "testing"))]
        if self.painted.is_some() {
            return true;
        }
        self.tty.is_some()
    }

    /// Puts `text` on the bar as the state of things, and leaves it there.
    pub fn show(&self, text: &str) {
        self.change(|line| line.standing = Some(Note::warning(text)));
    }

    /// Says that something is happening, for as long as that is still true.
    ///
    /// Over the standing line rather than instead of it, and gone again after
    /// [`PASSING`] — see the module doc for why those are two fields.
    pub fn flash(&self, text: &str) {
        self.passing(Note::doing(text));
    }

    /// The same, for something that has just gone wrong rather than something
    /// happening.
    ///
    /// Passing rather than standing because it is about one attempt and not
    /// about the channel: a link this laptop's browser refused says nothing
    /// about the next one, and a line that stayed up would go on describing a
    /// session that recovered a minute ago.
    pub fn flash_warning(&self, text: &str) {
        self.passing(Note::warning(text));
    }

    /// Draws the current line again, for a caller that suspects the program
    /// underneath has painted over it — and the tick a passing line expires on.
    /// Nothing when the bar is clear.
    pub fn repaint(&self) {
        self.change(|_| {});
    }

    /// Takes the line off the screen, both kinds of it. Idempotent, and safe on
    /// a bar that never showed anything.
    pub fn clear(&self) {
        self.change(|line| {
            line.standing = None;
            line.passing = None;
        });
    }

    fn passing(&self, note: Note) {
        self.change(|line| line.passing = Some((note, Instant::now() + PASSING)));
    }

    /// Every write goes through here: change what the bar has to say, then put
    /// the result on the screen, both under the one lock. Nothing paints
    /// outside it, which is what stops a repaint and a clear interleaving into
    /// a line that outlives the state it came from.
    fn change(&self, change: impl FnOnce(&mut Line)) {
        let Ok(mut line) = self.line.lock() else {
            return;
        };
        change(&mut line);
        match resolve(&mut line, Instant::now()) {
            Some((role, text)) => {
                #[cfg(any(test, feature = "testing"))]
                self.record(&text);
                let painted = self.theme.paint(role, &fit(&text, self.columns()));
                self.paint(Some(&painted));
                line.inked = true;
            }
            None => {
                if line.inked {
                    self.paint(None);
                    line.inked = false;
                }
            }
        }
    }

    /// Keeps what a recording bar has said, with a repaint of the line already
    /// on the row collapsed into the one it repeats — the developer sees one
    /// line either way, and a test should not have to count ticks.
    #[cfg(any(test, feature = "testing"))]
    fn record(&self, text: &str) {
        let Some(painted) = &self.painted else {
            return;
        };
        if let Ok(mut painted) = painted.lock()
            && painted.last().is_none_or(|last| last != text)
        {
            painted.push(text.to_string());
        }
    }

    /// The one write, called with the line held so that a repaint and a clear
    /// cannot interleave into a stale line.
    fn paint(&self, painted: Option<&str>) {
        let Some(tty) = &self.tty else {
            return;
        };
        let out = sequence(ROW, painted);

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

/// Retires a passing line that has had its time, and answers what belongs on
/// the row now — `None` when nothing does.
///
/// The whole of the precedence rule, in one place a test can reach: a passing
/// line stands over a standing one and then falls away leaving it, and nothing
/// about that can be read back off a terminal riabuild has written to.
///
/// Expiry happens *here*, on a repaint, rather than on a timer, because this
/// crate has no runtime to run one on. The cost is that a bar nobody repaints
/// holds its passing line; every bar with a terminal to draw on is repainted by
/// the channel's painter, and one without a terminal draws nothing anyway.
fn resolve(line: &mut Line, now: Instant) -> Option<(Role, String)> {
    if line
        .passing
        .as_ref()
        .is_some_and(|(_, until)| now >= *until)
    {
        line.passing = None;
    }
    let note = line
        .passing
        .as_ref()
        .map(|(note, _)| note)
        .or(line.standing.as_ref())?;
    Some((note.role, format!("{} {}", note.glyph, note.text)))
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
    /// run except a remote session gets one, and the supervisor and the agent
    /// both call it on paths where a panic would take the developer's shell
    /// with it.
    #[test]
    fn a_disabled_bar_says_so_and_does_nothing() {
        let bar = StatusBar::disabled();
        assert!(!bar.enabled());
        bar.show("Clipboard channel — down");
        bar.flash("opening https://github.com/login/device");
        bar.flash_warning("this laptop could not open the link");
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

    fn standing(text: &str) -> Line {
        Line {
            standing: Some(Note::warning(text)),
            ..Line::default()
        }
    }

    /// Each kind of line carries riabuild's glyph for what it means, so the bar
    /// reads as the same voice as the report the developer saw a minute
    /// earlier.
    #[test]
    fn a_standing_line_is_a_warning_and_a_passing_one_says_what_it_is() {
        let now = Instant::now();

        let mut down = standing("Clipboard channel — down");
        let (role, text) = resolve(&mut down, now).expect("a standing line");
        assert_eq!(role, Role::Warn);
        assert_eq!(text, "▲ Clipboard channel — down");

        let mut opening = Line {
            passing: Some((Note::doing("opening a link on this laptop"), now + PASSING)),
            ..Line::default()
        };
        let (role, text) = resolve(&mut opening, now).expect("a passing line");
        assert_eq!(role, Role::Busy);
        assert_eq!(text, "◐ opening a link on this laptop");
    }

    /// The whole reason a passing line is a second field. A link opening in a
    /// session that is already warning about paste must not take the warning
    /// off the screen with it: it stands over it, and then gives it back.
    #[test]
    fn a_passing_line_stands_over_the_standing_one_and_gives_it_back() {
        let now = Instant::now();
        let mut line = standing("Clipboard channel — down");
        line.passing = Some((Note::doing("opening a link"), now + PASSING));

        let (_, over) = resolve(&mut line, now).expect("the passing line");
        assert_eq!(over, "◐ opening a link");

        let (_, after) = resolve(&mut line, now + PASSING).expect("the standing line, back");
        assert_eq!(after, "▲ Clipboard channel — down");
        // Retired rather than merely outranked, so it cannot come back.
        assert!(line.passing.is_none());
    }

    /// A passing line with nothing underneath it leaves the row empty, which is
    /// the case `inked` exists for: the bar has to erase what it wrote and then
    /// stop writing.
    #[test]
    fn a_passing_line_with_nothing_under_it_leaves_the_row_empty() {
        let now = Instant::now();
        let mut line = Line {
            passing: Some((Note::warning("this laptop could not open the link"), now)),
            ..Line::default()
        };
        assert!(resolve(&mut line, now).is_none());
        assert!(line.passing.is_none());
    }
}

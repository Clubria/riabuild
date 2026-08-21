//! Turning what survived the filter into lines on the developer's terminal.
//!
//! The state this holds is one number — how many columns the last unterminated
//! line occupied — and it is the whole reason a progress line redrawn shorter
//! does not leave the tail of the longer one behind it.

use riabuild_theme::{Role, Theme};

use crate::subdue::Subdue;

/// The indent child output is printed at — `ui::note`'s, because that is what
/// a line from a child is: a note under the task that started it.
const INDENT: &str = "    ";

/// Renders subdued lines, and remembers how much of the terminal the last
/// unterminated one occupied.
pub(super) struct Painter {
    theme: Theme,
    /// Columns written by the last `partial`, still on screen. A shorter redraw
    /// has to cover them or the tail of the longer frame stays visible.
    open: usize,
    /// What that partial said, so an unchanged repaint writes nothing.
    last: String,
}

impl Painter {
    pub(super) fn new(theme: Theme) -> Self {
        Self {
            theme,
            open: 0,
            last: String::new(),
        }
    }

    /// A finished line. Ends with a newline; nothing is left open.
    pub(super) fn line(&mut self, text: &str) -> String {
        let out = self.draw(text);
        self.open = 0;
        self.last.clear();
        out + "\n"
    }

    /// The line as it currently stands, with the child still writing it.
    ///
    /// Empty when nothing has changed. The pump repaints after every read, and
    /// a child that prints `Password: ` and then waits must not have it
    /// reprinted on every wakeup.
    pub(super) fn partial(&mut self, text: &str) -> String {
        if text == self.last {
            return String::new();
        }
        let out = self.draw(text);
        self.open = text.chars().count();
        self.last = text.to_string();
        out
    }

    /// The same idiom `ui::applied` uses over a status line: return to the
    /// start, write, and pad over whatever the longer previous frame left.
    fn draw(&self, text: &str) -> String {
        let padding = " ".repeat(self.open.saturating_sub(text.chars().count()));
        format!("\r{INDENT}{}{padding}", self.theme.paint(Role::Muted, text))
    }
}

/// How wide the child is told its terminal is.
///
/// The real width less the indent, so a child that wraps at the width it was
/// given does not push every wrapped line past the right edge. Never zero: a
/// terminal of no width makes some children divide by it.
pub(super) fn child_columns(terminal: u16) -> u16 {
    terminal.saturating_sub(INDENT.len() as u16).max(1)
}

/// Runs one read's worth of bytes through the filter and paints the result.
pub(super) fn show(filter: &mut Subdue, painter: &mut Painter, bytes: &[u8]) {
    let mut out = String::new();
    for line in filter.feed(bytes) {
        out.push_str(&painter.line(&line));
    }
    if let Some(text) = filter.partial() {
        out.push_str(&painter.partial(&text));
    }
    emit(&out);
}

pub(super) fn emit(text: &str) {
    if text.is_empty() {
        return;
    }
    use std::io::Write;
    // Raw mode is on, so a bare `\n` moves down without returning to column
    // zero. Every line the painter produces starts with `\r`, which is what
    // puts it back.
    print!("{text}");
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_theme::Depth;

    #[test]
    fn a_line_is_indented_to_note_depth_and_dimmed() {
        let mut painter = Painter::new(Theme::with_depth(Depth::Ansi16));
        assert_eq!(painter.line("Unpacking"), "\r    \x1b[2mUnpacking\x1b[0m\n");
    }

    #[test]
    fn a_plain_theme_still_indents_and_still_ends_the_line() {
        let mut painter = Painter::new(Theme::plain());
        assert_eq!(painter.line("Unpacking"), "\r    Unpacking\n");
    }

    #[test]
    fn a_partial_line_is_written_without_ending_it() {
        let mut painter = Painter::new(Theme::plain());
        assert_eq!(painter.partial("Password: "), "\r    Password: ");
    }

    #[test]
    fn a_redraw_covers_what_the_longer_frame_left_behind() {
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Reading database... 45%");
        assert_eq!(painter.partial("Done"), "\r    Done                   ");
    }

    #[test]
    fn a_line_after_a_partial_covers_it_too() {
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Progress: 100%");
        assert_eq!(painter.line("Done"), "\r    Done          \n");
    }

    #[test]
    fn repainting_the_same_partial_writes_nothing() {
        // The pump repaints after every read. A child that writes a prompt and
        // then waits must not have it reprinted on each wakeup.
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Password: ");
        assert_eq!(painter.partial("Password: "), "");
    }

    #[test]
    fn a_finished_line_stops_covering_for_the_next_one() {
        // `line` clears the open width; otherwise the padding from a long
        // progress bar would be re-applied to every line after it.
        let mut painter = Painter::new(Theme::plain());
        painter.partial("a very long progress line");
        painter.line("short");
        assert_eq!(painter.line("also short"), "\r    also short\n");
    }

    #[test]
    fn the_child_gets_the_terminal_width_less_the_indent() {
        // Otherwise the child wraps at the full width and the indent pushes
        // every wrapped line four columns past the right edge.
        assert_eq!(child_columns(80), 76);
        // Never zero, whatever the terminal claims.
        assert_eq!(child_columns(4), 1);
        assert_eq!(child_columns(0), 1);
    }
}

//! Fitting riabuild's own prose to the terminal it lands on.
//!
//! Long messages used to be laid out at the call site — a `\n` and four spaces
//! inside a format string. That fixes the *indent* and leaves the *wrapping* to
//! the terminal, which folds at column 0: the second half of a sentence lands
//! under the mark rather than under the text, and a block stops looking like a
//! block. Worse, the width baked into the sentence is whatever the author's
//! editor was set to, which is not the width anyone reads it at.
//!
//! So a call site writes paragraphs and this file decides where the lines
//! break. Everything here is pure but [`terminal_columns`], which is the one
//! measurement, taken once — the rest can be asserted without owning a
//! terminal, the same split as `theme::depth_for` and `ui::cover`.

use riabuild_theme::{Role, Theme};

/// Columns of text under a task line's mark: `  ▲ ` and `    ` are both four.
///
/// One constant rather than two because they must stay equal — folded prose
/// that lines up under the first word is the entire point, and a hanging
/// indent that drifts from the mark's width is how it stops.
pub const INDENT: &str = "    ";

/// One step further in, for a line the developer copies rather than reads.
pub const VERBATIM_INDENT: &str = "      ";

/// Columns the terminal reports, or `None` when stdout is not one.
///
/// Measured on **stdout**, which is what `Theme::detect` also asks about, so a
/// run whose output is a pipe gets no colour and no measurement and the two
/// fall back together. A pipe has no width to report, and inventing one from
/// `COLUMNS` would take a number the developer's *shell* set for its own
/// window and apply it to a file.
pub fn terminal_columns() -> Option<usize> {
    // SAFETY: `winsize` is plain data with no invalid bit patterns, and the
    // ioctl either fills it or leaves it as the zeroes it was created with —
    // which the `> 0` below rejects.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } != 0 {
        return None;
    }
    (size.ws_col > 0).then_some(size.ws_col as usize)
}

/// The width riabuild folds prose to, given what the terminal said.
///
/// Both bounds are deliberate. A terminal that reports 300 columns is a
/// maximised window, not a request for 300-column paragraphs — prose that wide
/// is measurably harder to read, and a warning is the last place to make the
/// developer's eye track that far back. A terminal narrower than the floor gets
/// folded lines the terminal then folds again, which is ugly but readable;
/// folding to its true width instead would put one word on some lines.
pub fn wrap_width(reported: Option<usize>) -> usize {
    /// What a terminal that will not say is assumed to be. The width every
    /// terminal emulator opens at, and the one a pipe is read at.
    const ASSUMED: usize = 80;
    const NARROWEST: usize = 32;
    const WIDEST: usize = 96;
    reported.unwrap_or(ASSUMED).clamp(NARROWEST, WIDEST)
}

/// `text` as lines of at most `width` columns, without indentation.
///
/// Greedy, and it never breaks a word: an SSH public key, a path, or a URL
/// comes back on a line of its own, over-long, rather than in two halves
/// neither of which can be copied. That is the property that matters — riabuild
/// prints keys and paths into exactly these messages.
///
/// Columns are counted in `char`s, as [`crate::cover`] does. riabuild's own
/// prose is Latin text with a handful of box glyphs, so the places where a
/// `char` is not a column are the places nothing here prints.
pub fn fold(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let joined = line.chars().count() + 1 + word.chars().count();
        if !line.is_empty() && joined > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// One paragraph of the explanation under a warning or a failure.
///
/// The distinction is the caller's to make and cannot be inferred from the
/// text. "A line with no spaces in it" was the obvious rule and is wrong on the
/// first thing riabuild prints this way: an SSH public key is `ssh-ed25519`, a
/// base64 blob and a comment — three words, and folding between any two of them
/// gives the developer something that is not a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail<'a> {
    /// A sentence. Folded to the terminal and dimmed, like every other note.
    Prose(&'a str),
    /// Something the developer copies rather than reads: a key, a path, a
    /// command. Printed whole and emphasised, one step further in.
    ///
    /// All three properties are load-bearing, and they are the same argument
    /// [`crate::value_line`] makes about a device code. Folded, it is no longer
    /// the value; dimmed, the one line on the screen that has to be read
    /// exactly is the least legible thing on it; and at the prose indent it
    /// reads as the end of the sentence above rather than as a block.
    Verbatim(&'a str),
}

impl Detail<'_> {
    /// The text, whichever it is — for the recorders a test asserts on, which
    /// care what the developer was told and not how it was set.
    pub fn text(&self) -> &str {
        match self {
            Detail::Prose(text) | Detail::Verbatim(text) => text.trim(),
        }
    }
}

/// The explanation printed under a warning or a failure.
pub fn detail_lines(theme: Theme, width: usize, paragraphs: &[Detail]) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in paragraphs {
        let text = paragraph.text();
        if text.is_empty() {
            continue;
        }
        match paragraph {
            Detail::Verbatim(_) => lines.push(format!(
                "{VERBATIM_INDENT}{}",
                theme.paint(Role::Strong, text)
            )),
            Detail::Prose(_) => {
                for line in fold(text, width.saturating_sub(INDENT.len())) {
                    lines.push(format!("{INDENT}{}", theme.paint(Role::Muted, &line)));
                }
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_theme::Depth;

    #[test]
    fn prose_is_folded_at_the_last_word_that_fits() {
        assert_eq!(
            fold("the quick brown fox jumps over the lazy dog", 20),
            vec!["the quick brown fox", "jumps over the lazy", "dog"]
        );
    }

    #[test]
    fn a_word_that_fits_exactly_is_not_pushed_down() {
        // The off-by-one that makes a fold look one word narrower than it is:
        // "one two" is 7 columns and belongs on a 7-column line.
        assert_eq!(fold("one two", 7), vec!["one two"]);
        assert_eq!(fold("one two", 6), vec!["one", "two"]);
    }

    #[test]
    fn a_public_key_is_never_broken_in_half() {
        // The property the whole file exists for. Half a key is not a shorter
        // key, it is nothing — and this is the message riabuild prints keys in.
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDQMfwG+m0AkDbU6a0vxE5ktTNTso5LskpebOKYF2VHP riabuild";
        let folded = fold(key, 40);
        assert!(
            folded.iter().any(|line| line
                == "AAAAC3NzaC1lZDI1NTE5AAAAIDQMfwG+m0AkDbU6a0vxE5ktTNTso5LskpebOKYF2VHP"),
            "the key body must survive whole: {folded:?}"
        );
    }

    #[test]
    fn nothing_folds_to_nothing() {
        assert!(fold("", 40).is_empty());
        assert!(fold("   \n  ", 40).is_empty());
    }

    #[test]
    fn a_terminal_that_will_not_say_is_assumed_to_be_eighty() {
        assert_eq!(wrap_width(None), 80);
    }

    #[test]
    fn a_maximised_window_does_not_get_maximised_paragraphs() {
        assert_eq!(wrap_width(Some(300)), 96);
        assert_eq!(wrap_width(Some(100)), 96);
        assert_eq!(wrap_width(Some(72)), 72);
    }

    #[test]
    fn a_very_narrow_terminal_does_not_get_one_word_per_line() {
        assert_eq!(wrap_width(Some(10)), 32);
        assert_eq!(wrap_width(Some(0)), 32);
    }

    #[test]
    fn detail_is_dimmed_and_indented_under_the_mark() {
        // 30 columns of terminal, four of which are the indent: the fold
        // budget is the width the *text* gets, not the width of the screen.
        let lines = detail_lines(
            Theme::with_depth(Depth::TrueColor),
            30,
            &[Detail::Prose("the quick brown fox jumps over it")],
        );
        assert_eq!(
            lines,
            vec![
                "    \x1b[2mthe quick brown fox jumps\x1b[0m",
                "    \x1b[2mover it\x1b[0m"
            ]
        );
    }

    #[test]
    fn a_line_to_copy_is_neither_dimmed_nor_folded() {
        // A key printed dim, folded, and at the same indent as the prose above
        // it is three separate ways of hiding the one line that has to be read
        // exactly — the same argument `note_value` makes about a device code.
        // Note the spaces in it: this is why the two kinds are told apart by
        // the caller and never by looking at the text.
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDQMfwG riabuild";
        let lines = detail_lines(
            Theme::with_depth(Depth::TrueColor),
            40,
            &[Detail::Prose("Add this:"), Detail::Verbatim(key)],
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], format!("      \x1b[1m{key}\x1b[0m"));
    }

    #[test]
    fn detail_survives_being_nothing_but_its_words() {
        // NO_COLOR, a pipe, a CI log.
        let lines = detail_lines(
            Theme::plain(),
            40,
            &[
                Detail::Prose("one two three"),
                Detail::Prose(""),
                Detail::Verbatim("  four  "),
            ],
        );
        assert_eq!(lines, vec!["    one two three", "      four"]);
        assert!(!lines.iter().any(|line| line.contains('\x1b')));
    }
}

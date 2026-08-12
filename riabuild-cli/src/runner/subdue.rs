//! What a subdued child is allowed to say.
//!
//! A pty hands back whatever the child drew: colour, cursor motion, an
//! alternate screen, a window title. riabuild prints a page it chose, and a
//! third-party program is not a co-author of it — so everything a child draws
//! *with* is dropped here, and only the text it drew survives.
//!
//! No terminal, no theme, no IO. Bytes in, lines out, which is what lets the
//! whole of the line discipline be tested against canned `apt` and `gh`
//! transcripts rather than against a machine in a particular state.
//!
//! Within a single line this is a small terminal emulator rather than a
//! stripper, and it has to be. `\r` rewinds without clearing, so a spinner that
//! rewrites only its first character stays legible; a program that means to
//! *shorten* a line says so with `ESC[K`, which is honoured. Dropping the erase
//! and truncating on the rewind instead would turn `- Downloading` into `\`.

/// Where the escape parser is between bytes.
///
/// A read can end anywhere, including halfway through a sequence, so this is
/// state rather than a loop inside `feed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    Text,
    /// Seen `ESC`, waiting to learn what kind of sequence this is.
    Escape,
    /// Inside `ESC [ … final`, where final is 0x40..=0x7e.
    Csi,
    /// Inside a string sequence — OSC, DCS, APC, PM — until `BEL` or `ST`.
    String,
    /// Inside a string sequence, having just seen `ESC`: `\` makes it `ST`.
    StringEscape,
}

/// The most parameter bytes of a CSI that are kept.
///
/// The cap exists so a child that emits an unterminated sequence with a
/// megabyte of digits in it cannot make riabuild hold the lot.
const MAX_PARAMS: usize = 16;

/// How far right a child may move within its line.
///
/// `ESC[999999999C` is one byte sequence and a gigabyte of spaces if the column
/// is taken at face value. No terminal is this wide and no line riabuild prints
/// is either.
const MAX_COLUMNS: usize = 4096;

pub(super) struct Subdue {
    /// The line being assembled. Bytes rather than a `String` because a read
    /// can split a multi-byte character and because rewrites are positional.
    line: Vec<u8>,
    /// Where the next byte lands. `\r` moves it to 0 without clearing, which
    /// is what makes a redraw overwrite rather than append.
    column: usize,
    state: Scan,
    /// Parameter bytes of the CSI currently being parsed.
    params: Vec<u8>,
}

impl Subdue {
    pub(super) fn new() -> Self {
        Self {
            line: Vec::new(),
            column: 0,
            state: Scan::Text,
            params: Vec::new(),
        }
    }

    /// Bytes in, completed lines out. A line completes only on `\n`.
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut done = Vec::new();
        for &byte in bytes {
            match self.state {
                Scan::Text => {
                    if let Some(line) = self.text(byte) {
                        done.push(line);
                    }
                }
                Scan::Escape => {
                    self.state = Self::after_escape(byte);
                    self.params.clear();
                }
                // A CSI ends on its first byte in the final range; everything
                // before that is parameters and intermediates.
                Scan::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.csi(byte);
                        self.state = Scan::Text;
                    } else if self.params.len() < MAX_PARAMS {
                        self.params.push(byte);
                    }
                }
                Scan::String => match byte {
                    0x07 => self.state = Scan::Text,
                    0x1b => self.state = Scan::StringEscape,
                    _ => {}
                },
                Scan::StringEscape => {
                    self.state = if byte == b'\\' {
                        Scan::Text
                    } else {
                        Scan::String
                    }
                }
            }
        }
        done
    }

    /// The unterminated line as it currently stands, if it has any content.
    ///
    /// Untrimmed, unlike a completed line: `[sudo] password for ilya: ` ends in
    /// a space the developer is about to type after, and trimming it would put
    /// the cursor against the colon.
    pub(super) fn partial(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.line).into_owned();
        (!text.trim().is_empty()).then_some(text)
    }

    /// One byte outside any escape sequence.
    fn text(&mut self, byte: u8) -> Option<String> {
        match byte {
            b'\n' => {
                // Trailing space is invisible on a finished line, and erasing
                // to the end of one leaves a run of it behind.
                let line = String::from_utf8_lossy(&self.line).trim_end().to_string();
                self.line.clear();
                self.column = 0;
                Some(line)
            }
            // Rewind, do not clear. See the module doc.
            b'\r' => {
                self.column = 0;
                None
            }
            0x08 => {
                self.column = self.column.saturating_sub(1);
                None
            }
            0x1b => {
                self.state = Scan::Escape;
                None
            }
            // Every other C0 control — bell, NUL, vertical tab — is a thing a
            // terminal does, not a thing the child said. Tab is content.
            byte if byte < 0x20 && byte != b'\t' => None,
            byte => {
                // The column can be past the end after an erase, and the gap
                // between is blank rather than absent.
                if self.column > self.line.len() {
                    self.line.resize(self.column, b' ');
                }
                if self.column < self.line.len() {
                    self.line[self.column] = byte;
                } else {
                    self.line.push(byte);
                }
                self.column += 1;
                None
            }
        }
    }

    /// A finished CSI.
    ///
    /// Only the ones that move within the current line, or erase part of it,
    /// change anything riabuild prints. Everything else — vertical motion, the
    /// alternate screen, colour, scroll regions, erase-in-*display* — is
    /// dropped, and that is the guarantee the mode exists for: a subdued child
    /// gets one line at a time and no way to reach past it.
    ///
    /// Horizontal motion is honoured rather than dropped for the same reason
    /// `\r` is. A program that backs up four columns and rewrites is redrawing,
    /// not decorating, and a filter that ignored the motion would append the
    /// redraw to the line it was meant to replace.
    fn csi(&mut self, final_byte: u8) {
        // `ESC[?…` is a private mode — the alternate screen, bracketed paste,
        // cursor visibility. None of it is line content.
        if self.params.first() == Some(&b'?') {
            return;
        }
        match final_byte {
            b'K' => match self.params.as_slice() {
                // Erase to end of line. How a program that means to shorten a
                // line actually says so.
                b"" | b"0" => self.line.truncate(self.column.min(self.line.len())),
                // Erase to start of line, inclusive of the cursor.
                b"1" => {
                    let end = (self.column + 1).min(self.line.len());
                    self.line[..end].fill(b' ');
                }
                b"2" => self.line.fill(b' '),
                _ => {}
            },
            // CUB, CUF: left and right within the line.
            b'D' => self.column = self.column.saturating_sub(self.number(1)),
            b'C' => self.column = (self.column + self.number(1)).min(MAX_COLUMNS),
            // CHA, HPA: an absolute column, counted from one.
            b'G' | b'`' => self.column = self.number(1).saturating_sub(1).min(MAX_COLUMNS),
            _ => {}
        }
    }

    /// The first numeric parameter, or `default` when it is absent or zero —
    /// which is how every terminal reads an omitted count.
    fn number(&self, default: usize) -> usize {
        let text = String::from_utf8_lossy(&self.params);
        match text.split(';').next().unwrap_or("").parse::<usize>() {
            Ok(0) | Err(_) => default,
            Ok(number) => number,
        }
    }

    /// What `ESC` turned out to introduce.
    fn after_escape(byte: u8) -> Scan {
        match byte {
            b'[' => Scan::Csi,
            // OSC, DCS, SOS, PM, APC: all run until BEL or ST.
            b']' | b'P' | b'X' | b'^' | b'_' => Scan::String,
            // Anything else is a two-byte sequence, already consumed.
            _ => Scan::Text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8]) -> Vec<String> {
        Subdue::new().feed(input)
    }

    #[test]
    fn colour_is_removed_and_the_words_survive() {
        assert_eq!(
            lines(b"\x1b[32mUnpacking riabuild\x1b[0m\n"),
            vec!["Unpacking riabuild"]
        );
    }

    #[test]
    fn a_progress_bar_collapses_to_its_final_frame() {
        // apt rewrites one line over and over. Fifty redraws are one line's
        // worth of information, and the information is the last frame.
        let out = lines(b"Progress: 20%\rProgress: 60%\rProgress: 100%\n");
        assert_eq!(out, vec!["Progress: 100%"]);
    }

    #[test]
    fn a_bare_rewind_overwrites_in_place_the_way_a_terminal_would() {
        // No erase, so the tail of the longer frame stays — which is what the
        // developer would have seen unfiltered, and what keeps a spinner that
        // rewrites only its first character legible.
        assert_eq!(lines(b"- Downloading\r\\\n"), vec!["\\ Downloading"]);
    }

    #[test]
    fn an_erase_to_end_of_line_is_how_a_line_actually_shortens() {
        assert_eq!(
            lines(b"Reading database... 45%\rDone\x1b[K\n"),
            vec!["Done"]
        );
    }

    #[test]
    fn an_erase_of_the_whole_line_empties_it() {
        assert_eq!(lines(b"stale\x1b[2K\rfresh\n"), vec!["fresh"]);
    }

    #[test]
    fn an_erase_to_the_start_of_the_line_blanks_what_it_covers() {
        // Cursor after "abc", erase to start, then "Z" at the same column.
        assert_eq!(lines(b"abcdef\x1b[3D\x1b[1KZ\n"), vec!["   Zef"]);
    }

    #[test]
    fn a_prompt_with_no_newline_is_available_before_it_is_answered() {
        // sudo writes this and blocks. A filter that waited for `\n` would
        // show the developer a terminal that had gone silent.
        let mut subdue = Subdue::new();
        assert_eq!(
            subdue.feed(b"[sudo] password for ilya: "),
            Vec::<String>::new()
        );
        assert_eq!(
            subdue.partial().as_deref(),
            Some("[sudo] password for ilya: ")
        );
    }

    #[test]
    fn a_partial_line_continues_rather_than_repeating() {
        let mut subdue = Subdue::new();
        subdue.feed(b"Fetching ");
        assert_eq!(subdue.feed(b"riabuild\n"), vec!["Fetching riabuild"]);
        assert_eq!(subdue.partial(), None);
    }

    #[test]
    fn a_window_title_cannot_be_set() {
        // OSC 0. Left through, the child renames the developer's terminal and
        // leaves it renamed after riabuild exits.
        assert_eq!(lines(b"\x1b]0;apt-get\x07installing\n"), vec!["installing"]);
    }

    #[test]
    fn an_osc_terminated_by_st_is_also_dropped() {
        assert_eq!(lines(b"\x1b]2;title\x1b\\kept\n"), vec!["kept"]);
    }

    #[test]
    fn the_alternate_screen_cannot_be_entered() {
        assert_eq!(lines(b"\x1b[?1049hhello\x1b[?1049l\n"), vec!["hello"]);
    }

    #[test]
    fn cursor_motion_is_dropped_without_eating_the_text_around_it() {
        assert_eq!(lines(b"a\x1b[2Ab\x1b[Kc\n"), vec!["abc"]);
    }

    #[test]
    fn an_escape_split_across_two_reads_is_still_one_escape() {
        let mut subdue = Subdue::new();
        assert_eq!(subdue.feed(b"one\x1b["), Vec::<String>::new());
        assert_eq!(subdue.feed(b"32mtwo\n"), vec!["onetwo"]);
    }

    #[test]
    fn an_unterminated_sequence_cannot_grow_without_bound() {
        // A child that emits `ESC[` and then a megabyte of digits must not make
        // riabuild hold the megabyte.
        let mut subdue = Subdue::new();
        subdue.feed(b"\x1b[");
        subdue.feed(&vec![b'1'; 4096]);
        assert!(subdue.params.len() <= MAX_PARAMS);
    }

    #[test]
    fn backspace_moves_the_cursor_without_erasing() {
        // What a terminal does. The erase is the *idiom* below, not the byte.
        assert_eq!(lines(b"abcd\x08\x08X\n"), vec!["abXd"]);
    }

    #[test]
    fn the_backspace_space_backspace_idiom_erases() {
        // How a program actually rubs out the character it just printed.
        assert_eq!(lines(b"abcd\x08 \x08\n"), vec!["abc"]);
    }

    #[test]
    fn a_redraw_that_backs_up_overwrites_rather_than_appending() {
        // `ESC[4D` then four characters is a program redrawing the tail of its
        // own line. Dropping the motion would give "Fetching 45%100%".
        assert_eq!(lines(b"Fetching  45%\x1b[4D100%\n"), vec!["Fetching 100%"]);
    }

    #[test]
    fn an_absolute_column_is_counted_from_one() {
        assert_eq!(lines(b"abcdef\x1b[3GZ\n"), vec!["abZdef"]);
    }

    #[test]
    fn moving_right_past_the_end_pads_rather_than_appends() {
        assert_eq!(lines(b"ab\x1b[3CZ\n"), vec!["ab   Z"]);
    }

    #[test]
    fn a_child_cannot_move_a_gigabyte_to_the_right() {
        // One byte sequence, and a gigabyte of spaces if taken at face value.
        let out = lines(b"\x1b[999999999CZ\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), MAX_COLUMNS + 1);
    }

    #[test]
    fn a_bell_is_not_content() {
        assert_eq!(lines(b"done\x07\n"), vec!["done"]);
    }

    #[test]
    fn carriage_returns_do_not_emit_empty_lines() {
        // `\r\n` is one line ending, not a rewind followed by a blank line.
        assert_eq!(lines(b"first\r\nsecond\r\n"), vec!["first", "second"]);
    }

    #[test]
    fn a_finished_line_loses_its_trailing_space() {
        assert_eq!(lines(b"padded   \n"), vec!["padded"]);
    }

    #[test]
    fn a_line_that_is_only_whitespace_is_not_a_partial() {
        let mut subdue = Subdue::new();
        subdue.feed(b"   ");
        assert_eq!(subdue.partial(), None);
    }

    #[test]
    fn the_bytes_a_real_pty_delivers() {
        // Captured with `xxd` from a `/bin/sh` running under `script`: an OSC
        // title, a colour, the `\r\r\n` a pty's ONLCR produces, and a progress
        // rewrite. Written down because every other test here is a hand-built
        // guess at what a child emits, and this one is not.
        let out = lines(
            b"\x1b]0;stolen\x07\x1b[32mworking\x1b[0m\r\r\nProgress: 20%\rProgress: 100%\r\n",
        );
        assert_eq!(out, vec!["working", "Progress: 100%"]);
    }

    #[test]
    fn invalid_utf8_does_not_lose_the_line() {
        assert_eq!(lines(b"caf\xff\n"), vec!["caf\u{fffd}"]);
    }
}

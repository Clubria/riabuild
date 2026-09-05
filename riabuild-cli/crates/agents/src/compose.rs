//! What a developer types into, and where the caret is in it.
//!
//! Its own type because the caret is the whole of what makes the session pane
//! keep the keyboard. `←` at character 0 is what hands it back to the session
//! list, and every other `←` moves inside the text — so "where is the caret"
//! is not a rendering detail, it is the condition the keymap branches on.
//!
//! Indices are **characters**, never bytes. A developer typing an em-dash into
//! a prompt would otherwise split a `String` mid-codepoint, which is a panic
//! rather than a wrong caret.
//!
//! # It holds a prompt, not a line
//!
//! A prompt is prose, and prose is longer than a terminal is wide. This used to
//! be one line that ran off the right edge of the pane and took the caret with
//! it, so the half of a paragraph a developer had just written was somewhere
//! they could not see. It wraps now — [`Compose::wrap`] is the whole of that,
//! and it is here rather than in the renderer because the caret has to land on
//! the same row the text does.
//!
//! Wrapping keeps every row a **contiguous slice** of the text, breaking after
//! the space rather than swallowing it. That is what makes the caret's row and
//! column arithmetic instead of a search: every character index belongs to
//! exactly one row. A wrap that dropped the space at the break would leave the
//! caret with two rows it could plausibly be on and no way to choose.
//!
//! Newlines are ordinary characters in the text, so a prompt with a blank line
//! in it round-trips through [`Compose::take`] unchanged — a shell command and
//! the sentence about it stay two lines all the way to the harness.

/// Where one wrapped row starts and ends, in characters from the start of the
/// whole text.
///
/// Half-open, and contiguous with its neighbours: `rows[n].end ==
/// rows[n + 1].start` always, which is what lets a caret at a break belong to
/// the row after it without a special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

/// The text, wrapped, and where the caret landed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    pub rows: Vec<String>,
    /// The row the caret is on, and how many characters into it.
    pub caret: (usize, usize),
}

/// A prompt, and a caret in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compose {
    text: String,
    /// In characters from the start.
    caret: usize,
}

impl Compose {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Whether `←` would leave the text rather than move inside it.
    ///
    /// The keymap asks this rather than comparing the caret to zero itself, so
    /// that the one gesture which changes focus is spelled once.
    pub fn at_start(&self) -> bool {
        self.caret == 0
    }

    pub fn insert(&mut self, ch: char) {
        let at = self.byte_of(self.caret);
        self.text.insert(at, ch);
        self.caret += 1;
    }

    /// Deletes the character before the caret.
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let at = self.byte_of(self.caret - 1);
        self.text.remove(at);
        self.caret -= 1;
    }

    /// Deletes the character under the caret.
    pub fn delete(&mut self) {
        let at = self.byte_of(self.caret);
        if at < self.text.len() {
            self.text.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.len());
    }

    /// The start of the whole prompt.
    pub fn start(&mut self) {
        self.caret = 0;
    }

    /// The end of the whole prompt.
    pub fn end(&mut self) {
        self.caret = self.len();
    }

    /// The start of the line the caret is on.
    ///
    /// The *logical* line, bounded by newlines the developer typed — never the
    /// wrapped row. A caret sent to the start of a row it only landed on
    /// because the pane is narrow would move somewhere else the moment the
    /// window was resized, which is not a place anybody asked to go.
    pub fn line_start(&mut self) {
        let chars = self.chars();
        let mut at = self.caret;
        while at > 0 && chars[at - 1] != '\n' {
            at -= 1;
        }
        self.caret = at;
    }

    /// The end of the line the caret is on.
    pub fn line_end(&mut self) {
        let chars = self.chars();
        let mut at = self.caret;
        while at < chars.len() && chars[at] != '\n' {
            at += 1;
        }
        self.caret = at;
    }

    /// Back to the start of the word behind the caret.
    ///
    /// Whitespace first and then the word, which is what puts a caret sitting
    /// after `slow ` at the start of `slow` rather than one character short of
    /// where it already was.
    pub fn word_left(&mut self) {
        self.caret = self.word_boundary_left();
    }

    /// Forward to the end of the word ahead of the caret.
    pub fn word_right(&mut self) {
        let chars = self.chars();
        let mut at = self.caret;
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        while at < chars.len() && !chars[at].is_whitespace() {
            at += 1;
        }
        self.caret = at;
    }

    /// Deletes the word behind the caret.
    ///
    /// The same boundary [`Compose::word_left`] moves to, so the two gestures
    /// agree about what a word is — a developer who holds one down and then
    /// reaches for the other should not find them disagreeing by a space.
    pub fn delete_word_left(&mut self) {
        let to = self.word_boundary_left();
        self.cut(to, self.caret);
    }

    /// Deletes from the caret back to the start of its line.
    pub fn delete_to_line_start(&mut self) {
        let was = self.caret;
        self.line_start();
        let to = self.caret;
        self.caret = was;
        self.cut(to, was);
    }

    /// Empties the box and hands back what was in it, trimmed.
    ///
    /// One operation rather than a read and a clear, because a prompt that was
    /// sent and a box that was not emptied is the next prompt going out with
    /// the last one still in front of it.
    pub fn take(&mut self) -> String {
        let sent = self.text.trim().to_string();
        self.text.clear();
        self.caret = 0;
        sent
    }

    /// The text laid out in a box `width` columns wide, and the caret's place
    /// in it.
    ///
    /// Always at least one row, because an empty box still has a caret to draw.
    pub fn wrap(&self, width: usize) -> Wrapped {
        let chars = self.chars();
        let rows = self.rows(width);
        // The last row that starts at or before the caret. Rows are contiguous,
        // so a caret at a break belongs to the row after it and needs no
        // special case; `position` from the far end is the whole of that.
        let at = rows
            .iter()
            .rposition(|row| row.start <= self.caret)
            .unwrap_or(0);
        let row = rows.get(at).copied().unwrap_or(Row { start: 0, end: 0 });
        let column = self.caret.saturating_sub(row.start);
        Wrapped {
            rows: rows
                .iter()
                .map(|row| {
                    chars
                        .get(row.start..row.end)
                        .unwrap_or_default()
                        .iter()
                        .collect()
                })
                .collect(),
            caret: (at, column),
        }
    }

    /// How many rows a box `width` wide needs to show all of this.
    ///
    /// Asked by the layout before the lines are built, because the box's height
    /// is what the transcript above it has to give up.
    pub fn height(&self, width: usize) -> usize {
        self.rows(width).len()
    }

    /// Where the rows fall, without building their text.
    fn rows(&self, width: usize) -> Vec<Row> {
        // A pane one column wide is a terminal nobody can use, but it is still
        // one this must not divide by.
        let width = width.max(1);
        let chars = self.chars();
        let mut rows = Vec::new();
        let mut line = 0;
        // Lines first: a newline the developer typed is a break the wrap may
        // not undo. The newline itself is then handed to the row that ends the
        // line, which keeps rows contiguous — it is simply never drawn.
        for (index, ch) in chars.iter().enumerate() {
            if *ch == '\n' {
                wrap_between(&chars, line, index, width, &mut rows);
                if let Some(last) = rows.last_mut() {
                    last.end += 1;
                }
                line = index + 1;
            }
        }
        wrap_between(&chars, line, chars.len(), width, &mut rows);
        rows
    }

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    /// Where the word behind the caret begins.
    fn word_boundary_left(&self) -> usize {
        let chars = self.chars();
        let mut at = self.caret;
        while at > 0 && chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !chars[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }

    /// Removes `from..to` in characters and leaves the caret where the cut was.
    fn cut(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let (start, end) = (self.byte_of(from), self.byte_of(to));
        self.text.replace_range(start..end, "");
        self.caret = from;
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// The byte offset of a character index, clamped to the end.
    fn byte_of(&self, index: usize) -> usize {
        self.text
            .char_indices()
            .nth(index)
            .map(|(at, _)| at)
            .unwrap_or(self.text.len())
    }
}

/// Breaks one logical line into rows no wider than `width`.
///
/// Greedy, and it breaks **after** the space rather than dropping it, so the
/// rows it emits are contiguous slices of the text. That is what the caret's
/// arithmetic depends on. A word longer than the box — a path, a URL — is cut
/// at the edge, because the alternative is a row wider than the pane.
fn wrap_between(chars: &[char], start: usize, end: usize, width: usize, rows: &mut Vec<Row>) {
    let mut at = start;
    loop {
        if end.saturating_sub(at) <= width {
            rows.push(Row { start: at, end });
            return;
        }
        // The last space inside the window, and the break falls after it. No
        // space at all is one word wider than the box — a path, a URL — which
        // is cut at the edge, because the alternative is a row wider than the
        // pane and ratatui draws that by simply stopping.
        let cut = (at..at + width)
            .rev()
            .find(|index| chars.get(*index).is_some_and(|ch| *ch == ' '))
            .map(|index| index + 1)
            .unwrap_or(at + width);
        rows.push(Row {
            start: at,
            end: cut,
        });
        at = cut;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Compose {
        let mut compose = Compose::default();
        for ch in text.chars() {
            compose.insert(ch);
        }
        compose
    }

    #[test]
    fn typing_leaves_the_caret_after_what_was_typed() {
        let compose = typed("hello");
        assert_eq!(compose.text(), "hello");
        assert_eq!(compose.caret(), 5);
        assert!(!compose.at_start());
    }

    #[test]
    fn the_caret_moves_inside_the_text_and_stops_at_both_ends() {
        let mut compose = typed("abc");
        compose.right();
        assert_eq!(compose.caret(), 3);
        for _ in 0..5 {
            compose.left();
        }
        assert_eq!(compose.caret(), 0);
        assert!(compose.at_start());
        compose.end();
        assert_eq!(compose.caret(), 3);
        compose.start();
        assert_eq!(compose.caret(), 0);
    }

    #[test]
    fn a_character_typed_mid_line_lands_at_the_caret() {
        let mut compose = typed("ac");
        compose.left();
        compose.insert('b');
        assert_eq!(compose.text(), "abc");
        assert_eq!(compose.caret(), 2);
    }

    #[test]
    fn backspace_takes_what_is_behind_and_delete_takes_what_is_under() {
        let mut compose = typed("abcd");
        compose.left();
        compose.backspace();
        assert_eq!(compose.text(), "abd");
        compose.delete();
        assert_eq!(compose.text(), "ab");
        // and neither runs off its end
        compose.start();
        compose.backspace();
        assert_eq!(compose.text(), "ab");
        compose.end();
        compose.delete();
        assert_eq!(compose.text(), "ab");
    }

    #[test]
    fn a_multibyte_character_is_one_step_and_never_a_split_codepoint() {
        // The panic this stops: `String::insert` at a byte offset that lands
        // inside a character. An em-dash in a prompt is enough to reach it.
        let mut compose = typed("a—b");
        assert_eq!(compose.caret(), 3);
        compose.left();
        compose.backspace();
        assert_eq!(compose.text(), "ab");
        assert_eq!(compose.caret(), 1);
    }

    #[test]
    fn sending_empties_the_line_so_the_next_prompt_starts_clean() {
        let mut compose = typed("  ship it  ");
        assert_eq!(compose.take(), "ship it");
        assert_eq!(compose.text(), "");
        assert_eq!(compose.caret(), 0);
        assert!(compose.is_empty());
    }

    #[test]
    fn a_word_jump_crosses_the_space_and_stops_at_the_word() {
        // Ctrl-← and Ctrl-→. The space is crossed on the way, which is what
        // puts a caret after "slow " at the start of "slow" rather than one
        // character short of where it already was.
        let mut compose = typed("why is the job slow");
        compose.word_left();
        assert_eq!(compose.caret(), 15);
        compose.word_left();
        assert_eq!(compose.caret(), 11);
        compose.start();
        compose.word_right();
        assert_eq!(compose.caret(), 3);
        // and neither runs off its end
        for _ in 0..20 {
            compose.word_right();
        }
        assert_eq!(compose.caret(), 19);
        for _ in 0..20 {
            compose.word_left();
        }
        assert_eq!(compose.caret(), 0);
    }

    #[test]
    fn a_line_jump_stays_inside_the_line_the_caret_is_on() {
        // Cmd-← and Cmd-→. The logical line, never the wrapped row: a caret
        // sent to the start of a row it only landed on because the pane is
        // narrow would move again the moment the window was resized.
        let mut compose = typed("first line\nsecond line");
        compose.line_start();
        assert_eq!(compose.caret(), 11);
        compose.line_end();
        assert_eq!(compose.caret(), 22);
        compose.left();
        compose.left();
        compose.line_start();
        assert_eq!(compose.caret(), 11);
    }

    #[test]
    fn deleting_a_word_takes_the_space_with_it() {
        // Ctrl-backspace. It agrees with `word_left` about where a word begins,
        // because a developer who holds one down and reaches for the other
        // should not find them off by a space.
        let mut compose = typed("cargo test --workspace ");
        compose.delete_word_left();
        assert_eq!(compose.text(), "cargo test ");
        compose.delete_word_left();
        assert_eq!(compose.text(), "cargo ");
        assert_eq!(compose.caret(), 6);
        // and it stops at the start rather than panicking there
        compose.delete_word_left();
        compose.delete_word_left();
        assert_eq!(compose.text(), "");
    }

    #[test]
    fn deleting_to_the_line_start_leaves_the_line_above_alone() {
        // Cmd-backspace. Two lines, and only the second one goes.
        let mut compose = typed("keep this\nthrow this away");
        compose.delete_to_line_start();
        assert_eq!(compose.text(), "keep this\n");
        assert_eq!(compose.caret(), 10);
        // at the start of a line it takes nothing rather than eating the newline
        compose.delete_to_line_start();
        assert_eq!(compose.text(), "keep this\n");
    }

    #[test]
    fn a_prompt_wider_than_the_box_wraps_at_a_word() {
        let compose = typed("why is the nightly job so slow");
        let wrapped = compose.wrap(12);
        assert_eq!(wrapped.rows, vec!["why is the ", "nightly job ", "so slow"]);
        // Every row is a contiguous slice, so the whole prompt is still there.
        assert_eq!(wrapped.rows.concat(), compose.text());
    }

    #[test]
    fn a_newline_the_developer_typed_is_a_break_the_wrap_may_not_undo() {
        let mut compose = typed("cargo test");
        compose.insert('\n');
        for ch in "and say why".chars() {
            compose.insert(ch);
        }
        let wrapped = compose.wrap(40);
        assert_eq!(wrapped.rows, vec!["cargo test\n", "and say why"]);
        // and it survives being sent, so the harness sees two lines
        assert_eq!(compose.take(), "cargo test\nand say why");
    }

    #[test]
    fn the_caret_lands_on_the_row_its_character_is_on() {
        let mut compose = typed("why is the nightly job so slow");
        // At the very end, which is the row the last word is on.
        assert_eq!(compose.wrap(12).caret, (2, 7));
        compose.start();
        assert_eq!(compose.wrap(12).caret, (0, 0));
        // Exactly on a break: the row after it, at column zero, rather than
        // hanging off the right edge of the row before.
        for _ in 0..11 {
            compose.right();
        }
        assert_eq!(compose.wrap(12).caret, (1, 0));
    }

    #[test]
    fn an_empty_box_still_has_one_row_and_a_caret_in_it() {
        let compose = Compose::default();
        let wrapped = compose.wrap(20);
        assert_eq!(wrapped.rows, vec![""]);
        assert_eq!(wrapped.caret, (0, 0));
        assert_eq!(compose.height(20), 1);
    }

    #[test]
    fn a_word_longer_than_the_box_is_cut_rather_than_drawn_past_the_edge() {
        // A path or a URL. The alternative is a row wider than the pane, which
        // ratatui draws by simply stopping — with no mark and no caret.
        let compose = typed("/home/ada/Clubria/riabuild/riabuild-cli");
        let wrapped = compose.wrap(10);
        assert!(wrapped.rows.iter().all(|row| row.chars().count() <= 10));
        assert_eq!(wrapped.rows.concat(), compose.text());
    }

    #[test]
    fn a_box_no_columns_wide_still_wraps_rather_than_dividing_by_zero() {
        let compose = typed("ab");
        assert_eq!(compose.wrap(0).rows.concat(), "ab");
    }
}

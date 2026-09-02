//! The one line a developer types into, and where the caret is in it.
//!
//! Its own type because the caret is the whole of what makes the session pane
//! keep the keyboard. `←` at column 0 is what hands it back to the session
//! list, and every other `←` moves inside the text — so "where is the caret"
//! is not a rendering detail, it is the condition the keymap branches on.
//!
//! Indices are **characters**, never bytes. A developer typing an em-dash into
//! a prompt would otherwise split a `String` mid-codepoint, which is a panic
//! rather than a wrong caret.

/// A single-line editor.
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

    pub fn start(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.len();
    }

    /// Empties the line and hands back what was in it, trimmed.
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

    /// The text either side of the caret, for drawing one.
    pub fn split(&self) -> (&str, &str) {
        let at = self.byte_of(self.caret);
        self.text.split_at(at)
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
    fn the_two_halves_of_the_line_are_what_a_caret_is_drawn_between() {
        let mut compose = typed("abcd");
        compose.left();
        compose.left();
        assert_eq!(compose.split(), ("ab", "cd"));
    }
}

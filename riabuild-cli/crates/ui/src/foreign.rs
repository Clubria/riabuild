//! Text riabuild did not write, on its way to a terminal.
//!
//! Two boxes now carry a sentence somebody else typed: a repository's
//! description, which GitHub serves and any member of the org can set, and a
//! shared server's, which riabuild-web serves and a lead types. Both are
//! printed straight onto the developer's terminal, which is the whole reason
//! this function exists rather than a `format!`.
//!
//! A terminal is not a text box. `\x1b[2J` clears the screen, `\x1b[1;1H` moves
//! the cursor, and `\r` rewrites the line just printed — so a description
//! carrying one of those does not appear in the box, it *edits* the box, and
//! the row a developer reads is no longer the row riabuild drew. A newline in
//! the middle of a description does the same thing more quietly: the box's
//! alignment is computed over rows, and one value spanning two of them shifts
//! every row under it.
//!
//! So nothing arrives from elsewhere and reaches a terminal unchanged. What is
//! kept is the printable text; everything else becomes a space, and runs of
//! space collapse to one.
//!
//! The same rule as `riabuild-runner`'s `subdue` filter, which drops every
//! escape sequence out of a subdued child's output, applied at the other end:
//! there it is a program's own output, here it is a value a person typed into a
//! form somewhere else.

/// One printable line of somebody else's text, at most `max` terminal columns.
///
/// Truncation is by `chars()` rather than by display width, as everywhere else
/// in the boxes: riabuild does not depend on a width table, and the cost of
/// being wrong is a column that does not line up rather than a sentence that
/// rewrites the screen.
///
/// The ellipsis is inside the budget, so the answer never exceeds `max` — a
/// caller that sized a column against `max` must not be handed `max + 1`.
pub fn one_line(raw: &str, max: usize) -> String {
    let mut kept = String::with_capacity(raw.len().min(max));
    let mut pending_space = false;
    for character in raw.chars() {
        if character.is_whitespace() {
            // A run of whitespace is one space, and a leading run is nothing:
            // `pending_space` is only spent when something printable follows.
            pending_space = !kept.is_empty();
            continue;
        }
        // Dropped rather than spaced, because neither kind occupies a column:
        // a space in their place would open a gap in a sentence that never had
        // one, and `\x1b` sits *inside* a word as often as between two.
        if character.is_control() || is_invisible(character) {
            continue;
        }
        if pending_space {
            kept.push(' ');
            pending_space = false;
        }
        kept.push(character);
    }

    if kept.chars().count() <= max {
        return kept;
    }
    // A caller with no room at all gets nothing, rather than a lone ellipsis
    // one column wider than the column it was measured for.
    if max == 0 {
        return String::new();
    }
    let cut: String = kept.chars().take(max - 1).collect();
    // A trailing space against the ellipsis reads as a gap in the sentence
    // rather than as a cut, so it goes.
    format!("{}…", cut.trim_end())
}

/// Characters that occupy no columns and change how the ones around them are
/// laid out: the bidirectional overrides, the zero-width joiners and spaces,
/// and the byte-order mark.
///
/// `char::is_control` does not cover them — they are `Cf`, not `Cc` — and a
/// right-to-left override in a repository description reverses the rest of the
/// row it lands in, which is a box that lies about which repository is on which
/// line. Named individually rather than by category so that adding one is a
/// decision: riabuild has no unicode tables and is not acquiring any here.
fn is_invisible(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_sentence_survives_whole() {
        assert_eq!(
            one_line("The hub every builder starts from", 80),
            "The hub every builder starts from"
        );
    }

    #[test]
    fn an_escape_sequence_cannot_reach_the_terminal() {
        // The one that matters: a description of this shape does not appear in
        // the box, it clears the screen and redraws the rows above it.
        let drawn = one_line("payments\x1b[2J\x1b[1;1Hriabuild: not signed in", 80);
        assert!(!drawn.contains('\x1b'), "{drawn}");
        assert_eq!(drawn, "payments[2J[1;1Hriabuild: not signed in");
    }

    #[test]
    fn a_description_spanning_two_lines_becomes_one() {
        // A box's alignment is computed over rows. One value spanning two of
        // them shifts every row under it.
        assert_eq!(
            one_line("Billing\nand payment flows\r\n", 80),
            "Billing and payment flows"
        );
        assert_eq!(one_line("a\tb", 80), "a b");
    }

    #[test]
    fn runs_of_space_collapse_and_the_ends_are_trimmed() {
        assert_eq!(one_line("   too    much   room   ", 80), "too much room");
    }

    #[test]
    fn a_right_to_left_override_is_dropped_rather_than_printed() {
        // `Cf`, not `Cc`, so `is_control` says nothing about it — and it
        // reverses the rest of the row it lands in.
        let drawn = one_line("payments\u{202e}stnemyap", 80);
        assert_eq!(drawn, "paymentsstnemyap");
        assert!(!drawn.contains('\u{202e}'));
    }

    #[test]
    fn an_essay_is_cut_to_the_room_there_is() {
        let cut = one_line("a".repeat(200).as_str(), 10);
        assert_eq!(cut.chars().count(), 10, "{cut}");
        assert!(cut.ends_with('…'), "{cut}");
    }

    #[test]
    fn a_cut_never_leaves_a_space_against_the_ellipsis() {
        assert_eq!(one_line("one two three", 9), "one two…");
    }

    #[test]
    fn a_column_with_no_room_gets_nothing_rather_than_an_ellipsis() {
        assert_eq!(one_line("anything", 0), "");
        assert_eq!(one_line("anything", 1), "…");
    }

    #[test]
    fn nothing_typed_is_nothing_printed() {
        assert_eq!(one_line("", 80), "");
        assert_eq!(one_line("   \n\t ", 80), "");
    }

    #[test]
    fn a_multibyte_description_is_cut_on_a_character_rather_than_a_byte() {
        // `chars()` throughout: slicing this by bytes panics.
        let cut = one_line("Ünï‑çödé descriptions everywhere", 8);
        assert_eq!(cut.chars().count(), 8, "{cut}");
    }
}

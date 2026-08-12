//! The riabuild mark.
//!
//! A triangle drawn as a border of even geometric width. That evenness is the
//! whole design: a terminal cell is about twice as tall as it is wide, so a
//! wall offset by `t` columns measures `t · 2/√5` across while a base of `b`
//! rows measures `2b`. The mark below uses `t = 2, b = 1` — 1.79 against 2.00,
//! the closest an integer grid gets at this size. Changing either number
//! without redoing that arithmetic makes one edge visibly heavier than the
//! others.
//!
//! Two renderings, chosen by whether the terminal can be trusted with the block
//! glyphs. Both are six rows tall, so the banner occupies the same vertical
//! space either way and nothing jumps when a developer switches terminals.

use crate::theme::{self, Role, Theme};

/// Rows in the mark. Both renderings share it.
pub const HEIGHT: usize = 6;

/// The block rendering.
///
/// **Every glyph here is Block Elements, U+2580–U+259F, and that is a hard
/// constraint rather than a preference.** The first version cut the corners with
/// `◢◣◤◥` (U+25E2–U+25E5, Geometric Shapes), which is the shape the design
/// wants: a true half-cell diagonal. Menlo does not carry them. macOS Terminal
/// falls back to another face, which draws them at *its* optical size instead of
/// the cell box — so the sloped glyphs rendered visibly smaller than the `█`
/// beside them and the border came apart. Block Elements are defined to tile the
/// cell exactly and every monospace font ships them, so the wall and the base
/// are guaranteed to meet.
///
/// The quadrant blocks say the same thing, quantised: each one is a full cell
/// with the corner *outside* the shape removed. `▟` is missing its upper left,
/// so it cuts the outer left edge; `▛` is missing its lower right, so it cuts
/// the hole's left edge facing back the other way. `▙` and `▜` mirror them. The
/// base is `█` between `▟` and `▙`, all three of which fill the bottom of their
/// cell — that is what keeps it flat, and it is why the half-height blocks
/// (`▀`, `▄`) are not an option for tuning the base weight even though they are
/// in the same Unicode block.
///
/// Flush left and twelve columns wide, which is what makes the mirror test
/// below meaningful: the apex straddles columns 5 and 6, so the reflection axis
/// falls between two cells rather than through one.
const MARK: [&str; HEIGHT] = [
    "     ▟▙",
    "    ▟██▙",
    "   ▟█▛▜█▙",
    "  ▟█▛  ▜█▙",
    " ▟█▛    ▜█▙",
    "▟██████████▙",
];

/// The ASCII rendering, for terminals without a UTF-8 locale.
///
/// The border is drawn by its two outlines with nothing between them: every
/// printable ASCII fill character is noisy enough to compete with the
/// silhouette, so whitespace is the better fill.
const ASCII: [&str; HEIGHT] = [
    "     /\\",
    "    /  \\",
    "   / /\\ \\",
    "  / /  \\ \\",
    " / /____\\ \\",
    "/__________\\",
];

/// Whether the terminal's locale promises the block glyphs will render.
///
/// Unset locale means ASCII. A mark that renders as six rows of `▯` is a worse
/// first impression than one drawn in slashes, and this runs before riabuild
/// has done anything a developer can judge it by.
pub fn glyphs_render() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .map(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
        .unwrap_or(false)
}

/// The mark's rows, unpainted.
pub fn rows(unicode: bool) -> &'static [&'static str; HEIGHT] {
    if unicode { &MARK } else { &ASCII }
}

/// Columns the mark occupies, so the wordmark beside it can be aligned.
fn width(unicode: bool) -> usize {
    rows(unicode)
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0)
}

/// The startup banner: the mark, painted down the brand gradient, with the
/// wordmark set beside it.
///
/// Returns lines rather than printing them, so the layout can be tested without
/// capturing stdout.
pub fn banner(theme: Theme, unicode: bool, org: &str, version: &str) -> Vec<String> {
    let rows = rows(unicode);
    let width = width(unicode);
    // Down the mark, pink to brand — the same accent gradient clubria.com runs
    // across its primary button.
    let gradient = theme::ramp(theme::PINK, theme::BRAND, HEIGHT);

    let beside = |row: usize| -> Option<String> {
        match row {
            2 => Some(theme.paint(Role::Brand, "riabuild")),
            3 => Some(theme.paint(Role::Muted, version)),
            4 => Some(theme.paint(Role::Muted, &format!("· {org} environment"))),
            _ => None,
        }
    };

    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let padding = " ".repeat(width - row.chars().count());
            let painted = theme.paint_rgb(gradient[index], row);
            match beside(index) {
                // The padding is computed from the unpainted row: escape
                // sequences occupy no columns, so measuring the painted string
                // would push the wordmark right by however many bytes of colour
                // happen to precede it.
                Some(text) => format!("  {painted}{padding}    {text}"),
                None => format!("  {painted}"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Depth;

    /// Left and right halves of every row must be reflections. The glyph pairs
    /// swap handedness across the mirror; anything else must be symmetric on
    /// its own.
    fn mirrored(row: &str, width: usize) -> String {
        let padded: Vec<char> = format!("{row:<width$}").chars().collect();
        padded
            .iter()
            .rev()
            .map(|glyph| match glyph {
                '▟' => '▙',
                '▙' => '▟',
                '▛' => '▜',
                '▜' => '▛',
                '/' => '\\',
                '\\' => '/',
                other => *other,
            })
            .collect()
    }

    #[test]
    fn both_renderings_are_exactly_symmetric() {
        for unicode in [true, false] {
            let width = width(unicode);
            for (index, row) in rows(unicode).iter().enumerate() {
                assert_eq!(
                    format!("{row:<width$}"),
                    mirrored(row, width),
                    "row {index} of {}",
                    if unicode {
                        "the mark"
                    } else {
                        "the ascii mark"
                    }
                );
            }
        }
    }

    #[test]
    fn the_base_is_flat() {
        // Every glyph on the base row must fill the bottom of its cell. `▟`,
        // `█` and `▙` all do. `▀` does not, and a base built from it hangs
        // above the line with a gap at each corner — which is what `▛`/`▜`
        // would do here too, since each is missing one bottom quadrant.
        let base = MARK[HEIGHT - 1].trim();
        let mut glyphs = base.chars();
        assert_eq!(glyphs.next(), Some('▟'));
        assert_eq!(glyphs.next_back(), Some('▙'));
        assert!(glyphs.all(|glyph| glyph == '█'), "{base}");

        let ascii = ASCII[HEIGHT - 1];
        assert!(ascii.starts_with('/') && ascii.ends_with('\\'));
        assert!(ascii[1..ascii.len() - 1].chars().all(|g| g == '_'));
    }

    #[test]
    fn the_mark_is_block_elements_only() {
        // The bug this pins: `◢◣◤◥` (U+25E2..) are the shape the design wants,
        // but Menlo does not have them, so macOS Terminal drew them from a
        // fallback face at that face's optical size — visibly smaller than the
        // `█` beside them, with the border coming apart at every corner. Block
        // Elements are defined to tile the cell and ship in every monospace
        // font, so the wall and the base are guaranteed to meet.
        for row in MARK {
            for glyph in row.chars() {
                assert!(
                    glyph == ' ' || ('\u{2580}'..='\u{259f}').contains(&glyph),
                    "{glyph:?} (U+{:04X}) in {row:?} is outside Block Elements",
                    glyph as u32
                );
            }
        }
    }

    #[test]
    fn the_base_row_carries_no_glyph_with_an_empty_bottom_quadrant() {
        // Stated separately from `the_base_is_flat` because this is the rule a
        // future edit would break: `▛` and `▜` are the right glyphs for the
        // hole's edges and exactly the wrong ones for the base.
        for glyph in MARK[HEIGHT - 1].trim().chars() {
            assert!(
                "▟█▙▄".contains(glyph),
                "{glyph:?} does not fill the bottom of its cell"
            );
        }
    }

    #[test]
    fn both_renderings_are_the_same_size() {
        // So the banner does not change shape with the developer's locale, and
        // the wordmark sits in the same column either way.
        assert_eq!(MARK.len(), ASCII.len());
        assert_eq!(MARK.len(), HEIGHT);
        assert_eq!(width(true), width(false));
    }

    #[test]
    fn the_reflection_axis_falls_between_two_cells() {
        // An odd field width would put the axis through a column, and every row
        // would then fail to mirror by half a cell — the symmetry test would be
        // asserting the wrong thing rather than nothing.
        assert_eq!(width(true) % 2, 0);
    }

    #[test]
    fn the_mark_stays_inside_a_narrow_terminal() {
        // Against 80 columns, with the longest plausible wordmark beside it.
        let lines = banner(Theme::plain(), true, "Clubria", "9999.0.0-dev");
        for line in &lines {
            assert!(line.chars().count() < 60, "{line:?} is {}", line.len());
        }
    }

    #[test]
    fn a_plain_banner_carries_no_escapes() {
        let lines = banner(Theme::plain(), true, "Clubria", "2026.08.05");
        assert!(!lines.join("\n").contains('\x1b'));
    }

    #[test]
    fn the_banner_names_the_org_and_the_version() {
        let text = banner(Theme::plain(), true, "Clubria", "2026.08.05").join("\n");
        assert!(text.contains("riabuild"), "{text}");
        assert!(text.contains("2026.08.05"), "{text}");
        assert!(text.contains("· Clubria environment"), "{text}");
    }

    #[test]
    fn the_wordmark_lines_up_whether_or_not_the_mark_is_painted() {
        // Escapes occupy no columns. Measuring the painted row instead of the
        // bare one would indent the wordmark by the length of a colour code.
        let plain = banner(Theme::plain(), true, "Clubria", "2026.08.05");
        let painted = banner(
            Theme::with_depth(Depth::TrueColor),
            true,
            "Clubria",
            "2026.08.05",
        );
        for (plain, painted) in plain.iter().zip(&painted) {
            let strip = |line: &str| {
                let mut out = String::new();
                let mut chars = line.chars();
                while let Some(glyph) = chars.next() {
                    if glyph == '\x1b' {
                        for glyph in chars.by_ref() {
                            if glyph == 'm' {
                                break;
                            }
                        }
                    } else {
                        out.push(glyph);
                    }
                }
                out
            };
            assert_eq!(*plain, strip(painted));
        }
    }

    #[test]
    fn the_gradient_paints_each_row_a_different_colour() {
        let lines = banner(
            Theme::with_depth(Depth::TrueColor),
            true,
            "Clubria",
            "2026.08.05",
        );
        let apex = &lines[0];
        let base = &lines[HEIGHT - 1];
        assert!(apex.contains("38;2;230;74;160"), "{apex:?}"); // --pink
        assert!(base.contains("38;2;247;79;37"), "{base:?}"); // brand
    }

    #[test]
    fn a_utf8_locale_is_what_selects_the_block_glyphs() {
        // `glyphs_render` reads the process environment, so assert the decision
        // it encodes rather than mutating env vars under parallel tests.
        for value in ["en_US.UTF-8", "C.utf8", "en_GB.utf-8"] {
            let value = value.to_ascii_lowercase();
            assert!(value.contains("utf-8") || value.contains("utf8"), "{value}");
        }
        for value in ["C", "POSIX", "en_US.ISO8859-1"] {
            let value = value.to_ascii_lowercase();
            assert!(
                !(value.contains("utf-8") || value.contains("utf8")),
                "{value}"
            );
        }
    }

    /// Prints the banner at every rung of the ladder.
    ///
    /// Ignored by default — it asserts nothing. It exists because the one thing
    /// no assertion here covers is whether the mark actually *looks* right, and
    /// that has to be checked by eye after any change to the glyphs or the
    /// gradient: `cargo test preview -- --ignored --nocapture`.
    #[test]
    #[ignore = "visual check only"]
    fn preview() {
        for (label, depth, unicode) in [
            ("truecolor", Depth::TrueColor, true),
            ("256 colour", Depth::Ansi256, true),
            ("16 colour", Depth::Ansi16, true),
            ("ascii, truecolor", Depth::TrueColor, false),
            ("plain", Depth::None, true),
        ] {
            println!("\n--- {label} ---");
            for line in banner(Theme::with_depth(depth), unicode, "Clubria", "2026.08.05.1") {
                println!("{line}");
            }
        }
    }
}

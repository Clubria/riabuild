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

/// The quadrant-triangle rendering.
///
/// `◢`/`◣` fill the lower half of their cell and `◤`/`◥` the upper half, so the
/// outer edge fills inward and the inner edge fills back outward and the wall
/// between them reads as one solid stroke. The base is `█` throughout: every
/// glyph on that row has to touch the bottom of its cell or the base stops
/// looking flat, which rules out the half-blocks that would otherwise be the
/// obvious way to fine-tune its weight.
/// Both renderings are flush left and twelve columns wide, which is what makes
/// the mirror test below meaningful: the apex straddles columns 5 and 6, so the
/// reflection axis falls between two cells rather than through one.
const MARK: [&str; HEIGHT] = [
    "     ◢◣",
    "    ◢██◣",
    "   ◢█◤◥█◣",
    "  ◢█◤  ◥█◣",
    " ◢█◤    ◥█◣",
    "◢██████████◣",
];

/// The ASCII rendering, for terminals that will not draw the mark properly —
/// no UTF-8 locale, or macOS Terminal. See [`glyphs_render_in`].
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

/// Whether this terminal will draw the mark the way it is designed.
///
/// Two ways it will not, and both fall back to the ASCII outline rather than
/// degrade the mark itself — a triangle drawn in slashes is a fine thing to
/// look at, and this runs before riabuild has done anything a developer can
/// judge it by.
pub fn glyphs_render() -> bool {
    // Read here, decided in `glyphs_render_in`, so the rules are testable
    // without setting process-wide environment variables — the same split
    // `theme::depth_for` uses.
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    glyphs_render_in(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        locale.as_deref(),
    )
}

/// The rules behind [`glyphs_render`].
///
/// `Apple_Terminal` is macOS's built-in Terminal, and it is named specifically
/// rather than inferred from the platform: this is a fact about a font, not
/// about an operating system. Terminal's faces — Menlo, and SF Mono on newer
/// releases — carry Block Elements but not `◢◣◤◥` (U+25E2–U+25E5, Geometric
/// Shapes). macOS resolves the gap through font fallback, which draws those
/// four at the substitute face's optical size instead of the cell box, so the
/// walls come out visibly smaller than the `█` between them and the border
/// falls apart at every corner. iTerm2, Ghostty, Alacritty, WezTerm and VS
/// Code's terminal all ship faces that have them, and all identify themselves
/// differently, so none of them is affected.
///
/// The locale is the second rule and the older one: no UTF-8, no mark.
pub fn glyphs_render_in(term_program: Option<&str>, locale: Option<&str>) -> bool {
    if term_program == Some("Apple_Terminal") {
        return false;
    }
    locale
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
                '◢' => '◣',
                '◣' => '◢',
                '◤' => '◥',
                '◥' => '◤',
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
        // Every glyph on the base row must touch the bottom of its cell. `▀`
        // and the other half-blocks do not, and a base built from them hangs
        // above the line with a gap at each corner.
        let base = MARK[HEIGHT - 1].trim();
        let mut glyphs = base.chars();
        assert_eq!(glyphs.next(), Some('◢'));
        assert_eq!(glyphs.next_back(), Some('◣'));
        assert!(glyphs.all(|glyph| glyph == '█'), "{base}");

        let ascii = ASCII[HEIGHT - 1];
        assert!(ascii.starts_with('/') && ascii.ends_with('\\'));
        assert!(ascii[1..ascii.len() - 1].chars().all(|g| g == '_'));
    }

    #[test]
    fn the_mark_uses_no_half_height_glyphs() {
        // The half-blocks are what a future edit would reach for to adjust the
        // base weight. They are exactly what makes the corners hollow.
        for row in MARK {
            for glyph in row.chars() {
                assert!(
                    !"▀▄▖▗▘▝▚▞▙▟▛▜".contains(glyph),
                    "half-height glyph {glyph:?} in {row:?}"
                );
            }
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
        for locale in ["en_US.UTF-8", "C.utf8", "en_GB.utf-8"] {
            assert!(glyphs_render_in(None, Some(locale)), "{locale}");
        }
        for locale in ["C", "POSIX", "en_US.ISO8859-1"] {
            assert!(!glyphs_render_in(None, Some(locale)), "{locale}");
        }
        assert!(!glyphs_render_in(None, None), "an unset locale means ascii");
    }

    #[test]
    fn macos_terminal_gets_the_ascii_mark_however_good_its_locale_is() {
        // Terminal.app's fonts have no U+25E2..U+25E5, and macOS fills the gap
        // by substituting a face that draws them at its own optical size — the
        // walls render smaller than the `█` between them. The outline is the
        // better thing to show there.
        assert!(!glyphs_render_in(
            Some("Apple_Terminal"),
            Some("en_US.UTF-8")
        ));
    }

    #[test]
    fn every_other_terminal_keeps_the_mark() {
        // Named individually because the rule is about one program's fonts, not
        // about macOS: iTerm2, Ghostty and the rest run on the same laptops and
        // draw the mark correctly.
        for program in ["iTerm.app", "ghostty", "WezTerm", "vscode", "Alacritty"] {
            assert!(
                glyphs_render_in(Some(program), Some("en_US.UTF-8")),
                "{program}"
            );
        }
        assert!(glyphs_render_in(None, Some("en_US.UTF-8")));
    }

    #[test]
    fn a_bad_locale_still_wins_under_any_terminal() {
        for program in [None, Some("iTerm.app"), Some("Apple_Terminal")] {
            assert!(!glyphs_render_in(program, Some("C")), "{program:?}");
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

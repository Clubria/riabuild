//! The Clubria colour scheme.
//!
//! One palette and one degradation ladder, used everywhere riabuild writes to a
//! terminal. The hexes are the brand's own, taken from clubria.com: `#f74f25`
//! is the fill of the Clubria logo mark, and the rest are the site's published
//! design tokens (`--pink`, `--orange`, `--green`).
//!
//! Colour is chosen by *role*, never by escape code at the call site. A role
//! knows how to render itself at every colour depth, so a terminal that cannot
//! do 24-bit still gets something deliberate rather than nothing — and adding a
//! colour in one place cannot leave another place on the old palette.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct. The `feature = "testing"` half matters as much as the `test` half:
// when a downstream crate turns the feature on, this crate is compiled as a
// dependency and `cfg(test)` is false, so the exemption would not apply.
#![cfg_attr(any(test, feature = "testing"), allow(clippy::unwrap_used))]

/// A brand colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// From the hex the design system publishes, so the constants below can be
    /// read straight against the site's CSS.
    pub const fn hex(value: u32) -> Self {
        Self(
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
        )
    }

    fn distance_to(self, other: Rgb) -> u32 {
        let d = |a: u8, b: u8| {
            let delta = a as i32 - b as i32;
            (delta * delta) as u32
        };
        d(self.0, other.0) + d(self.1, other.1) + d(self.2, other.2)
    }

    fn lerp(self, other: Rgb, numerator: usize, denominator: usize) -> Rgb {
        let mix = |a: u8, b: u8| {
            let a = a as usize;
            let b = b as usize;
            let value = if b >= a {
                a + (b - a) * numerator / denominator
            } else {
                a - (a - b) * numerator / denominator
            };
            value as u8
        };
        Rgb(
            mix(self.0, other.0),
            mix(self.1, other.1),
            mix(self.2, other.2),
        )
    }
}

/// The Clubria logo mark's fill. The primary brand colour.
pub const BRAND: Rgb = Rgb::hex(0xf74f25);
/// `--pink`. The far end of the site's accent gradient.
pub const PINK: Rgb = Rgb::hex(0xe64aa0);
/// `--orange`. Brand-adjacent, one step cooler than [`BRAND`].
pub const ORANGE: Rgb = Rgb::hex(0xf0563c);
/// `--green`. Reserved for "this is done".
pub const GREEN: Rgb = Rgb::hex(0x3ddc84);

/// How much colour this terminal can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Depth {
    /// No escapes at all: piped output, `NO_COLOR`, or `TERM=dumb`.
    None,
    /// The original eight, which every terminal has had for forty years.
    Ansi16,
    /// The xterm-256 cube. Enough for a gradient, though not an exact one.
    Ansi256,
    /// 24-bit. The palette renders as the brand actually specifies it.
    TrueColor,
}

/// Picks a depth from the environment.
///
/// Split out from [`Theme::detect`] so the ladder is testable without setting
/// process-wide environment variables, which race across parallel tests.
pub fn depth_for(
    is_terminal: bool,
    no_color: bool,
    colorterm: Option<&str>,
    term: Option<&str>,
) -> Depth {
    // NO_COLOR is honoured whatever the terminal claims it can do; so is a
    // destination that is not a terminal, because escapes in a log file or a
    // pipe are noise a reader has to strip back out.
    if no_color || !is_terminal {
        return Depth::None;
    }
    // `TERM=dumb` is the one value that means "escapes will not work", as
    // opposed to merely not saying how many colours there are.
    if term == Some("dumb") {
        return Depth::None;
    }
    let colorterm = colorterm.unwrap_or_default();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return Depth::TrueColor;
    }
    match term {
        Some(term) if term.contains("256color") => Depth::Ansi256,
        // An unset TERM on a real terminal is unusual but not a reason to give
        // up colour entirely — the original sixteen are always safe.
        _ => Depth::Ansi16,
    }
}

/// The nearest xterm-256 index to `colour`.
///
/// The 256-colour space is a 6×6×6 cube plus a 24-step grey ramp, and the two
/// overlap badly near grey, so both are tried and the closer one wins.
pub fn xterm256(colour: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |value: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| (**level as i32 - value as i32).abs())
            .map(|(index, _)| index)
            .unwrap_or(0)
    };
    let (r, g, b) = (nearest(colour.0), nearest(colour.1), nearest(colour.2));
    let cube = Rgb(LEVELS[r], LEVELS[g], LEVELS[b]);
    let cube_index = 16 + 36 * r + 6 * g + b;

    let average = (colour.0 as u32 + colour.1 as u32 + colour.2 as u32) / 3;
    let step = (average.saturating_sub(8) / 10).min(23) as usize;
    let level = (8 + 10 * step) as u8;
    let grey = Rgb(level, level, level);

    if grey.distance_to(colour) < cube.distance_to(colour) {
        (232 + step) as u8
    } else {
        cube_index as u8
    }
}

/// `steps` colours walking from `from` to `to`, inclusive of both ends.
pub fn ramp(from: Rgb, to: Rgb, steps: usize) -> Vec<Rgb> {
    match steps {
        0 => Vec::new(),
        1 => vec![from],
        _ => (0..steps)
            .map(|step| from.lerp(to, step, steps - 1))
            .collect(),
    }
}

/// What a piece of text *is*, rather than what colour it should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The riabuild wordmark and anything else speaking as the brand.
    Brand,
    /// Finished, satisfied, active.
    Ok,
    /// In progress.
    Busy,
    /// Worth reading, not fatal.
    Warn,
    /// Fatal.
    Danger,
    /// Secondary text: reasons, hints, versions.
    Muted,
    /// Structural emphasis with no hue of its own.
    Strong,
}

impl Role {
    /// The SGR parameters for this role, or `None` for "write it plain".
    pub fn sgr(self, depth: Depth) -> Option<String> {
        // Muted and Strong are attributes, not colours. Dim and bold adapt to
        // whatever background and theme the developer has chosen, which a fixed
        // grey cannot do — a `#8b8794` "muted" is invisible on a dark theme and
        // muddy on a light one. They render identically at every depth.
        match self {
            Role::Muted => return (depth != Depth::None).then(|| "2".to_string()),
            Role::Strong => return (depth != Depth::None).then(|| "1".to_string()),
            _ => {}
        }
        let (colour, bold, legacy) = match self {
            Role::Brand => (BRAND, true, "1;31"),
            Role::Ok => (GREEN, false, "32"),
            Role::Busy => (ORANGE, false, "33"),
            Role::Warn => (ORANGE, false, "33"),
            Role::Danger => (BRAND, true, "1;31"),
            Role::Muted | Role::Strong => unreachable!("handled above"),
        };
        match depth {
            Depth::None => None,
            Depth::Ansi16 => Some(legacy.to_string()),
            Depth::Ansi256 => Some(prefix(bold) + &format!("38;5;{}", xterm256(colour))),
            Depth::TrueColor => {
                Some(prefix(bold) + &format!("38;2;{};{};{}", colour.0, colour.1, colour.2))
            }
        }
    }
}

fn prefix(bold: bool) -> String {
    if bold {
        "1;".to_string()
    } else {
        String::new()
    }
}

/// The palette bound to one terminal's capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    depth: Depth,
}

impl Default for Theme {
    fn default() -> Self {
        Self::plain()
    }
}

impl Theme {
    /// Reads the environment. Call once, at startup.
    pub fn detect(is_terminal: bool) -> Self {
        Self {
            depth: depth_for(
                is_terminal,
                std::env::var_os("NO_COLOR").is_some(),
                std::env::var("COLORTERM").ok().as_deref(),
                std::env::var("TERM").ok().as_deref(),
            ),
        }
    }

    /// No colour, ever. The shape tests and non-terminal output use this.
    pub const fn plain() -> Self {
        Self { depth: Depth::None }
    }

    /// A theme pinned to one rung of the ladder, so a test can assert what a
    /// given terminal actually receives.
    #[cfg(any(test, feature = "testing"))]
    pub const fn with_depth(depth: Depth) -> Self {
        Self { depth }
    }

    /// Whether anything at all will be painted.
    pub fn enabled(self) -> bool {
        self.depth != Depth::None
    }

    pub fn paint(self, role: Role, text: &str) -> String {
        match role.sgr(self.depth) {
            Some(sgr) => format!("\x1b[{sgr}m{text}\x1b[0m"),
            None => text.to_string(),
        }
    }

    /// Paints an exact colour, for the banner gradient.
    ///
    /// Below 256 colours there is no gradient to be had, so every step
    /// collapses onto the brand — deliberately one flat brand-coloured mark
    /// rather than a banded approximation of a smooth one.
    pub fn paint_rgb(self, colour: Rgb, text: &str) -> String {
        match self.depth {
            Depth::None => text.to_string(),
            Depth::Ansi16 => self.paint(Role::Brand, text),
            Depth::Ansi256 => format!("\x1b[38;5;{}m{text}\x1b[0m", xterm256(colour)),
            Depth::TrueColor => {
                format!(
                    "\x1b[38;2;{};{};{}m{text}\x1b[0m",
                    colour.0, colour.1, colour.2
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_hex_is_the_logo_marks_fill() {
        // clubria.com paints its logo `<g fill="#f74f25">`. If this constant
        // drifts, the CLI stops matching the site it provisions access to.
        assert_eq!(BRAND, Rgb(0xf7, 0x4f, 0x25));
        assert_eq!(PINK, Rgb(0xe6, 0x4a, 0xa0));
        assert_eq!(GREEN, Rgb(0x3d, 0xdc, 0x84));
    }

    #[test]
    fn no_color_beats_every_capability_the_terminal_advertises() {
        assert_eq!(
            depth_for(true, true, Some("truecolor"), Some("xterm-256color")),
            Depth::None
        );
    }

    #[test]
    fn output_that_is_not_a_terminal_gets_no_escapes() {
        // Escapes in a pipe or a CI log are noise the reader has to strip.
        assert_eq!(
            depth_for(false, false, Some("truecolor"), Some("xterm-256color")),
            Depth::None
        );
    }

    #[test]
    fn a_dumb_terminal_is_not_merely_an_unknown_one() {
        assert_eq!(depth_for(true, false, None, Some("dumb")), Depth::None);
        assert_eq!(depth_for(true, false, None, None), Depth::Ansi16);
    }

    #[test]
    fn the_depth_ladder_reads_colorterm_then_term() {
        assert_eq!(
            depth_for(true, false, Some("truecolor"), Some("xterm-256color")),
            Depth::TrueColor
        );
        assert_eq!(
            depth_for(true, false, Some("24bit"), None),
            Depth::TrueColor
        );
        assert_eq!(
            depth_for(true, false, None, Some("xterm-256color")),
            Depth::Ansi256
        );
        assert_eq!(depth_for(true, false, None, Some("xterm")), Depth::Ansi16);
    }

    #[test]
    fn every_role_is_silent_when_there_is_no_colour() {
        for role in [
            Role::Brand,
            Role::Ok,
            Role::Busy,
            Role::Warn,
            Role::Danger,
            Role::Muted,
            Role::Strong,
        ] {
            assert_eq!(role.sgr(Depth::None), None, "{role:?}");
            assert!(!Theme::plain().paint(role, "text").contains('\x1b'));
        }
    }

    #[test]
    fn true_colour_renders_the_brand_hex_exactly() {
        assert_eq!(
            Role::Brand.sgr(Depth::TrueColor).unwrap(),
            "1;38;2;247;79;37"
        );
        assert_eq!(Role::Ok.sgr(Depth::TrueColor).unwrap(), "38;2;61;220;132");
    }

    #[test]
    fn sixteen_colour_terminals_keep_the_codes_they_always_had() {
        // The fallback is the palette riabuild shipped before the brand colours
        // existed, so nothing regresses on a terminal that cannot do better.
        assert_eq!(Role::Ok.sgr(Depth::Ansi16).unwrap(), "32");
        assert_eq!(Role::Busy.sgr(Depth::Ansi16).unwrap(), "33");
        assert_eq!(Role::Danger.sgr(Depth::Ansi16).unwrap(), "1;31");
        assert_eq!(Role::Muted.sgr(Depth::Ansi16).unwrap(), "2");
    }

    #[test]
    fn muted_stays_an_attribute_so_it_survives_a_light_theme() {
        // A fixed grey cannot be legible on both a light and a dark terminal;
        // dim is whatever the developer's own theme says it is.
        for depth in [Depth::Ansi16, Depth::Ansi256, Depth::TrueColor] {
            assert_eq!(Role::Muted.sgr(depth).unwrap(), "2");
            assert_eq!(Role::Strong.sgr(depth).unwrap(), "1");
        }
    }

    #[test]
    fn the_brand_maps_onto_a_sensible_256_colour_index() {
        // 203 is the cube's nearest salmon-red. The point of the assertion is
        // that it lands in the cube rather than on the grey ramp.
        let index = xterm256(BRAND);
        assert!((16..232).contains(&index), "{index}");
        assert_eq!(xterm256(Rgb(0, 0, 0)), 16);
        assert_eq!(xterm256(Rgb(255, 255, 255)), 231);
    }

    #[test]
    fn a_mid_grey_lands_on_the_grey_ramp_not_the_cube() {
        let index = xterm256(Rgb(0x80, 0x80, 0x80));
        assert!((232..=255).contains(&index), "{index}");
    }

    #[test]
    fn a_ramp_starts_and_ends_on_its_endpoints() {
        let steps = ramp(PINK, BRAND, 6);
        assert_eq!(steps.len(), 6);
        assert_eq!(steps[0], PINK);
        assert_eq!(steps[5], BRAND);
        // and moves monotonically between them on every channel
        for pair in steps.windows(2) {
            assert!(pair[1].0 >= pair[0].0, "{pair:?}");
            assert!(pair[1].2 <= pair[0].2, "{pair:?}");
        }
    }

    #[test]
    fn a_degenerate_ramp_does_not_divide_by_zero() {
        assert_eq!(ramp(PINK, BRAND, 0), Vec::new());
        assert_eq!(ramp(PINK, BRAND, 1), vec![PINK]);
    }

    #[test]
    fn below_256_colours_the_gradient_collapses_onto_the_brand() {
        let flat = Theme::with_depth(Depth::Ansi16);
        assert_eq!(flat.paint_rgb(PINK, "x"), flat.paint(Role::Brand, "x"));
    }
}

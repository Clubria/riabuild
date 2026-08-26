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
//!
//! # Why ratatui's types
//!
//! [`Color`], [`Style`] and [`Modifier`] are re-exported from `ratatui-core`
//! rather than defined here, because riabuild now paints two very different
//! surfaces and they must not drift apart. `riabuild-ui` prints lines to a
//! terminal it does not own; `riabuild-agents` draws a full-screen frame it
//! does. A private `Rgb` would have meant every colour crossing into the TUI
//! being converted at the boundary, and a converted palette is a second palette
//! — the exact failure the "by role, never by escape code" rule exists to stop.
//!
//! What ratatui does **not** bring is the reason this crate still exists, and
//! it is the whole of the interesting part. Ratatui has no notion of how much
//! colour a terminal can render: its backends write [`Color::Rgb`] out as a
//! 24-bit escape whatever is on the other end, and it has no `NO_COLOR`. So the
//! ladder below is riabuild's, applied *before* a style reaches ratatui, and
//! [`Theme::paint`] is riabuild's too — there is no ratatui API that renders one
//! styled string for a `println!`, because ratatui only ever writes whole
//! frames. Both halves are tested here.
//!
//! This crate deliberately depends on `ratatui-core` and not on `ratatui`. It
//! is the palette and nothing else, and `riabuild-fetch` and `riabuild-runner`
//! — which paint lines and never draw a frame — should not acquire a widget set
//! and a backend by depending on colour.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct — but the exemption is `test` and nothing else, and must stay that
// way.
//
// It read `any(test, feature = "testing")`, which switched the lint off for
// this crate's *production* code under the one command that enforces it.
// `cargo clippy --workspace --all-targets` resolves dev-dependencies, a
// dev-dependency somewhere in the workspace asks for `testing`, and features
// unify onto the lib target — so the whole crate compiled with the allow on.
// With `test` alone the lib target is linted again, and the unit-test target
// that keeps the allow holds no production code the lib target does not.
//
// Scaffolding behind `feature = "testing"` carries its own allow where it is
// defined, which is a hole the size of a module rather than of a crate.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

pub use ratatui_core::style::{Color, Modifier, Style};

/// The Clubria logo mark's fill. The primary brand colour.
pub const BRAND: Color = Color::Rgb(0xf7, 0x4f, 0x25);
/// `--pink`. The far end of the site's accent gradient.
pub const PINK: Color = Color::Rgb(0xe6, 0x4a, 0xa0);
/// `--orange`. Brand-adjacent, one step cooler than [`BRAND`].
pub const ORANGE: Color = Color::Rgb(0xf0, 0x56, 0x3c);
/// `--green`. Reserved for "this is done".
pub const GREEN: Color = Color::Rgb(0x3d, 0xdc, 0x84);

/// The channels of a colour that has them.
///
/// [`Color`] is a sum over four different ways of naming a colour and only
/// [`Color::Rgb`] carries numbers, so every function here that does colour
/// *arithmetic* — the gradient, the 256-cube search — has to say what it does
/// about the rest. It returns `None`, and each caller decides: a ramp between
/// two named colours is not a ramp, and there is no honest answer to invent.
fn channels(colour: Color) -> Option<(u8, u8, u8)> {
    match colour {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| {
        let delta = x as i32 - y as i32;
        (delta * delta) as u32
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

fn lerp(from: (u8, u8, u8), to: (u8, u8, u8), numerator: usize, denominator: usize) -> Color {
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
    Color::Rgb(mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// How much colour this terminal can render.
///
/// Ratatui has no equivalent and needs one: a `Color::Rgb` handed to any of its
/// backends is written as a 24-bit escape sequence, on a terminal that may have
/// told us it can only do sixteen.
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
///
/// A colour with no channels — one of ratatui's sixteen names, or an index
/// already — has no cube coordinate to look for, so it is returned unchanged
/// where it is already an index and given up on otherwise.
pub fn xterm256(colour: Color) -> Option<u8> {
    if let Color::Indexed(index) = colour {
        return Some(index);
    }
    let rgb = channels(colour)?;
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |value: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| (**level as i32 - value as i32).abs())
            .map(|(index, _)| index)
            .unwrap_or(0)
    };
    let (r, g, b) = (nearest(rgb.0), nearest(rgb.1), nearest(rgb.2));
    let cube = (LEVELS[r], LEVELS[g], LEVELS[b]);
    let cube_index = 16 + 36 * r + 6 * g + b;

    let average = (rgb.0 as u32 + rgb.1 as u32 + rgb.2 as u32) / 3;
    let step = (average.saturating_sub(8) / 10).min(23) as usize;
    let level = (8 + 10 * step) as u8;
    let grey = (level, level, level);

    if distance(grey, rgb) < distance(cube, rgb) {
        Some((232 + step) as u8)
    } else {
        Some(cube_index as u8)
    }
}

/// `steps` colours walking from `from` to `to`, inclusive of both ends.
///
/// Both ends must carry channels; a ramp between two *named* colours is not a
/// ramp, and inventing one would put a gradient on screen that no palette
/// chose. Such a call flattens onto `from`, which is what the caller would have
/// drawn anyway at a depth too low for a gradient.
pub fn ramp(from: Color, to: Color, steps: usize) -> Vec<Color> {
    let (Some(start), Some(end)) = (channels(from), channels(to)) else {
        return vec![from; steps];
    };
    match steps {
        0 => Vec::new(),
        1 => vec![from],
        _ => (0..steps)
            .map(|step| lerp(start, end, step, steps - 1))
            .collect(),
    }
}

/// The sixteen colours every terminal has, with the channel values xterm gives
/// them, for finding the nearest one to an arbitrary [`Color::Rgb`].
///
/// Only the eight non-bright entries are candidates. The bright half is
/// reachable in SGR only as a colour *or* as bold-plus-colour depending on the
/// terminal, and riabuild uses bold to mean emphasis — so a downgrade that
/// picked one would either lose the distinction or spend it.
const ANSI16: [(Color, (u8, u8, u8)); 8] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (205, 0, 0)),
    (Color::Green, (0, 205, 0)),
    (Color::Yellow, (205, 205, 0)),
    (Color::Blue, (0, 0, 238)),
    (Color::Magenta, (205, 0, 205)),
    (Color::Cyan, (0, 205, 205)),
    (Color::White, (229, 229, 229)),
];

/// The nearest of the original eight to `colour`.
fn nearest_ansi16(colour: Color) -> Color {
    let Some(rgb) = channels(colour) else {
        return colour;
    };
    ANSI16
        .iter()
        .min_by_key(|(_, candidate)| distance(*candidate, rgb))
        .map(|(named, _)| *named)
        .unwrap_or(colour)
}

/// Rewrites one colour for a terminal that cannot render it as specified.
///
/// This is the step ratatui has no equivalent of, and the reason a [`Style`]
/// must pass through [`Theme::style`] before it reaches a frame.
pub fn lower_colour(colour: Color, depth: Depth) -> Option<Color> {
    match depth {
        Depth::None => None,
        Depth::TrueColor => Some(colour),
        Depth::Ansi256 => Some(match colour {
            Color::Rgb(..) => xterm256(colour).map(Color::Indexed).unwrap_or(colour),
            other => other,
        }),
        Depth::Ansi16 => Some(nearest_ansi16(colour)),
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
    /// This role at full fidelity, before any terminal has been consulted.
    ///
    /// Muted and Strong are attributes, not colours. Dim and bold adapt to
    /// whatever background and theme the developer has chosen, which a fixed
    /// grey cannot do — a `#8b8794` "muted" is invisible on a dark theme and
    /// muddy on a light one. They carry no `fg` at all, which is what makes
    /// them render identically at every depth.
    pub fn style(self) -> Style {
        match self {
            Role::Muted => Style::new().add_modifier(Modifier::DIM),
            Role::Strong => Style::new().add_modifier(Modifier::BOLD),
            Role::Brand | Role::Danger => Style::new().fg(BRAND).add_modifier(Modifier::BOLD),
            Role::Ok => Style::new().fg(GREEN),
            Role::Busy | Role::Warn => Style::new().fg(ORANGE),
        }
    }

    /// The sixteen-colour rendering, which is chosen rather than computed.
    ///
    /// [`nearest_ansi16`] would answer for these, and its answer is worse: the
    /// brand's `#f74f25` is nearest to `Red` on channel distance, but `Ok`'s
    /// `#3ddc84` lands on `Green` and `Busy`'s `#f0563c` on `Red` — putting
    /// "in progress" and "fatal" on the same colour. This table is the palette
    /// riabuild shipped before the brand colours existed, so nothing regresses
    /// on a terminal that cannot do better.
    fn legacy(self) -> Style {
        match self {
            Role::Muted => Style::new().add_modifier(Modifier::DIM),
            Role::Strong => Style::new().add_modifier(Modifier::BOLD),
            Role::Brand | Role::Danger => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            Role::Ok => Style::new().fg(Color::Green),
            Role::Busy | Role::Warn => Style::new().fg(Color::Yellow),
        }
    }

    /// This role as the given terminal should receive it.
    pub fn at(self, depth: Depth) -> Style {
        match depth {
            Depth::None => Style::new(),
            Depth::Ansi16 => self.legacy(),
            _ => {
                let style = self.style();
                match style.fg.and_then(|fg| lower_colour(fg, depth)) {
                    Some(fg) => style.fg(fg),
                    None => style,
                }
            }
        }
    }

    /// The SGR parameters for this role, or `None` for "write it plain".
    pub fn sgr(self, depth: Depth) -> Option<String> {
        sgr_of(self.at(depth))
    }
}

/// One [`Style`] as SGR parameters, or `None` where it would paint nothing.
///
/// Ratatui has no such function — a backend writes styles into a frame through
/// crossterm's own commands, and never produces a string — so this is riabuild's
/// and is what lets `riabuild-ui` keep printing ordinary lines while sharing one
/// palette with the TUI.
///
/// Modifiers come first so that a bold colour reads `1;38;2;…`, which is the
/// order every terminal documents and the order riabuild has always emitted.
pub fn sgr_of(style: Style) -> Option<String> {
    let mut params: Vec<String> = Vec::new();
    if style.add_modifier.contains(Modifier::BOLD) {
        params.push("1".to_string());
    }
    if style.add_modifier.contains(Modifier::DIM) {
        params.push("2".to_string());
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        params.push("3".to_string());
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        params.push("4".to_string());
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        params.push("7".to_string());
    }
    if let Some(fg) = style.fg {
        params.push(sgr_colour(fg, false)?);
    }
    if let Some(bg) = style.bg {
        params.push(sgr_colour(bg, true)?);
    }
    (!params.is_empty()).then(|| params.join(";"))
}

/// One colour as an SGR parameter. `ground` picks foreground or background.
fn sgr_colour(colour: Color, ground: bool) -> Option<String> {
    // The named eight are 30–37 in the foreground and 40–47 in the background;
    // the bright eight are 90–97 and 100–107.
    let offset = if ground { 10 } else { 0 };
    let named = |base: u32| Some((base + offset).to_string());
    match colour {
        Color::Reset => None,
        Color::Black => named(30),
        Color::Red => named(31),
        Color::Green => named(32),
        Color::Yellow => named(33),
        Color::Blue => named(34),
        Color::Magenta => named(35),
        Color::Cyan => named(36),
        Color::Gray => named(37),
        Color::DarkGray => named(90),
        Color::LightRed => named(91),
        Color::LightGreen => named(92),
        Color::LightYellow => named(93),
        Color::LightBlue => named(94),
        Color::LightMagenta => named(95),
        Color::LightCyan => named(96),
        Color::White => named(97),
        Color::Indexed(index) => Some(format!("{}8;5;{index}", 3 + offset / 10)),
        Color::Rgb(r, g, b) => Some(format!("{}8;2;{r};{g};{b}", 3 + offset / 10)),
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

    /// What this terminal can render, for a caller that has to branch on it.
    pub fn depth(self) -> Depth {
        self.depth
    }

    /// A role's [`Style`], lowered onto this terminal.
    ///
    /// This is what a ratatui widget asks for. Ratatui will happily render the
    /// undegraded [`Role::style`], which is exactly the bug — a `Color::Rgb`
    /// reaches a sixteen-colour terminal as a 24-bit escape it does not
    /// understand.
    pub fn style(self, role: Role) -> Style {
        role.at(self.depth)
    }

    /// An arbitrary style lowered onto this terminal, for the colours a widget
    /// chooses that no role covers — a sparkline, a diff, a session's own hue.
    pub fn lower(self, style: Style) -> Style {
        if self.depth == Depth::None {
            return Style::new();
        }
        let mut lowered = style;
        lowered.fg = style.fg.and_then(|fg| lower_colour(fg, self.depth));
        lowered.bg = style.bg.and_then(|bg| lower_colour(bg, self.depth));
        lowered
    }

    pub fn paint(self, role: Role, text: &str) -> String {
        self.paint_style(role.at(self.depth), text)
    }

    /// Paints an exact colour, for the banner gradient.
    ///
    /// Below 256 colours there is no gradient to be had, so every step
    /// collapses onto the brand — deliberately one flat brand-coloured mark
    /// rather than a banded approximation of a smooth one.
    pub fn paint_rgb(self, colour: Color, text: &str) -> String {
        match self.depth {
            Depth::None => text.to_string(),
            Depth::Ansi16 => self.paint(Role::Brand, text),
            depth => match lower_colour(colour, depth) {
                Some(lowered) => self.paint_style(Style::new().fg(lowered), text),
                None => text.to_string(),
            },
        }
    }

    /// Paints an already-lowered style. The one place an escape is written.
    fn paint_style(self, style: Style, text: &str) -> String {
        match sgr_of(style) {
            Some(sgr) => format!("\x1b[{sgr}m{text}\x1b[0m"),
            None => text.to_string(),
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
        assert_eq!(BRAND, Color::Rgb(0xf7, 0x4f, 0x25));
        assert_eq!(PINK, Color::Rgb(0xe6, 0x4a, 0xa0));
        assert_eq!(GREEN, Color::Rgb(0x3d, 0xdc, 0x84));
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
            assert_eq!(Role::Muted.at(depth).fg, None, "{depth:?}");
        }
    }

    #[test]
    fn the_brand_maps_onto_a_sensible_256_colour_index() {
        // 203 is the cube's nearest salmon-red. The point of the assertion is
        // that it lands in the cube rather than on the grey ramp.
        let index = xterm256(BRAND).unwrap();
        assert!((16..232).contains(&index), "{index}");
        assert_eq!(xterm256(Color::Rgb(0, 0, 0)).unwrap(), 16);
        assert_eq!(xterm256(Color::Rgb(255, 255, 255)).unwrap(), 231);
    }

    #[test]
    fn a_mid_grey_lands_on_the_grey_ramp_not_the_cube() {
        let index = xterm256(Color::Rgb(0x80, 0x80, 0x80)).unwrap();
        assert!((232..=255).contains(&index), "{index}");
    }

    #[test]
    fn a_colour_with_no_channels_has_no_cube_coordinate() {
        // `Color::Red` is a name a terminal resolves, not a value — there is no
        // honest 256-cube answer for it, and inventing one would be picking a
        // colour the palette never chose.
        assert_eq!(xterm256(Color::Red), None);
        assert_eq!(xterm256(Color::Indexed(42)).unwrap(), 42);
    }

    #[test]
    fn a_ramp_starts_and_ends_on_its_endpoints() {
        let steps = ramp(PINK, BRAND, 6);
        assert_eq!(steps.len(), 6);
        assert_eq!(steps[0], PINK);
        assert_eq!(steps[5], BRAND);
        // and moves monotonically between them on every channel
        for pair in steps.windows(2) {
            let (a, b) = (channels(pair[0]).unwrap(), channels(pair[1]).unwrap());
            assert!(b.0 >= a.0, "{pair:?}");
            assert!(b.2 <= a.2, "{pair:?}");
        }
    }

    #[test]
    fn a_degenerate_ramp_does_not_divide_by_zero() {
        assert_eq!(ramp(PINK, BRAND, 0), Vec::new());
        assert_eq!(ramp(PINK, BRAND, 1), vec![PINK]);
    }

    #[test]
    fn a_ramp_between_named_colours_is_flat_rather_than_invented() {
        // There is no arithmetic between two names, and a gradient nobody chose
        // is worse than no gradient.
        assert_eq!(ramp(Color::Red, Color::Blue, 3), vec![Color::Red; 3]);
    }

    #[test]
    fn below_256_colours_the_gradient_collapses_onto_the_brand() {
        let flat = Theme::with_depth(Depth::Ansi16);
        assert_eq!(flat.paint_rgb(PINK, "x"), flat.paint(Role::Brand, "x"));
    }

    #[test]
    fn a_widget_style_is_lowered_before_it_reaches_a_frame() {
        // The whole reason this crate still exists on top of ratatui: ratatui
        // would write the 24-bit escape to a terminal that cannot read it.
        let sixteen = Theme::with_depth(Depth::Ansi16);
        assert_eq!(sixteen.style(Role::Ok).fg, Some(Color::Green));

        let indexed = Theme::with_depth(Depth::Ansi256);
        assert!(matches!(
            indexed.style(Role::Ok).fg,
            Some(Color::Indexed(_))
        ));

        let full = Theme::with_depth(Depth::TrueColor);
        assert_eq!(full.style(Role::Ok).fg, Some(GREEN));
    }

    #[test]
    fn no_colour_erases_a_style_rather_than_passing_it_through() {
        // A widget that hardcoded a colour must still respect NO_COLOR, so the
        // erasure happens here and not at the call site.
        let plain = Theme::plain();
        let loud = Style::new().fg(BRAND).bg(PINK);
        assert_eq!(plain.lower(loud), Style::new());
        assert_eq!(plain.style(Role::Danger), Style::new());
    }

    #[test]
    fn an_arbitrary_widget_colour_walks_the_same_ladder_as_a_role() {
        let sixteen = Theme::with_depth(Depth::Ansi16);
        // Backgrounds walk it too — a pane header sets one.
        assert_eq!(
            sixteen.lower(Style::new().bg(BRAND)).bg,
            Some(nearest_ansi16(BRAND))
        );
        // Truecolor leaves an arbitrary colour exactly as the widget chose it.
        let full = Theme::with_depth(Depth::TrueColor);
        assert_eq!(full.lower(Style::new().fg(PINK)).fg, Some(PINK));
    }

    #[test]
    fn nearest_match_is_why_a_roles_sixteen_colour_palette_is_chosen_by_hand() {
        // `--green` is `#3ddc84`, a mint with far more blue in it than the
        // original `Green` has: it is nearer to `Cyan` on channel distance, and
        // `--orange` (#f0563c) is nearest to `Red` — which is also where
        // `Danger` lands. Left to arithmetic, "done" would go cyan and "in
        // progress" would become indistinguishable from "fatal".
        //
        // So `Role::legacy` is a table, and this test is the reason standing in
        // the file rather than only in the comment above it.
        assert_eq!(nearest_ansi16(GREEN), Color::Cyan);
        assert_eq!(nearest_ansi16(ORANGE), Color::Red);
        assert_eq!(nearest_ansi16(BRAND), Color::Red);

        assert_eq!(Role::Ok.at(Depth::Ansi16).fg, Some(Color::Green));
        assert_eq!(Role::Busy.at(Depth::Ansi16).fg, Some(Color::Yellow));
        assert_ne!(
            Role::Busy.at(Depth::Ansi16).fg,
            Role::Danger.at(Depth::Ansi16).fg
        );
    }

    #[test]
    fn a_background_renders_in_the_forties_not_the_thirties() {
        assert_eq!(sgr_of(Style::new().bg(Color::Red)).unwrap(), "41");
        assert_eq!(
            sgr_of(Style::new().bg(Color::Rgb(1, 2, 3))).unwrap(),
            "48;2;1;2;3"
        );
        assert_eq!(
            sgr_of(Style::new().bg(Color::Indexed(9))).unwrap(),
            "48;5;9"
        );
    }

    #[test]
    fn modifiers_precede_the_colour_they_apply_to() {
        // Every terminal documents this order, and riabuild has always emitted
        // it. A reversed pair renders on some terminals and not others.
        let style = Style::new().fg(BRAND).add_modifier(Modifier::BOLD);
        assert_eq!(sgr_of(style).unwrap(), "1;38;2;247;79;37");
    }

    #[test]
    fn an_empty_style_paints_nothing_at_all() {
        assert_eq!(sgr_of(Style::new()), None);
        assert_eq!(sgr_of(Style::new().fg(Color::Reset)), None);
    }
}

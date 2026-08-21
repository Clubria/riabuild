//! The line the environment shell opens with.

use riabuild_theme::{Role, Theme};

/// Printed once when the environment shell starts. Tells the developer they are
/// somewhere different and how to leave — and that launching an editor from
/// inside this shell is what makes the editor inherit the environment.
///
/// "Once" is the load-bearing word. This used to be printed by the parent
/// process *and* by the generated rcfile, so every developer saw it twice. The
/// rcfile is the one that keeps it: it runs after the developer's own config,
/// so the banner is the last thing on screen before the first prompt rather
/// than something their `.zshrc` output scrolls away.
pub const BANNER: &str =
    "● Clubria environment active — type `exit` to leave, `code .` to open your editor here";

const BULLET: &str = "●";
const HEADLINE: &str = "Clubria environment active";
const HINT: &str = "— type `exit` to leave, `code .` to open your editor here";

/// The remote counterpart to [`HINT`], named in `scope::Scope::banner` too —
/// `a_servers_banner_matches_between_colour_and_plain` guards the two from
/// drifting apart.
const REMOTE_HINT: &str = "— type `exit` to leave, `claude` to start working";

/// The banner with colour, matching what `Ui` does elsewhere: a green bullet
/// for a good state, and the trailing advice dimmed so the headline reads first.
///
/// The escapes are baked into the generated rcfile because that file, not
/// `Ui`, is what prints them — so the palette has to be threaded across that
/// boundary rather than re-derived inside the shell.
///
/// `server` is the name of the box this riabuild is managing, from
/// `Ctx::server` (see `scope.rs`) — `None` on a developer's own laptop. The
/// uncoloured text is `scope::Scope`'s own construction, read straight
/// through rather than re-formatted a second time, so there is exactly one
/// sentence for "the environment is active" and one for "it is active on
/// this named server".
pub fn banner(theme: Theme, server: Option<&str>) -> String {
    let plain = crate::scope::Scope::read(server).banner();
    if !theme.enabled() {
        return plain;
    }
    let bullet = theme.paint(Role::Ok, BULLET);
    match server {
        Some(name) => format!(
            "{bullet} Clubria environment active on {name} {}",
            theme.paint(Role::Muted, REMOTE_HINT)
        ),
        None => format!("{bullet} {HEADLINE} {}", theme.paint(Role::Muted, HINT)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_theme::Depth;

    #[test]
    fn the_coloured_banner_says_the_same_thing_as_the_plain_one() {
        // Two spellings of one sentence drift apart. This is what stops the
        // NO_COLOR path and the coloured path from disagreeing.
        assert_eq!(BANNER, format!("{BULLET} {HEADLINE} {HINT}"));
        assert_eq!(banner(Theme::plain(), None), BANNER);
    }

    #[test]
    fn colour_wraps_the_bullet_and_dims_the_advice() {
        let coloured = banner(Theme::with_depth(Depth::Ansi16), None);
        assert!(coloured.starts_with("\x1b[32m●\x1b[0m "), "{coloured:?}");
        assert!(coloured.contains("\x1b[2m— type `exit`"), "{coloured:?}");
        assert!(coloured.ends_with("\x1b[0m"), "{coloured:?}");
        // The words survive the escapes.
        assert!(coloured.contains(HEADLINE));
    }

    #[test]
    fn a_capable_terminal_gets_the_brand_green_not_the_ansi_one() {
        // The shell banner is baked into a generated rcfile, so it is the one
        // place the palette could silently stay on the old sixteen colours
        // while everything printed by `Ui` moved to the brand.
        let coloured = banner(Theme::with_depth(Depth::TrueColor), None);
        assert!(
            coloured.starts_with("\x1b[38;2;61;220;132m●\x1b[0m "),
            "{coloured:?}"
        );
    }

    #[test]
    fn a_laptop_banner_is_unchanged_byte_for_byte() {
        // The whole reason `server` is a parameter and not a rewrite: a
        // laptop's banner — the case every existing developer sees — must be
        // exactly what it was before remote mode existed.
        assert_eq!(banner(Theme::plain(), None), BANNER);
    }

    #[test]
    fn a_servers_banner_names_it_in_both_variants() {
        let plain = banner(Theme::plain(), Some("build-01"));
        let coloured = banner(Theme::with_depth(Depth::Ansi16), Some("build-01"));
        assert!(plain.contains("build-01"), "{plain}");
        assert!(coloured.contains("build-01"), "{coloured}");
        assert!(coloured.starts_with("\x1b[32m●\x1b[0m "), "{coloured:?}");
    }

    #[test]
    fn a_servers_banner_matches_between_colour_and_plain() {
        // REMOTE_HINT (used by the coloured path) and scope::Scope::banner
        // (used by the plain path) are two spellings of one sentence — this
        // is what stops them drifting apart the way BANNER and HINT are
        // guarded above.
        let plain = banner(Theme::plain(), Some("build-01"));
        let coloured = banner(Theme::with_depth(Depth::Ansi16), Some("build-01"));
        assert!(plain.contains("`exit` to leave, `claude` to start working"));
        assert!(coloured.contains("`exit` to leave, `claude` to start working"));
    }
}

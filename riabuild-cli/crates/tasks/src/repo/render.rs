//! The repositories box: what the picker numbers its question against.
//!
//! The theme is a parameter rather than something read from `Ui`, as in
//! `remote/render.rs` and `accounts/render.rs`: nothing here decides whether
//! this terminal gets colour, it is told.

use riabuild_api::Repo;
use riabuild_theme::{Role, Theme};

/// How many repositories the box draws.
///
/// Ten is what a developer can read at a prompt without scrolling their first
/// screen of a run. The rest are not hidden — the line under the box says how
/// many there are and that a name can be typed — and the ordering puts the ones
/// this machine actually works with above the cut.
pub const SHOWN: usize = 10;

/// One row of the box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub repo: Repo,
    /// Seconds since the epoch, or 0 when GitHub did not say — which is also
    /// every row that came from this machine's own list of checkouts rather than
    /// from the listing.
    pub pushed_at: u64,
    /// This machine has a checkout of it.
    pub cloned: bool,
    /// The org default, which is what Enter means on a machine that has never
    /// chosen.
    pub default: bool,
}

/// `heading` rather than the org name, because the same box answers two
/// different questions: which repository to work on, and which checkout to move.
/// Naming the org above a list of this machine's own checkouts would be the wrong
/// label on the right list.
pub fn repos_box(heading: &str, rows: &[Row], hidden: usize, now: u64, theme: Theme) -> String {
    let mut lines = vec![
        theme.paint(Role::Strong, &format!("{heading}:")),
        String::new(),
    ];

    // Measured before anything is painted: an escape sequence occupies no
    // terminal columns but plenty of `chars()`, so padding computed over painted
    // text lines up on nothing.
    let name_width = width(rows.iter().map(|row| row.repo.name().to_string()));
    let number_width = rows.len().to_string().chars().count();

    for (index, row) in rows.iter().enumerate() {
        let notes = notes(row, now);
        lines.push(format!(
            "  {:>number_width$}  {:<name_width$}   {}",
            index + 1,
            row.repo.name(),
            theme.paint(Role::Muted, &notes),
        ));
    }

    if hidden > 0 {
        lines.push(String::new());
        lines.push(theme.paint(
            Role::Muted,
            &format!("  … {hidden} more — type a name to work on one of those"),
        ));
    }

    lines.join("\n")
}

/// What is worth saying about a row beyond its name, in the order a developer
/// would want it.
///
/// `cloned` comes before the timestamp because it is the only part of a row that
/// says what picking it will *cost*: a repository already checked out here is
/// picked with no clone and no path question.
fn notes(row: &Row, now: u64) -> String {
    let mut notes = Vec::new();
    if row.default {
        notes.push("default".to_string());
    }
    if row.cloned {
        notes.push("cloned".to_string());
    }
    if row.pushed_at > 0 {
        notes.push(format!("pushed {}", pushed_words(now, row.pushed_at)));
    }
    notes.join(" · ")
}

/// How long ago something happened, in one short column.
///
/// `ui::duration_words` is the wrong shape here: it spells a duration in full
/// ("2 days 3 hours 5 minutes"), which is right for "this took" and too wide for
/// a column repeated down ten rows.
///
/// A future timestamp reads as "just now" rather than as a negative age. Clocks
/// disagree, and a laptop a few seconds behind GitHub must not draw `pushed
/// 18446744073709551614s ago`.
pub fn pushed_words(now: u64, then: u64) -> String {
    let seconds = now.saturating_sub(then);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    match (minutes, hours, days) {
        (0, _, _) => "just now".to_string(),
        (minutes, 0, _) => format!("{minutes}m ago"),
        (_, hours, 0) => format!("{hours}h ago"),
        (_, _, days) if days < 30 => format!("{days}d ago"),
        (_, _, days) if days < 365 => format!("{}mo ago", days / 30),
        (_, _, days) => format!("{}y ago", days / 365),
    }
}

fn width(values: impl Iterator<Item = String>) -> usize {
    values.map(|value| value.chars().count()).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_755_000_000;
    const HOUR: u64 = 3_600;

    fn row(name: &str, cloned: bool, default: bool, pushed_at: u64) -> Row {
        Row {
            repo: Repo::parse(&format!("Clubria/{name}")).expect("parses"),
            pushed_at,
            cloned,
            default,
        }
    }

    #[test]
    fn a_row_is_numbered_named_and_annotated() {
        let rows = [
            row("ai-builders-hub", true, true, NOW - 2 * HOUR),
            row("payments", false, false, NOW - 50 * HOUR),
        ];
        let drawn = repos_box("Clubria repositories", &rows, 0, NOW, Theme::plain());

        assert!(drawn.contains("Clubria repositories:"), "{drawn}");
        assert!(
            drawn.contains("1  ai-builders-hub   default · cloned · pushed 2h ago"),
            "{drawn}"
        );
        assert!(drawn.contains("2  payments"), "{drawn}");
        assert!(drawn.contains("pushed 2d ago"), "{drawn}");
        // Nothing was left out, so nothing claims anything was.
        assert!(!drawn.contains("more"), "{drawn}");
    }

    #[test]
    fn the_repositories_past_the_cut_are_counted_and_named_reachable() {
        // Silent truncation would read as "this is all of them", which is the
        // one thing the box must not imply about an org list capped at ten.
        let rows = [row("payments", false, false, NOW)];
        let drawn = repos_box("Clubria repositories", &rows, 6, NOW, Theme::plain());
        assert!(drawn.contains("… 6 more — type a name"), "{drawn}");
    }

    #[test]
    fn a_row_this_machine_knows_but_github_did_not_mention_says_nothing_about_pushes() {
        // A checkout of a repository outside the listing — another owner's, or
        // past the first page. It is still pickable, and inventing an age for it
        // would be a lie in a column.
        let rows = [row("payments", true, false, 0)];
        let drawn = repos_box("Clubria repositories", &rows, 0, NOW, Theme::plain());
        assert!(drawn.contains("cloned"), "{drawn}");
        assert!(!drawn.contains("pushed"), "{drawn}");
    }

    #[test]
    fn ages_read_the_way_a_developer_would_say_them() {
        assert_eq!(pushed_words(NOW, NOW), "just now");
        assert_eq!(pushed_words(NOW, NOW - 90), "1m ago");
        assert_eq!(pushed_words(NOW, NOW - 2 * HOUR), "2h ago");
        assert_eq!(pushed_words(NOW, NOW - 3 * 24 * HOUR), "3d ago");
        assert_eq!(pushed_words(NOW, NOW - 60 * 24 * HOUR), "2mo ago");
        assert_eq!(pushed_words(NOW, NOW - 800 * 24 * HOUR), "2y ago");
    }

    #[test]
    fn a_clock_behind_githubs_does_not_draw_an_age_from_the_far_future() {
        assert_eq!(pushed_words(NOW, NOW + 5 * HOUR), "just now");
    }
}

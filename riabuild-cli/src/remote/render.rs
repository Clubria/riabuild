//! The servers box: what `riabuild remote` picks from, and what
//! `riabuild remote list` shows.
//!
//! One renderer for both, so a server reads the same way wherever it appears
//! and there is one place to change how. The two surfaces differ only in what
//! each already answers for itself: the picker numbers its rows, because a
//! number is what the question below it is read against, and carries "Add a
//! server" as the option after the last one — where the list carries it as a
//! hint instead.
//!
//! The theme is a parameter rather than something read from `Ui`, the same as
//! `accounts/render.rs`: nothing here decides whether this terminal gets
//! colour, it is told.

use super::store::Record;
use crate::theme::{Role, Theme};

/// Which surface the box is being drawn for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// `riabuild remote`'s prompt.
    Choosing,
    /// `riabuild remote list`.
    Listing,
}

/// The number that means "add a server", given how many are saved.
///
/// One definition, shared with the picker: the box prints this number and the
/// picker accepts it, and the two drifting apart would offer an option that
/// does nothing when typed.
pub fn add_option(count: usize) -> usize {
    count + 1
}

/// The index of the server a bare Enter should connect to — the most recently
/// used one.
///
/// Shared with the picker deliberately, rather than each working it out: the
/// prompt's default and the "connect without asking" hint name the same
/// server, which is what makes the hint a demonstration of what Enter just
/// did rather than a note about syntax. `None` only for an empty list.
///
/// "Used" means *connected to successfully*: `store::remember` writes
/// `last_used_at` only after the server's own check run came back 0, so a
/// server that was reached yesterday and failed to provision keeps whatever
/// timestamp it had. That is the right reading for a default — it offers the
/// server that last worked, not the one last attempted.
///
/// The index breaks ties, so the *later* record wins when two servers are
/// equally recent — which is every server on a laptop where none has ever
/// connected, all of them sitting at 0. Records are appended in the order they
/// were added, so that offers the one just typed in rather than the oldest
/// one saved. Written into the key rather than left to `max_by_key`'s
/// documented last-wins, because a later tidy-up that reached for `fold` or a
/// sort would flip it silently.
pub fn most_recently_used(records: &[Record]) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .max_by_key(|(index, record)| (record.last_used_at, *index))
        .map(|(index, _)| index)
}

/// The index of the stalest server — the one the forget hint names.
///
/// Ties go the other way, to the earliest record, so that on an all-zero list
/// this and [`most_recently_used`] still name two different servers and the
/// two hints keep teaching two different things.
fn least_recently_used(records: &[Record]) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .min_by_key(|(index, record)| (record.last_used_at, *index))
        .map(|(index, _)| index)
}

pub fn servers_box(records: &[Record], shown: Shown, theme: Theme) -> String {
    let mut lines = vec![theme.paint(Role::Strong, "Your servers:"), String::new()];

    // Every column is measured before any of it is painted: an escape sequence
    // occupies no terminal columns but plenty of `chars()`, so padding computed
    // over painted text lines up on nothing.
    let name_width = width(records.iter().map(|record| record.name.clone()));
    let login_width = width(records.iter().map(login));
    let number_width = add_option(records.len()).to_string().chars().count();

    for (index, record) in records.iter().enumerate() {
        let row = format!(
            "{:<name_width$}   {:<login_width$}   {}",
            record.name,
            login(record),
            theme.paint(Role::Muted, &used(record)),
        );
        match shown {
            Shown::Choosing => {
                lines.push(format!("  {:>number_width$}  {row}", index + 1));
            }
            Shown::Listing => lines.push(format!("  {row}")),
        }
    }
    if shown == Shown::Choosing {
        lines.push(format!(
            "  {:>number_width$}  Add a server",
            add_option(records.len())
        ));
    }

    let hints = hints(records, shown);
    if !hints.is_empty() {
        lines.push(String::new());
        let label_width = width(hints.iter().map(|(label, _)| label.clone()));
        for (label, command) in hints {
            let padding = " ".repeat(label_width - label.chars().count());
            lines.push(format!(
                "  {}{padding}  {command}",
                theme.paint(Role::Muted, &label)
            ));
        }
    }

    lines.join("\n")
}

/// The widest of a column's values, in terminal columns.
fn width(values: impl Iterator<Item = String>) -> usize {
    values.map(|value| value.chars().count()).max().unwrap_or(0)
}

/// `user@host`, with the port only when it is not the default one.
///
/// A port is part of a server's identity — `Remote::hash` covers it — so two
/// rows that differ only there have to be told apart. Printing `:22` on every
/// row would bury the one row where it matters.
fn login(record: &Record) -> String {
    if record.port == 22 {
        format!("{}@{}", record.user, record.host)
    } else {
        format!("{}@{}:{}", record.user, record.host, record.port)
    }
}

/// When this server was last connected to, in words.
///
/// `last_used_at` is 0 for a server that was added and never connected to —
/// which is what a first run that failed at the install step leaves behind, and
/// today's most likely outcome on Linux. Handed to `duration_words` that reads
/// as the whole epoch: `used 29873 days`.
fn used(record: &Record) -> String {
    if record.last_used_at == 0 {
        return "never connected".to_string();
    }
    // `duration_words` takes minutes elapsed, not a timestamp, and answers with
    // a length of time — so the column has to say what that length is measured
    // from, or "used 3 hours" reads as how long the session lasted.
    format!(
        "used {} ago",
        crate::ui::duration_words(
            crate::config::now_secs().saturating_sub(record.last_used_at) / 60
        )
    )
}

/// Only the commands that would succeed right now, on the servers in front of
/// the developer.
///
/// The rule comes from `accounts::render::hints`: a hint that refuses when typed reads
/// as riabuild being broken rather than as the developer having asked for
/// something impossible. So every name here comes off the records being shown,
/// never off their count and never a placeholder.
fn hints(records: &[Record], shown: Shown) -> Vec<(String, String)> {
    let mut hints = Vec::new();
    if let Some(index) = most_recently_used(records) {
        hints.push((
            match shown {
                Shown::Choosing => "Connect without asking:",
                Shown::Listing => "Connect to one:",
            }
            .to_string(),
            format!("riabuild remote {}", records[index].name),
        ));
    }
    // Only where it is not already an option on screen.
    if shown == Shown::Listing && !records.is_empty() {
        hints.push(("Add a server:".to_string(), "riabuild remote".to_string()));
    }
    if let Some(index) = least_recently_used(records) {
        hints.push((
            "Forget a server:".to_string(),
            format!("riabuild remote forget {}", records[index].name),
        ));
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::Remote;
    use crate::remote::store::record_for;
    use crate::theme::Depth;

    fn remote(name: &str, host: &str, port: u16) -> Remote {
        Remote {
            name: name.into(),
            host: host.into(),
            port,
            user: "ada".into(),
        }
    }

    /// Two servers, one used three hours ago and one five days ago, so every
    /// hint that picks a server by recency has two to choose between.
    fn two() -> Vec<Record> {
        let mut fresh = record_for(&remote("build-01", "build-01.fly.dev", 22));
        fresh.last_used_at = crate::config::now_secs().saturating_sub(3 * 3600);
        let mut stale = record_for(&remote("gpu", "gpu.internal", 2222));
        stale.last_used_at = crate::config::now_secs().saturating_sub(5 * 86400);
        vec![fresh, stale]
    }

    #[test]
    fn every_server_is_listed_with_its_login_and_when_it_was_last_used() {
        let text = servers_box(&two(), Shown::Listing, Theme::plain());
        assert!(text.contains("Your servers:"), "{text}");
        assert!(text.contains("build-01"), "{text}");
        assert!(text.contains("ada@build-01.fly.dev"), "{text}");
        // "used 3 hours" is a duration, not a time — the column is answering
        // "when", so it has to read as one.
        assert!(text.contains("used 3 hours ago"), "{text}");
        assert!(text.contains("used 5 days ago"), "{text}");
    }

    #[test]
    fn a_nondefault_port_is_shown_and_the_default_one_is_not() {
        // The port is part of a server's identity, so a developer comparing two
        // rows has to be able to see which is which — but printing `:22` on
        // every row would make the one row that matters harder to spot.
        let text = servers_box(&two(), Shown::Listing, Theme::plain());
        assert!(text.contains("ada@gpu.internal:2222"), "{text}");
        assert!(!text.contains("build-01.fly.dev:22"), "{text}");
    }

    #[test]
    fn choosing_numbers_the_rows_and_offers_the_number_after_the_last_one() {
        let text = servers_box(&two(), Shown::Choosing, Theme::plain());
        assert!(text.contains("1  build-01"), "{text}");
        assert!(text.contains("2  gpu"), "{text}");
        assert!(text.contains("3  Add a server"), "{text}");
        assert_eq!(add_option(2), 3);
    }

    #[test]
    fn listing_numbers_nothing_and_offers_adding_as_a_hint_instead() {
        // There is no question under the list, so a number would be an
        // instruction to type something that nothing is waiting to read.
        let text = servers_box(&two(), Shown::Listing, Theme::plain());
        assert!(!text.contains("1  build-01"), "{text}");
        assert!(!text.contains("Add a server\n"), "{text}");
        assert!(
            text.contains("Add a server:     riabuild remote\n"),
            "{text}"
        );
    }

    #[test]
    fn a_server_that_has_never_connected_does_not_read_as_thirty_thousand_days() {
        // `last_used_at` is 0 for a server that was added and never
        // successfully connected to — which is what a failed first run leaves
        // behind. Handing that to `duration_words` renders the whole epoch.
        let records = vec![record_for(&remote("build-01", "build-01.fly.dev", 22))];
        let text = servers_box(&records, Shown::Listing, Theme::plain());
        assert!(text.contains("never connected"), "{text}");
        assert!(!text.contains("days"), "{text}");
    }

    #[test]
    fn the_connect_hint_names_the_server_enter_would_take() {
        // The hint is a demonstration of what the default just did, so the two
        // have to agree — which is why both read `most_recently_used`.
        let records = two();
        let text = servers_box(&records, Shown::Choosing, Theme::plain());
        assert_eq!(most_recently_used(&records), Some(0));
        assert!(
            text.contains("Connect without asking:  riabuild remote build-01"),
            "{text}"
        );
    }

    #[test]
    fn the_forget_hint_names_the_stalest_server_not_the_one_being_connected_to() {
        // Two hints naming one server teach the developer less than two
        // naming different ones: together they say "the name goes here"
        // without either line having to.
        let text = servers_box(&two(), Shown::Choosing, Theme::plain());
        assert!(
            text.contains("Forget a server:         riabuild remote forget gpu"),
            "{text}"
        );
    }

    #[test]
    fn every_hint_names_a_server_that_is_in_the_box() {
        // The `accounts::render::hints` rule: a hint that refuses when typed
        // reads as riabuild being broken rather than as the developer having
        // asked for something impossible. So the names come off the records
        // being shown, never off a count or a placeholder.
        let records = two();
        let text = servers_box(&records, Shown::Choosing, Theme::plain());
        for (_, command) in hints(&records, Shown::Choosing) {
            let named = command.split_whitespace().last().expect("a command");
            assert!(
                named == "remote" || records.iter().any(|record| record.name == named),
                "{command} names {named}, which is not a saved server: {text}"
            );
        }
    }

    #[test]
    fn with_nothing_ever_connected_to_the_newest_server_is_the_one_offered() {
        // Every `last_used_at` is 0 — two servers added, neither of which got
        // past the install step — so recency cannot separate them and the tie
        // rule is the whole answer. Records are appended in the order they were
        // added, so the last one is the one the developer just typed in, and
        // that is the better guess than the first they ever saved.
        //
        // Pinned rather than left to `max_by_key`'s documented last-wins: an
        // iterator swapped for `fold` or `sorted_by` during some later tidy-up
        // would flip it silently, and the developer would press Enter expecting
        // what the bracket said last time.
        let records = vec![
            record_for(&remote("build-01", "build-01.fly.dev", 22)),
            record_for(&remote("gpu", "gpu.internal", 22)),
        ];
        assert_eq!(most_recently_used(&records), Some(1));
        assert_eq!(least_recently_used(&records), Some(0));

        let text = servers_box(&records, Shown::Choosing, Theme::plain());
        assert!(text.contains("riabuild remote gpu"), "{text}");
        assert!(text.contains("riabuild remote forget build-01"), "{text}");
    }

    #[test]
    fn one_saved_server_is_named_by_both_hints() {
        // Honest rather than clever: it is the only server there is.
        let records = vec![record_for(&remote("build-01", "build-01.fly.dev", 22))];
        let text = servers_box(&records, Shown::Choosing, Theme::plain());
        assert!(text.contains("riabuild remote build-01"), "{text}");
        assert!(text.contains("riabuild remote forget build-01"), "{text}");
    }

    #[test]
    fn without_colour_there_are_no_escapes() {
        assert!(!servers_box(&two(), Shown::Choosing, Theme::plain()).contains('\x1b'));
        assert!(
            servers_box(&two(), Shown::Choosing, Theme::with_depth(Depth::TrueColor))
                .contains('\x1b')
        );
    }
}

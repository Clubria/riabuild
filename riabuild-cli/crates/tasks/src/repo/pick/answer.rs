//! What a typed answer meant, and what the box shows.
//!
//! Both pure, so every rule about an answer and every rule about the order of
//! the rows is testable without a test process reading real stdin.

use super::super::list::Entry;
use super::super::render::{Row, SHOWN};
use riabuild_api::Repo;
use std::collections::BTreeMap;

/// How many unusable answers are asked about again before riabuild takes the
/// default. The bound `project::choose_dir` and `remote::pick` already use, for
/// the reason they already give: a developer who cannot give a usable answer is
/// better served by riabuild choosing than by being asked forever.
pub(super) const ATTEMPTS: usize = 3;

/// What a typed answer meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Enter: the repository the question offered.
    Default,
    /// A number, as a zero-based index into the rows shown.
    Listed(usize),
    /// A name, which may be any repository this developer can see.
    Named(Repo),
}

/// What an answer means, given how many rows the box drew.
///
/// `Err` carries the objection to put to the developer before asking again —
/// `Repo::parse`'s own words when they typed a name, since it knows why the name
/// is unusable and this does not.
pub fn settle(answer: &str, shown: usize, default_owner: &str) -> Result<Answer, String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(Answer::Default);
    }

    // A number is read as a row before it is read as a name. Nothing is guessed
    // at from there: a number past the end is an objection, not the first row.
    if answer.chars().all(|character| character.is_ascii_digit()) {
        let number: usize = answer.parse().map_err(|_| {
            format!("{answer:?} is too long to be a row number — pick 1 to {shown}")
        })?;
        return match (1..=shown).contains(&number) {
            true => Ok(Answer::Listed(number - 1)),
            false => Err(format!(
                "there is no repository {number} in that list — pick 1 to {shown}, \
                 or type a name"
            )),
        };
    }

    Repo::parse_with_owner(answer, default_owner)
        .map(Answer::Named)
        .map_err(|error| format!("{error}"))
}

/// The rows the box draws, and how many repositories it left out.
///
/// The order is what makes a ten-row cut safe: the repository this run would
/// take by default first, then the ones this machine already has a checkout of,
/// then everything else by most recently pushed. So the rows above the cut are
/// the ones a developer works with, and the rows below it are ones they have
/// never touched from this machine.
pub fn rows_for(
    listing: &[Entry],
    known: &BTreeMap<String, String>,
    default: &Repo,
    org_default: &Repo,
) -> (Vec<Row>, usize) {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for entry in listing {
        rows.insert(
            entry.repo.slug().to_string(),
            Row {
                repo: entry.repo.clone(),
                pushed_at: entry.pushed_at,
                cloned: known.contains_key(entry.repo.slug()),
                default: entry.repo == *org_default,
            },
        );
    }

    // A checkout on this machine is always offered, even when the listing does
    // not mention it: another owner's repository, or one past the single page
    // asked for. Leaving it out would mean a developer could not get back to a
    // tree they are working in without typing its full slug.
    for slug in known.keys().chain(std::iter::once(
        &default.slug().to_string(), // whatever Enter would take is always shown
    )) {
        if let Ok(repo) = Repo::parse(slug) {
            rows.entry(slug.clone()).or_insert(Row {
                cloned: known.contains_key(slug),
                default: repo == *org_default,
                repo,
                pushed_at: 0,
            });
        }
    }

    let mut rows: Vec<Row> = rows.into_values().collect();
    rows.sort_by(|left, right| {
        let key = |row: &Row| {
            (
                row.repo != *default, // what Enter takes, first
                !row.cloned,          // then the trees already here
                std::cmp::Reverse(row.pushed_at),
                row.repo.slug().to_string(),
            )
        };
        key(left).cmp(&key(right))
    });

    let hidden = rows.len().saturating_sub(SHOWN);
    rows.truncate(SHOWN);
    (rows, hidden)
}

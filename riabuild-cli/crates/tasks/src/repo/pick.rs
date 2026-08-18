//! Which repository this run is about.
//!
//! Put once, at the top of a provisioning run, before any task has looked at a
//! checkout. The answer is the second thing riabuild *offers* rather than
//! imposes — the first being where the checkout goes — and for the same reason:
//! a developer who presses Enter has still decided nothing.
//!
//! Deciding and acting are separate here, as in `remote::pick`. [`settle`] and
//! [`rows_for`] are pure, so every rule about what an answer means and what the
//! box shows is testable without a test process ever reading real stdin — which
//! under `cargo test` run from a terminal would be a blocking read on the
//! developer's own keyboard.

use super::list::{self, Entry, Listing};
use super::render::{self, Row, SHOWN};
use crate::Ctx;
use anyhow::Result;
use riabuild_api::Repo;
use riabuild_paths::config::UserConfig;
use riabuild_ui::Ui;
use std::collections::BTreeMap;

/// How many unusable answers are asked about again before riabuild takes the
/// default. The bound `project::choose_dir` and `remote::pick` already use, for
/// the reason they already give: a developer who cannot give a usable answer is
/// better served by riabuild choosing than by being asked forever.
const ATTEMPTS: usize = 3;

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
                row.repo != *default,  // what Enter takes, first
                !row.cloned,           // then the trees already here
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

/// Which repository this run is about, asked if there is anybody to ask.
///
/// Writes the answer before returning, so every repository-scoped task in the
/// run that follows reads it from one place.
pub async fn choose(ctx: &mut Ctx) -> Result<Repo> {
    // What Enter takes: the repository this machine last worked on, and the org
    // default on a machine that has never chosen. Fallible only for a dashboard
    // slug nobody could clone, which is the one case worth stopping a
    // provisioning run for — see `OrgConfig::default_repo`.
    let default = ctx.repo()?;
    let org_default = ctx.org()?.default_repo()?;

    // Checked before the listing is fetched rather than after, so an unattended
    // run does not spend a GitHub round trip on a box nobody will see. Taking
    // the default here is the crate rule for `ask`, and the right rule for this
    // question: picking a repository is the decision riabuild would otherwise
    // have made alone. `remote::pick` refuses instead, because connecting
    // provisions a server — this does not.
    if !ctx.ui.interactive() {
        return adopt(ctx, default, &org_default).await;
    }

    let listing = list::fetch(ctx, org_default.owner()).await;
    let entries = match &listing {
        Listing::Repos(entries) => entries.as_slice(),
        Listing::NotYet => {
            ctx.ui.info(
                "riabuild has not installed GitHub sign-in on this machine yet, \
                 so it cannot list your repositories — it will next run.",
            );
            &[]
        }
        Listing::Unavailable(detail) => {
            // Named rather than swallowed: a developer who is offered one
            // repository has to be able to tell "that is all you can see" from
            // "we could not ask".
            ctx.ui
                .warn(&format!("Could not list your repositories — {detail}"));
            &[]
        }
    };

    let (rows, hidden) = rows_for(entries, &ctx.config.repos, &default, &org_default);
    if matches!(&listing, Listing::Repos(entries) if entries.is_empty()) {
        ctx.ui.info(&format!(
            "GitHub lists no repositories you can see in {}.",
            org_default.owner()
        ));
    }
    ctx.ui.info("");
    ctx.ui.info(&render::repos_box(
        org_default.owner(),
        &rows,
        hidden,
        now(),
        ctx.ui.theme(),
    ));

    let chosen = ask(&ctx.ui, &rows, &default, org_default.owner());
    adopt(ctx, chosen, &org_default).await
}

/// The question, and the three attempts it is put in.
fn ask(ui: &Ui, rows: &[Row], default: &Repo, default_owner: &str) -> Repo {
    // The default is named inside the question rather than only in the box
    // above it: `Ui::info` returns early under `--quiet` and `Ui::ask` does not,
    // so `riabuild --quiet` puts this question with the box silently dropped.
    // The same reason `remote::pick::settle` names the server in its prompt.
    let question = format!("Which repository? (press enter for {default})");
    for _ in 0..ATTEMPTS {
        // `None` is Enter, ^D, or nobody there, and all three mean "the one you
        // offered" — so none of them costs the developer an attempt.
        let Some(answer) = ui.ask(&question) else {
            break;
        };
        match settle(&answer, rows.len(), default_owner) {
            Ok(Answer::Default) => break,
            // In range by construction: `settle` only ever reports a row it was
            // told about.
            Ok(Answer::Listed(index)) => return rows[index].repo.clone(),
            Ok(Answer::Named(repo)) => return repo,
            Err(objection) => ui.warn(&objection),
        }
    }
    default.clone()
}

/// Records the repository this run is about, and migrates the checkout an older
/// riabuild left behind.
///
/// The migration happens here because this is the first place both facts are
/// known: the path in `config.json`, and the repository it must be a checkout of
/// — the org default, because it is the only repository riabuild could have
/// cloned before it asked. Both go in one write, so a run that is interrupted
/// between them cannot leave a machine that has adopted nothing and forgotten
/// where its checkout was.
pub async fn adopt(ctx: &mut Ctx, chosen: Repo, org_default: &Repo) -> Result<Repo> {
    let (slug, default_slug) = (chosen.slug().to_string(), org_default.slug().to_string());
    // Under `--check` nothing is written: a dry run must leave the machine as it
    // found it, and `config.json` is part of "as it found it".
    if !ctx.dry_run {
        ctx.update_config(|config: &mut UserConfig| {
            config.adopt_legacy_checkout(&default_slug);
            config.active_repo = Some(slug);
        })
        .await?;
    }
    ctx.repo = Some(chosen.clone());
    Ok(chosen)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_and_runner, install_owned_tools, org_config};
    use riabuild_runner::FakeRunner;

    const NOW: u64 = 1_755_000_000;

    fn repo(slug: &str) -> Repo {
        Repo::parse(slug).expect("parses")
    }

    fn entry(slug: &str, pushed_at: u64) -> Entry {
        Entry {
            repo: repo(slug),
            pushed_at,
        }
    }

    fn known(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(slug, path)| (slug.to_string(), path.to_string()))
            .collect()
    }

    #[test]
    fn enter_means_the_repository_the_question_offered() {
        assert_eq!(settle("", 3, "Clubria"), Ok(Answer::Default));
        assert_eq!(settle("   ", 3, "Clubria"), Ok(Answer::Default));
    }

    #[test]
    fn a_number_picks_the_row_it_names() {
        assert_eq!(settle("1", 3, "Clubria"), Ok(Answer::Listed(0)));
        assert_eq!(settle(" 3 ", 3, "Clubria"), Ok(Answer::Listed(2)));
    }

    #[test]
    fn a_number_outside_the_box_is_an_objection_not_the_first_row() {
        for outside in ["0", "4", "99"] {
            let objection = settle(outside, 3, "Clubria").expect_err("out of range");
            assert!(objection.contains("pick 1 to 3"), "{objection}");
        }
        // A number too large to hold is the same answer, not a panic.
        let huge = "9".repeat(40);
        assert!(settle(&huge, 3, "Clubria").is_err());
    }

    #[test]
    fn a_bare_name_is_a_repository_in_our_org() {
        assert_eq!(
            settle("payments", 3, "Clubria"),
            Ok(Answer::Named(repo("Clubria/payments")))
        );
        assert_eq!(
            settle("someone-else/tool", 3, "Clubria"),
            Ok(Answer::Named(repo("someone-else/tool")))
        );
    }

    #[test]
    fn an_unusable_name_is_refused_in_the_words_that_explain_why() {
        let objection = settle("Clubria/..", 3, "Clubria").expect_err("refused");
        assert!(objection.contains("not a repository name"), "{objection}");
        let objection = settle("-x", 3, "Clubria").expect_err("refused");
        assert!(objection.contains("dash"), "{objection}");
    }

    #[test]
    fn the_default_leads_then_the_trees_this_machine_has() {
        let listing = [
            entry("Clubria/design-system", NOW),
            entry("Clubria/payments", NOW - 100),
            entry("Clubria/ai-builders-hub", NOW - 200),
        ];
        let (rows, hidden) = rows_for(
            &listing,
            &known(&[("Clubria/payments", "/code/payments")]),
            &repo("Clubria/ai-builders-hub"),
            &repo("Clubria/ai-builders-hub"),
        );

        assert_eq!(hidden, 0);
        let order: Vec<&str> = rows.iter().map(|row| row.repo.name()).collect();
        assert_eq!(
            order,
            ["ai-builders-hub", "payments", "design-system"],
            "what Enter takes first, then the checkout that already exists"
        );
        assert!(rows[0].default && !rows[0].cloned);
        assert!(rows[1].cloned, "a known checkout is marked");
    }

    #[test]
    fn a_checkout_github_did_not_mention_is_still_offered() {
        // Past the single page, or another owner's repository. Without this a
        // developer could not get back to a tree they are working in.
        let (rows, _) = rows_for(
            &[entry("Clubria/ai-builders-hub", NOW)],
            &known(&[("someone-else/tool", "/code/tool")]),
            &repo("Clubria/ai-builders-hub"),
            &repo("Clubria/ai-builders-hub"),
        );
        let names: Vec<&str> = rows.iter().map(|row| row.repo.name()).collect();
        assert!(names.contains(&"tool"), "{names:?}");
    }

    #[test]
    fn what_enter_takes_is_always_in_the_box() {
        // The active repository can be absent from the listing entirely — a
        // failed listing draws no entries at all — and a question offering
        // something the box does not show is unreadable.
        let (rows, hidden) = rows_for(
            &[],
            &BTreeMap::new(),
            &repo("Clubria/payments"),
            &repo("Clubria/ai-builders-hub"),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo.slug(), "Clubria/payments");
        assert_eq!(hidden, 0);
    }

    #[test]
    fn a_long_org_is_cut_to_ten_rows_and_says_how_many_it_left() {
        let listing: Vec<Entry> = (0..25)
            .map(|index| entry(&format!("Clubria/repo-{index:02}"), NOW - index))
            .collect();
        let (rows, hidden) = rows_for(
            &listing,
            &BTreeMap::new(),
            &repo("Clubria/repo-24"),
            &repo("Clubria/repo-24"),
        );
        assert_eq!(rows.len(), SHOWN);
        assert_eq!(hidden, 15);
        assert_eq!(
            rows[0].repo.name(),
            "repo-24",
            "the row Enter takes must survive the cut"
        );
    }

    /// A `Ctx` that has connected: `org` set, and `gh` installed so the listing
    /// is reachable.
    async fn asked(
        answers: &[&str],
        runner: FakeRunner,
    ) -> (Ctx, tempfile::TempDir, std::sync::Arc<FakeRunner>) {
        let (mut ctx, home, fake) = ctx_and_runner(runner).await;
        install_owned_tools(&ctx).await;
        ctx.org = Some(org_config());
        ctx.ui = Ui::scripted(answers.iter().copied());
        (ctx, home, fake)
    }

    fn listing_runner() -> FakeRunner {
        FakeRunner::new().containing(
            "api orgs/Clubria/repos",
            0,
            "Clubria/ai-builders-hub\t1755000000\nClubria/payments\t1754900000\n",
            "",
        )
    }

    #[tokio::test]
    async fn enter_takes_the_org_default_on_a_machine_that_has_never_chosen() {
        let (mut ctx, _home, _fake) = asked(&[""], listing_runner()).await;

        let chosen = choose(&mut ctx).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/ai-builders-hub");
        assert_eq!(
            ctx.config.active_repo.as_deref(),
            Some("Clubria/ai-builders-hub"),
            "the answer has to be recorded before the tasks read it"
        );
        assert_eq!(ctx.repo.as_ref().map(Repo::slug), Some("Clubria/ai-builders-hub"));
    }

    #[tokio::test]
    async fn a_number_picks_the_repository_on_that_row() {
        let (mut ctx, _home, _fake) = asked(&["2"], listing_runner()).await;
        let chosen = choose(&mut ctx).await.expect("chooses");
        assert_eq!(chosen.slug(), "Clubria/payments");
        assert_eq!(ctx.config.active_repo.as_deref(), Some("Clubria/payments"));
    }

    #[tokio::test]
    async fn a_typed_name_picks_a_repository_the_box_never_showed() {
        let (mut ctx, _home, _fake) = asked(&["internal-tooling"], listing_runner()).await;
        let chosen = choose(&mut ctx).await.expect("chooses");
        assert_eq!(chosen.slug(), "Clubria/internal-tooling");
    }

    #[tokio::test]
    async fn three_unusable_answers_and_riabuild_takes_the_default() {
        let (mut ctx, _home, _fake) = asked(&["nope/../x", "-x", "99", "2"], listing_runner()).await;

        let chosen = choose(&mut ctx).await.expect("chooses");

        assert_eq!(
            chosen.slug(),
            "Clubria/ai-builders-hub",
            "the fourth answer is never read: the bound is three"
        );
        assert_eq!(ctx.ui.asked().len(), 3, "asked three times, then stopped");
    }

    #[tokio::test]
    async fn a_run_with_nobody_there_takes_the_default_without_asking_github() {
        // The e2e suites and every CI job. Nothing is drawn, nothing is fetched,
        // and the answer is what riabuild would have done alone.
        let (mut ctx, _home, fake) = ctx_and_runner(FakeRunner::new()).await;
        install_owned_tools(&ctx).await;
        ctx.org = Some(org_config());

        let chosen = choose(&mut ctx).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/ai-builders-hub");
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| !call.contains("api orgs/")),
            "an unattended run must not spend a round trip on a box nobody sees"
        );
    }

    #[tokio::test]
    async fn the_checkout_an_older_riabuild_recorded_is_adopted_by_the_default() {
        let (mut ctx, _home, _fake) = asked(&[""], listing_runner()).await;
        ctx.update_config(|config| config.project_path = Some("/code/hub".into()))
            .await
            .expect("write");

        choose(&mut ctx).await.expect("chooses");

        assert_eq!(
            ctx.config.repos.get("Clubria/ai-builders-hub").map(String::as_str),
            Some("/code/hub"),
            "the tree the developer already has must not be re-cloned"
        );
        assert_eq!(ctx.config.project_path, None);
    }

    #[tokio::test]
    async fn picking_a_second_repository_keeps_the_first_checkout_recorded() {
        let (mut ctx, _home, _fake) = asked(&["payments"], listing_runner()).await;
        ctx.update_config(|config| config.project_path = Some("/code/hub".into()))
            .await
            .expect("write");

        choose(&mut ctx).await.expect("chooses");

        assert_eq!(ctx.config.active_repo.as_deref(), Some("Clubria/payments"));
        assert_eq!(
            ctx.config.repos.get("Clubria/ai-builders-hub").map(String::as_str),
            Some("/code/hub"),
            "switching away must not forget where the other tree is"
        );
    }

    #[tokio::test]
    async fn a_dry_run_records_nothing() {
        let (mut ctx, _home, _fake) = asked(&["payments"], listing_runner()).await;
        ctx.dry_run = true;

        let chosen = choose(&mut ctx).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/payments");
        assert_eq!(
            ctx.config.active_repo, None,
            "a dry run must leave the machine as it found it"
        );
    }

    #[tokio::test]
    async fn a_listing_that_failed_is_named_and_the_run_goes_on() {
        let runner = FakeRunner::new().containing(
            "api orgs/Clubria/repos",
            1,
            "",
            "gh: You are not logged into any GitHub hosts",
        );
        let (mut ctx, _home, _fake) = asked(&[""], runner).await;

        let chosen = choose(&mut ctx).await.expect("a failed listing is not fatal");

        assert_eq!(chosen.slug(), "Clubria/ai-builders-hub");
    }

    #[tokio::test]
    async fn a_dashboard_default_nobody_could_clone_stops_the_run() {
        let (mut ctx, _home, _fake) = asked(&[""], listing_runner()).await;
        let mut org = org_config();
        org.repo_slug = "not a repository".into();
        ctx.org = Some(org);

        let error = choose(&mut ctx).await.expect_err("cannot proceed");
        assert!(
            format!("{error:#}").contains("riabuild dashboard"),
            "the developer has to be sent to the lead who typed it: {error:#}"
        );
    }
}

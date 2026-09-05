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
//!
//! The pure half is `answer`; what a command that acts on an existing
//! checkout asks instead is `cloned`. What is here is the question a
//! provisioning run puts, and the two writes that record what it settled on.

mod answer;
mod cloned;

pub use answer::{Answer, rows_for, settle};

use answer::ATTEMPTS;
pub use cloned::choose_cloned;

use super::list::{self, Access, Listing};
use super::render::{self, Row};
use crate::Ctx;
use anyhow::Result;
use riabuild_api::Repo;
use riabuild_paths::config::UserConfig;
use riabuild_ui::Ui;
use std::collections::BTreeMap;

/// Whether this run may take the repository a developer pinned without putting
/// the question at all.
///
/// A named type rather than a `bool` because both call sites read as an
/// English sentence and neither reads as `true`: the ordinary run honours a
/// pin, and `riabuild --repo` with nothing after it is a developer asking for
/// the box back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// The ordinary run. A pinned repository is taken in silence.
    IfNotPinned,
    /// Put the question even where there is a pin, and let this run's answer
    /// replace it.
    Always,
}

/// What this run's answer does to `config.always_repo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pin {
    /// The pin, if there is one, is none of this run's business.
    Leave,
    Set,
    Clear,
}

/// Which repository this run is about, asked if there is anybody to ask and if
/// the developer has not already said they never want to be.
///
/// Writes the answer before returning, so every repository-scoped task in the
/// run that follows reads it from one place.
pub async fn choose(ctx: &mut Ctx, ask: Ask) -> Result<Repo> {
    // What Enter takes: the repository this machine last worked on, and the org
    // default on a machine that has never chosen. Fallible only for a dashboard
    // slug nobody could clone, which is the one case worth stopping a
    // provisioning run for — see `OrgConfig::default_repo`.
    let mut default = ctx.repo()?;
    let org_default = ctx.org()?.default_repo()?;
    let pinned = pinned(&ctx.config);

    // Checked before the listing is fetched rather than after, so an unattended
    // run does not spend a GitHub round trip on a box nobody will see. Taking
    // the default here is the crate rule for `ask`, and the right rule for this
    // question: picking a repository is the decision riabuild would otherwise
    // have made alone. `remote::pick` refuses instead, because connecting
    // provisions a server — this does not.
    //
    // A pin is taken here without checking that GitHub still has it, and
    // deliberately: the check exists to decide whether to *ask*, and there is
    // nobody to ask. An unattended run on a repository that has gone fails at
    // the clone with GitHub's own words, which is a better answer than one this
    // could invent.
    if !ctx.ui.interactive() {
        return adopt(ctx, pinned.unwrap_or(default), &org_default, Pin::Leave).await;
    }

    // A pin nobody can parse is cleared by whatever this run settles on: it
    // names no repository, so it cannot be honoured, and leaving it would mean
    // asking this same question again on every run for ever.
    let mut pin = match ctx.config.always_repo.is_some() && pinned.is_none() {
        true => Pin::Clear,
        false => Pin::Leave,
    };

    if let (Ask::IfNotPinned, Some(pinned)) = (ask, pinned) {
        match list::access(ctx, &pinned).await {
            // Nothing is said here. The run's next line already names the
            // repository it is working on — `provision::describe_repo` — and it
            // is that line, on every run, that carries the way back to the box.
            // Two lines about one repository is how a developer stops reading
            // either of them.
            Access::Yes => return adopt(ctx, pinned, &org_default, Pin::Leave).await,
            // "We could not tell" is not "you have lost access". A token that
            // expired overnight must not move a developer off the repository
            // they are in the middle of, so the pin stands and the reason is
            // named rather than swallowed.
            Access::Unknown(detail) => {
                ctx.ui.note(&format!(
                    "GitHub could not confirm {pinned} just now ({detail}), so riabuild is \
                     carrying on with it"
                ));
                return adopt(ctx, pinned, &org_default, Pin::Leave).await;
            }
            Access::Gone => {
                ctx.ui.warn(&format!(
                    "GitHub does not have {pinned} for you any more, so riabuild is asking \
                     which repository to work on."
                ));
                pin = Pin::Clear;
                // And it cannot be what Enter takes either: `active_repo` is
                // almost always the same slug, and offering a repository
                // nobody can clone is offering to fail.
                if default == pinned {
                    default = org_default.clone();
                }
            }
        }
    }

    let chosen = offer(
        ctx,
        Offer {
            default: &default,
            org_default: &org_default,
            known: &ctx.config.repos,
            on: None,
        },
    )
    .await;

    if let Some(answer) = ctx.ui.confirm(&format!("Always use {chosen}?")) {
        pin = match answer {
            true => Pin::Set,
            // An explicit no clears a pin as well as declining one, which is
            // what makes `riabuild --repo` a way back to being asked every run
            // rather than only a way to be asked once.
            false => Pin::Clear,
        };
    }
    // Nothing is said about a yes either, for the same reason: the line
    // `describe_repo` prints a moment later says `always — riabuild --repo asks
    // again`, on this run and on every run after it, which is where a developer
    // will be looking for it on the morning they want the box back.
    adopt(ctx, chosen, &org_default, pin).await
}

/// The repository this machine said "always" to, if it named one riabuild could
/// use.
///
/// `Repo::parse` rather than the raw string, for the reason the crate rule
/// gives: this value reaches `gh repo clone` argv and a directory name, and it
/// has been sitting in a file a person can edit since the run that wrote it.
fn pinned(config: &UserConfig) -> Option<Repo> {
    Repo::parse(config.always_repo.as_deref()?).ok()
}

/// Who the question is being put for, and what it may say about their machine.
///
/// Named fields rather than four positional arguments, for the reason
/// `remote::Request` gives: two of these are a `&Repo` and the other two are
/// about *whose* machine is being asked about, so an argument list of that
/// shape is one transposition away from asking the wrong question about the
/// wrong box.
pub struct Offer<'a> {
    /// What Enter takes.
    pub default: &'a Repo,
    /// The org default, which is the row the box marks as such.
    pub org_default: &'a Repo,
    /// The checkouts to mark as already present, and to offer even where the
    /// listing does not mention them. This machine's own for a local run, and
    /// **empty** where the answer is for a server: a laptop cannot see what a
    /// server has cloned, and a row claiming otherwise would be a guess printed
    /// as a fact.
    pub known: &'a BTreeMap<String, String>,
    /// The server this is being asked on behalf of, named in the question when
    /// it is not this machine — so a developer connecting to `build-01` is not
    /// asked an unqualified "which repository?" that reads as a question about
    /// the laptop it is typed on.
    pub on: Option<&'a str>,
}

/// The box, the question, and nothing written down.
///
/// Split out of [`choose`] because `riabuild remote` puts the same question on
/// the laptop *for a server*, where every write `adopt` makes would be about the
/// wrong machine: that answer travels on as `--repo` and is recorded beside the
/// server in `remotes.json`, and this laptop's own `config.json` has nothing to
/// do with it.
///
/// The caller decides whether there is anybody to ask. Both of them check
/// before the listing is fetched rather than after, so an unattended run does
/// not spend a GitHub round trip on a box nobody will see.
pub async fn offer(ctx: &Ctx, offer: Offer<'_>) -> Repo {
    let Offer {
        default,
        org_default,
        known,
        on,
    } = offer;

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

    let (rows, hidden) = rows_for(entries, known, default, org_default);
    if matches!(&listing, Listing::Repos(entries) if entries.is_empty()) {
        ctx.ui.info(&format!(
            "GitHub lists no repositories you can see in {}.",
            org_default.owner()
        ));
    }
    ctx.ui.info("");
    ctx.ui.info(&render::repos_box(
        &format!("{} repositories", org_default.owner()),
        &rows,
        hidden,
        now(),
        ctx.ui.theme(),
    ));

    ask(&ctx.ui, &rows, default, org_default.owner(), on)
}

/// The question, and the three attempts it is put in.
fn ask(ui: &Ui, rows: &[Row], default: &Repo, default_owner: &str, on: Option<&str>) -> Repo {
    // The default is named inside the question rather than only in the box
    // above it: `Ui::info` returns early under `--quiet` and `Ui::ask` does not,
    // so `riabuild --quiet` puts this question with the box silently dropped.
    // The same reason `remote::pick::settle` names the server in its prompt.
    //
    // `on` is that same reason one step further out: `riabuild remote` asks this
    // on the laptop about a server, and the two questions are otherwise
    // indistinguishable at the one terminal they are both typed into.
    let question = match on {
        Some(server) => format!("Which repository on {server}? (press enter for {default})"),
        None => format!("Which repository? (press enter for {default})"),
    };
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

/// Records the repository this run is about, what that does to the pin, and
/// migrates the checkout an older riabuild left behind.
///
/// The migration happens here because this is the first place both facts are
/// known: the path in `config.json`, and the repository it must be a checkout of
/// — the org default, because it is the only repository riabuild could have
/// cloned before it asked. Both go in one write, so a run that is interrupted
/// between them cannot leave a machine that has adopted nothing and forgotten
/// where its checkout was.
async fn adopt(ctx: &mut Ctx, chosen: Repo, org_default: &Repo, pin: Pin) -> Result<Repo> {
    let (slug, default_slug) = (chosen.slug().to_string(), org_default.slug().to_string());
    // Under `--check` nothing is written: a dry run must leave the machine as it
    // found it, and `config.json` is part of "as it found it".
    if !ctx.dry_run {
        ctx.update_config(|config: &mut UserConfig| {
            config.adopt_legacy_checkout(&default_slug);
            config.active_repo = Some(slug.clone());
            // In the same write as the choice it is about. A run interrupted
            // between the two would leave a machine pinned to one repository
            // and working on another, which is the one state nothing on it
            // could explain.
            match pin {
                Pin::Leave => {}
                Pin::Set => config.always_repo = Some(slug),
                Pin::Clear => config.always_repo = None,
            }
        })
        .await?;
    }
    ctx.repo = Some(chosen.clone());
    Ok(chosen)
}

/// Records a repository named on the command line by `--repo`.
///
/// Separate from [`adopt`] only in where the org default comes from: `--repo` is
/// honoured on a machine with no session, where there is no default to migrate a
/// pre-picker checkout under, so there the choice is recorded on its own.
///
/// **A named repository replaces a pin and never creates one.** On a machine
/// that has answered "always", the next bare `riabuild` puts no question — so a
/// `--repo` that left the pin alone would move this run onto one repository and
/// the next one silently back, which is the switch nobody could see happening.
/// On a machine that has not, `--repo payments` is one run about `payments` and
/// says nothing about the rest, which is what it has always meant and what
/// every script and CI job passing it relies on.
pub async fn adopt_named(ctx: &mut Ctx, repo: Repo) -> Result<()> {
    let pin = match ctx.config.always_repo.is_some() {
        true => Pin::Set,
        false => Pin::Leave,
    };
    match ctx.org.as_ref().and_then(|org| org.default_repo().ok()) {
        Some(default) => {
            adopt(ctx, repo, &default, pin).await?;
        }
        None => {
            if !ctx.dry_run {
                let slug = repo.slug().to_string();
                ctx.update_config(|config: &mut UserConfig| {
                    config.active_repo = Some(slug.clone());
                    if pin == Pin::Set {
                        config.always_repo = Some(slug);
                    }
                })
                .await?;
            }
            ctx.repo = Some(repo);
        }
    }
    Ok(())
}

pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::list::Entry;
    use crate::repo::render::SHOWN;
    use crate::testing::{ctx_and_runner, install_owned_tools, org_config};
    use riabuild_runner::FakeRunner;
    use std::collections::BTreeMap;

    const NOW: u64 = 1_755_000_000;

    fn repo(slug: &str) -> Repo {
        Repo::parse(slug).expect("parses")
    }

    fn entry(slug: &str, pushed_at: u64) -> Entry {
        Entry {
            repo: repo(slug),
            pushed_at,
            description: String::new(),
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

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/ai-builders-hub");
        assert_eq!(
            ctx.config.active_repo.as_deref(),
            Some("Clubria/ai-builders-hub"),
            "the answer has to be recorded before the tasks read it"
        );
        assert_eq!(
            ctx.repo.as_ref().map(Repo::slug),
            Some("Clubria/ai-builders-hub")
        );
    }

    #[tokio::test]
    async fn a_number_picks_the_repository_on_that_row() {
        let (mut ctx, _home, _fake) = asked(&["2"], listing_runner()).await;
        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");
        assert_eq!(chosen.slug(), "Clubria/payments");
        assert_eq!(ctx.config.active_repo.as_deref(), Some("Clubria/payments"));
    }

    #[tokio::test]
    async fn a_typed_name_picks_a_repository_the_box_never_showed() {
        let (mut ctx, _home, _fake) = asked(&["internal-tooling"], listing_runner()).await;
        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");
        assert_eq!(chosen.slug(), "Clubria/internal-tooling");
    }

    #[tokio::test]
    async fn three_unusable_answers_and_riabuild_takes_the_default() {
        let (mut ctx, _home, _fake) =
            asked(&["nope/../x", "-x", "99", "2"], listing_runner()).await;

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(
            chosen.slug(),
            "Clubria/ai-builders-hub",
            "the fourth answer never picks a repository: the bound is three"
        );
        let asked_which = ctx
            .ui
            .asked()
            .into_iter()
            .filter(|question| question.starts_with("Which repository"))
            .count();
        assert_eq!(asked_which, 3, "asked three times, then stopped");
    }

    #[tokio::test]
    async fn a_run_with_nobody_there_takes_the_default_without_asking_github() {
        // The e2e suites and every CI job. Nothing is drawn, nothing is fetched,
        // and the answer is what riabuild would have done alone.
        let (mut ctx, _home, fake) = ctx_and_runner(FakeRunner::new()).await;
        install_owned_tools(&ctx).await;
        ctx.org = Some(org_config());

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

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

        choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(
            ctx.config
                .repos
                .get("Clubria/ai-builders-hub")
                .map(String::as_str),
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

        choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(ctx.config.active_repo.as_deref(), Some("Clubria/payments"));
        assert_eq!(
            ctx.config
                .repos
                .get("Clubria/ai-builders-hub")
                .map(String::as_str),
            Some("/code/hub"),
            "switching away must not forget where the other tree is"
        );
    }

    #[tokio::test]
    async fn a_dry_run_records_nothing() {
        let (mut ctx, _home, _fake) = asked(&["payments"], listing_runner()).await;
        ctx.dry_run = true;

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

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

        let chosen = choose(&mut ctx, Ask::IfNotPinned)
            .await
            .expect("a failed listing is not fatal");

        assert_eq!(chosen.slug(), "Clubria/ai-builders-hub");
    }

    #[tokio::test]
    async fn a_dashboard_default_nobody_could_clone_stops_the_run() {
        let (mut ctx, _home, _fake) = asked(&[""], listing_runner()).await;
        let mut org = org_config();
        org.repo_slug = "not a repository".into();
        ctx.org = Some(org);

        let error = choose(&mut ctx, Ask::IfNotPinned)
            .await
            .expect_err("cannot proceed");
        assert!(
            format!("{error:#}").contains("riabuild dashboard"),
            "the developer has to be sent to the lead who typed it: {error:#}"
        );
    }

    /// A listing runner that also answers for one repository by name, which is
    /// what `list::access` asks about a pinned one.
    fn runner_that_still_has(slug: &str) -> FakeRunner {
        listing_runner().containing(&format!("api repos/{slug}"), 0, slug, "")
    }

    /// The same, for a repository GitHub no longer has for this account.
    fn runner_that_lost(slug: &str) -> FakeRunner {
        listing_runner().containing(
            &format!("api repos/{slug}"),
            1,
            "",
            "gh: Not Found (HTTP 404)",
        )
    }

    async fn pinned_to(
        slug: &str,
        answers: &[&str],
        runner: FakeRunner,
    ) -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home, _fake) = asked(answers, runner).await;
        let slug = slug.to_string();
        ctx.update_config(|config| config.always_repo = Some(slug))
            .await
            .expect("write");
        (ctx, home)
    }

    #[tokio::test]
    async fn saying_yes_to_always_records_it_and_the_next_run_asks_nothing() {
        // The whole feature, in the order a developer meets it.
        let (mut ctx, _home, _fake) = asked(&["payments", "y"], listing_runner()).await;

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/payments");
        assert_eq!(
            ctx.config.always_repo.as_deref(),
            Some("Clubria/payments"),
            "the answer to `Always use …?` is what has to survive the run"
        );
        assert!(
            ctx.ui.asked().iter().any(|q| q.contains("Always use")),
            "{:?}",
            ctx.ui.asked()
        );
        // And nothing is said about it here. The way back is on the line
        // `provision::describe_repo` prints a moment later, on this run and on
        // every run after — which is `report::a_pinned_machine_is_told_how_to_
        // be_asked_again`, and is why two lines about one repository would be
        // one line too many.
        assert!(
            ctx.ui.noted().iter().all(|note| !note.contains("--repo")),
            "{:?}",
            ctx.ui.noted()
        );
    }

    #[tokio::test]
    async fn a_pinned_repository_is_taken_without_a_question_or_a_box() {
        let (mut ctx, _home) = pinned_to(
            "Clubria/payments",
            &[],
            runner_that_still_has("Clubria/payments"),
        )
        .await;

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/payments");
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
        assert_eq!(ctx.config.active_repo.as_deref(), Some("Clubria/payments"));
        assert_eq!(ctx.config.always_repo.as_deref(), Some("Clubria/payments"));
    }

    #[tokio::test]
    async fn a_pinned_repository_that_github_no_longer_has_asks_again() {
        // The one thing that undoes a pin on its own. Without it the machine
        // provisions a checkout nobody can clone, in the silence the pin was
        // chosen for.
        let (mut ctx, _home) = pinned_to(
            "Clubria/payments",
            &["", "n"],
            runner_that_lost("Clubria/payments"),
        )
        .await;

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert!(
            ctx.ui.warned().iter().any(|said| said.contains("payments")),
            "the developer has to be told why they are being asked: {:?}",
            ctx.ui.warned()
        );
        assert_eq!(
            chosen.slug(),
            "Clubria/ai-builders-hub",
            "Enter cannot still offer the repository that has just gone"
        );
        assert_eq!(ctx.config.always_repo, None, "and the pin goes with it");
    }

    #[tokio::test]
    async fn a_pin_github_could_not_confirm_is_left_exactly_where_it_was() {
        // A token that expired overnight, a 500, an org that turned SAML on
        // this morning. "We could not tell" is not "you have lost access", and
        // reading it that way moves a developer off the repository they are in
        // the middle of.
        let runner = listing_runner().containing(
            "api repos/Clubria/payments",
            1,
            "",
            "gh: Server Error (HTTP 500)",
        );
        let (mut ctx, _home) = pinned_to("Clubria/payments", &[], runner).await;

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/payments");
        assert_eq!(ctx.config.always_repo.as_deref(), Some("Clubria/payments"));
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }

    #[tokio::test]
    async fn asking_on_purpose_puts_the_box_back_and_no_answer_clears_the_pin() {
        // `riabuild --repo` with nothing after it.
        let (mut ctx, _home) = pinned_to(
            "Clubria/payments",
            &["1", "n"],
            runner_that_still_has("Clubria/payments"),
        )
        .await;

        let chosen = choose(&mut ctx, Ask::Always).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/ai-builders-hub");
        assert_eq!(
            ctx.config.always_repo, None,
            "declining is how a developer goes back to being asked every run"
        );
    }

    #[tokio::test]
    async fn a_pinned_machine_with_nobody_there_takes_the_pin_and_asks_github_nothing() {
        // Every CI job and every `ssh … riabuild --no-shell`. The access check
        // exists to decide whether to *ask*, and there is nobody to ask.
        let (mut ctx, _home) = pinned_to(
            "Clubria/payments",
            &[],
            runner_that_still_has("Clubria/payments"),
        )
        .await;
        ctx.ui = Ui::new(false);

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/payments");
    }

    #[tokio::test]
    async fn a_pin_nobody_could_parse_does_not_survive_the_run_that_finds_it() {
        // A hand-edited `config.json`, or one written by a riabuild that meant
        // something else by the field. It names no repository, so it cannot be
        // honoured — and leaving it would put this same question every run.
        let (mut ctx, _home) = pinned_to("not a repository", &["", "n"], listing_runner()).await;

        let chosen = choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(chosen.slug(), "Clubria/ai-builders-hub");
        assert_eq!(ctx.config.always_repo, None);
    }

    #[tokio::test]
    async fn a_dry_run_neither_pins_nor_unpins() {
        let (mut ctx, _home) = pinned_to(
            "Clubria/payments",
            &[],
            runner_that_lost("Clubria/payments"),
        )
        .await;
        ctx.dry_run = true;

        choose(&mut ctx, Ask::IfNotPinned).await.expect("chooses");

        assert_eq!(
            ctx.config.always_repo.as_deref(),
            Some("Clubria/payments"),
            "a run that promised to change nothing must not unpin a repository"
        );
    }

    #[tokio::test]
    async fn a_named_repository_replaces_a_pin_and_never_creates_one() {
        // `riabuild --repo payments` on a pinned machine is a switch, because
        // the next bare run would otherwise go silently back.
        let (mut ctx, _home) = pinned_to(
            "Clubria/ai-builders-hub",
            &[],
            runner_that_still_has("Clubria/ai-builders-hub"),
        )
        .await;

        adopt_named(&mut ctx, repo("Clubria/payments"))
            .await
            .expect("adopts");

        assert_eq!(ctx.config.always_repo.as_deref(), Some("Clubria/payments"));

        // …and on a machine that never pinned, it says nothing about the rest,
        // which is what every script passing `--repo` relies on.
        let (mut ctx, _home, _fake) = asked(&[], listing_runner()).await;
        adopt_named(&mut ctx, repo("Clubria/payments"))
            .await
            .expect("adopts");
        assert_eq!(ctx.config.always_repo, None);
    }
}

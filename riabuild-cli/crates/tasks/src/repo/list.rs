//! The repositories this developer may work on, as GitHub answers it.
//!
//! Asked through the `gh` riabuild owns, as the developer, which is the whole of
//! the authorization story: the token is theirs, so what comes back is what they
//! are allowed to see, and riabuild holds no permission logic that could be
//! wrong about it. riabuild-web is not involved and gains no endpoint — the
//! alternative would be one GitHub request per repository per member, because
//! there is no "repositories visible to user X" endpoint for an org token, and
//! its answer could still disagree with what the developer's own `gh` will
//! clone.

use crate::Ctx;
use riabuild_api::Repo;
use riabuild_runner::RunOptions;
use std::time::Duration;

/// One page, not `--paginate`. This list is read by a person at a prompt, and
/// the box shows ten rows of it; anything past them is reachable by typing its
/// name. Paginating an org of two hundred repositories to draw ten rows spends
/// a developer's first seconds on a list nobody will read.
const PAGE: usize = 30;

/// How long a repository list may take before the run goes on without it.
///
/// This is the first thing a provisioning run puts on screen, and the answer to
/// "which repository" is already known — Enter takes it. Waiting out a GitHub
/// slowdown to draw a box would make the picker the slowest part of a run whose
/// point is not asking developers anything.
const PATIENCE: Duration = Duration::from_secs(8);

/// A repository, and when it was last pushed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub repo: Repo,
    /// Seconds since the epoch, or 0 when GitHub did not say.
    pub pushed_at: u64,
}

/// What asking GitHub produced.
///
/// Three cases rather than a `Result<Vec<_>>`, because "we could not tell" must
/// never render as "you have no repositories" — the distinction
/// `github.ts::checkOrgMembership` draws with `unavailable`, for the same
/// reason: telling a developer they have no access when the truth is a missing
/// tool or a slow API sends them to the wrong person for help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Repos(Vec<Entry>),
    /// `gh` is not installed yet, which is every machine's first run: the task
    /// that installs it has not run, because the picker is put before the tasks.
    NotYet,
    /// We asked and could not tell.
    Unavailable(String),
}

/// The jq that turns GitHub's reply into one `slug<TAB>epoch` line per
/// repository.
///
/// `pushed_at` is null for a repository with no commits, and
/// `fromdateiso8601` on null fails the whole filter rather than that one row —
/// so the fallbacks are load-bearing, not defensive.
const JQ: &str = r#".[] | "\(.full_name)\t\((.pushed_at // .created_at // "1970-01-01T00:00:00Z") | fromdateiso8601)""#;

pub async fn fetch(ctx: &Ctx, owner: &str) -> Listing {
    let gh = ctx.gh();
    if !tokio::fs::try_exists(&gh).await.unwrap_or(false) {
        return Listing::NotYet;
    }

    let endpoint = format!("orgs/{owner}/repos?type=all&sort=pushed&per_page={PAGE}");
    let args = ["api", endpoint.as_str(), "--jq", JQ];
    // The bound is the runner's, not a `tokio::time::timeout` around the call.
    // A wrapper here stops riabuild *waiting* and leaves the `gh` running — one
    // more process holding the config directory the tasks after this are about
    // to rewrite — where the runner's own bound drops the child with the
    // expired future and the kernel reaps it (`RealRunner::start`'s
    // `kill_on_drop`).
    let options = RunOptions {
        timeout: Some(PATIENCE),
        ..Default::default()
    };

    let output = match ctx.runner.run(&gh, &args, &options).await {
        Err(error) => return Listing::Unavailable(unanswered(&error)),
        Ok(output) => output,
    };

    if !output.ok() {
        // `gh`'s own message, which distinguishes "not logged in" from "no such
        // org" from a 5xx — all three are things the developer can act on, and
        // none of them is riabuild's to paraphrase.
        let detail = output.stderr.trim();
        let detail = detail.lines().next().unwrap_or("gh api failed").to_string();
        return Listing::Unavailable(detail);
    }

    Listing::Repos(parse(&output.stdout))
}

/// What to tell the developer about a `gh` that produced no answer at all.
///
/// With the bound at the runner an expired call comes back as an ordinary
/// `Err` — the runner's own "`gh` did not finish within 8 seconds" — rather
/// than as an `Elapsed` this could match on the type of. The sentence has to
/// survive that shape change: what a slow API looks like from here is *GitHub
/// did not answer*, and that is what the box shows in place of the list. A
/// path naming riabuild's own copy of `gh` and a number the developer never
/// chose is a worse answer to the same question.
///
/// Everything else — `gh` missing between the `try_exists` above and the spawn,
/// a broken pipe — keeps the error's own words, for the reason the `!ok()`
/// branch keeps `gh`'s: none of it is riabuild's to paraphrase.
fn unanswered(error: &anyhow::Error) -> String {
    let said = format!("{error:#}");
    if said.contains("did not finish within") {
        return format!(
            "GitHub did not answer within {} seconds",
            PATIENCE.as_secs()
        );
    }
    said
}

/// One `slug<TAB>epoch` line per repository.
///
/// A row riabuild would refuse to clone is dropped rather than shown: GitHub
/// allows names `Repo::parse` does not, and offering one at a prompt that then
/// objects to it wastes the developer's attempt on riabuild's own rules.
pub fn parse(stdout: &str) -> Vec<Entry> {
    stdout
        .lines()
        .filter_map(|line| {
            // Split before trimming: a row GitHub gave no timestamp for is
            // `slug\t`, and trimming the line first eats the tab that makes it
            // one row rather than none.
            let (slug, pushed) = line.split_once('\t')?;
            Some(Entry {
                repo: Repo::parse(slug).ok()?,
                pushed_at: pushed.trim().parse().unwrap_or(0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Bounds, ctx_and_runner, ctx_with, install_owned_tools};
    use riabuild_runner::{CommandRunner, FakeRunner};

    const TWO_ROWS: &str = "Clubria/ai-builders-hub\t1755000000\nClubria/payments\t1754900000\n";

    #[test]
    fn a_reply_becomes_one_entry_a_line() {
        let entries = parse(TWO_ROWS);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].repo.slug(), "Clubria/ai-builders-hub");
        assert_eq!(entries[0].pushed_at, 1755000000);
        assert_eq!(entries[1].repo.slug(), "Clubria/payments");
    }

    #[test]
    fn a_row_riabuild_would_refuse_to_clone_is_not_offered() {
        // GitHub will not name a repository `..`, but the box exists to be typed
        // at, and a row that objects when picked is worse than no row.
        let entries = parse("Clubria/..\t1\nClubria/payments\t2\n-x/y\t3\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].repo.slug(), "Clubria/payments");
    }

    #[test]
    fn a_row_without_a_timestamp_still_counts() {
        let entries = parse("Clubria/payments\t\nClubria/hub\tnot-a-number\n");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.pushed_at == 0));
    }

    #[tokio::test]
    async fn a_machine_without_gh_yet_is_not_a_failure() {
        // The first run on every machine: the picker is put before the tasks, so
        // the task that installs `gh` has not run.
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(fetch(&ctx, "Clubria").await, Listing::NotYet);
    }

    #[tokio::test]
    async fn the_org_is_asked_for_by_name_most_recently_pushed_first() {
        let runner = FakeRunner::new().containing("api orgs/Clubria/repos", 0, TWO_ROWS, "");
        let (ctx, _home, fake) = ctx_and_runner(runner).await;
        install_owned_tools(&ctx).await;

        let Listing::Repos(entries) = fetch(&ctx, "Clubria").await else {
            panic!("a stubbed listing must come back as repositories");
        };
        assert_eq!(entries.len(), 2);

        let calls = fake.calls.lock().unwrap();
        let call = calls
            .iter()
            .find(|call| call.contains("api orgs/"))
            .expect("the listing call");
        assert!(call.contains("type=all"), "private repos too: {call}");
        assert!(call.contains("sort=pushed"), "{call}");
        assert!(
            !call.contains("--paginate"),
            "one page is deliberate: {call}"
        );
    }

    #[tokio::test]
    async fn a_gh_that_cannot_answer_says_so_rather_than_reporting_none() {
        let runner = FakeRunner::new().containing(
            "api orgs/Clubria/repos",
            1,
            "",
            "gh: You are not logged into any GitHub hosts\nRun gh auth login",
        );
        let (ctx, _home, _fake) = ctx_and_runner(runner).await;
        install_owned_tools(&ctx).await;

        match fetch(&ctx, "Clubria").await {
            Listing::Unavailable(detail) => assert!(
                detail.contains("not logged into"),
                "gh's own words should survive: {detail}"
            ),
            other => panic!("a failing listing must not read as an empty one: {other:?}"),
        }
    }

    /// The picker's own patience reaches the runner, rather than being a
    /// wrapper around it that leaves the `gh` running.
    #[tokio::test]
    async fn the_listing_holds_github_to_the_pickers_patience() {
        let runner = FakeRunner::new().containing("api orgs/Clubria/repos", 0, TWO_ROWS, "");
        let (mut ctx, _home, _fake) = ctx_and_runner(runner).await;
        install_owned_tools(&ctx).await;
        let bounds = Bounds::default();
        ctx.runner = bounds.watching(ctx.runner.clone());

        fetch(&ctx, "Clubria").await;

        assert_eq!(bounds.of("api orgs/"), Some(Duration::from_secs(8)));
        assert_ne!(
            bounds.of("api orgs/"),
            RunOptions::default().timeout,
            "the first thing a run puts on screen does not wait out the default ceiling"
        );
    }

    /// The sentence the developer reads when GitHub says nothing, pinned
    /// against the error the runner actually produces.
    ///
    /// `unanswered` recognises an expired call by the runner's wording, which
    /// is a coupling between two crates that nothing else would fail on: with
    /// the bound moved, an `Elapsed` no longer arrives as a type this can
    /// match. So the real error is made here, by a real child that outlasts a
    /// real bound, rather than written out by hand — a message reworded in
    /// `riabuild-runner` has to fail *somewhere*, and this is the somewhere.
    #[tokio::test]
    async fn a_github_that_never_answers_is_reported_as_github_not_answering() {
        let error = riabuild_runner::RealRunner
            .run(
                "sleep",
                &["30"],
                &RunOptions {
                    timeout: Some(Duration::from_millis(50)),
                    ..Default::default()
                },
            )
            .await
            .expect_err("a child that outlasts its bound is a failure");

        assert_eq!(unanswered(&error), "GitHub did not answer within 8 seconds");
    }

    #[tokio::test]
    async fn a_failure_that_is_not_a_timeout_keeps_its_own_words() {
        let error = anyhow::anyhow!("could not start `gh`");
        assert_eq!(unanswered(&error), "could not start `gh`");
    }

    #[tokio::test]
    async fn an_org_with_nothing_visible_is_an_empty_list_not_a_failure() {
        let runner = FakeRunner::new().containing("api orgs/Clubria/repos", 0, "", "");
        let (ctx, _home, _fake) = ctx_and_runner(runner).await;
        install_owned_tools(&ctx).await;
        assert_eq!(fetch(&ctx, "Clubria").await, Listing::Repos(vec![]));
    }
}

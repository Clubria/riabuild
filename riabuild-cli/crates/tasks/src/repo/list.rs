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

/// How much of a description the box will show.
///
/// Wide enough for the sentence most repositories actually carry, and narrow
/// enough that the row it sits under stays one row on an 80-column terminal
/// once the two-space indent is spent. Applied here rather than in `render` so
/// that a description is cut once, on the way in: what riabuild holds is what
/// riabuild would print.
pub const DESCRIPTION: usize = 72;

/// A repository, what it says it is, and when it was last pushed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub repo: Repo,
    /// Seconds since the epoch, or 0 when GitHub did not say.
    pub pushed_at: u64,
    /// GitHub's own one-line description, already put through
    /// [`riabuild_ui::one_line`] — empty for a repository that has none.
    ///
    /// Sanitised at the boundary rather than at the point of printing, because
    /// there is more than one point of printing and only one boundary: this is
    /// a sentence any member of the org can set, and it reaches a terminal.
    pub description: String,
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

/// The jq that turns GitHub's reply into one `slug<TAB>epoch<TAB>description`
/// line per repository.
///
/// `pushed_at` is null for a repository with no commits, and
/// `fromdateiso8601` on null fails the whole filter rather than that one row —
/// so the fallbacks are load-bearing, not defensive.
///
/// The description is folded onto one line **here**, before it becomes output,
/// rather than left to [`riabuild_ui::one_line`] on the way back in. That
/// function cannot help with this one: a newline inside a description would
/// have already ended the *row*, and `parse` would read the rest of the
/// sentence as a repository slug of its own. Splitting on the two characters
/// this format is built out of is enough, and it is enough without a regular
/// expression — everything else a description can carry is `one_line`'s.
const JQ: &str = r#".[] | "\(.full_name)\t\((.pushed_at // .created_at // "1970-01-01T00:00:00Z") | fromdateiso8601)\t\((.description // "") | split("\n") | join(" ") | split("\t") | join(" "))""#;

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

/// One `slug<TAB>epoch<TAB>description` line per repository.
///
/// A row riabuild would refuse to clone is dropped rather than shown: GitHub
/// allows names `Repo::parse` does not, and offering one at a prompt that then
/// objects to it wastes the developer's attempt on riabuild's own rules.
///
/// The third field is optional, and stays optional: a `gh` that answered before
/// this field was asked for, or a jq that could not produce it, must still give
/// a usable list of repositories rather than none.
pub fn parse(stdout: &str) -> Vec<Entry> {
    stdout
        .lines()
        .filter_map(|line| {
            // Split before trimming: a row GitHub gave no timestamp for is
            // `slug\t`, and trimming the line first eats the tab that makes it
            // one row rather than none.
            let (slug, rest) = line.split_once('\t')?;
            let (pushed, description) = match rest.split_once('\t') {
                Some(halves) => halves,
                None => (rest, ""),
            };
            Some(Entry {
                repo: Repo::parse(slug).ok()?,
                pushed_at: pushed.trim().parse().unwrap_or(0),
                description: riabuild_ui::one_line(description, DESCRIPTION),
            })
        })
        .collect()
}

/// Whether this developer can still see one particular repository.
///
/// Asked of exactly one repository, and only for the one a developer told
/// riabuild to *always* use: the picker's own listing cannot answer it, because
/// that is one page of the org sorted by push date and a repository can be
/// missing from it for reasons that have nothing to do with access.
///
/// The three answers are the three [`Listing`] draws, for the reason it gives:
/// "we could not tell" must never render as "you no longer have access". Only
/// GitHub saying **404** unpins a repository, and everything else — a `gh` that
/// is not installed yet, an expired token, a 500, a 403 from an org that has
/// just turned on SAML — leaves the pin exactly where it was. The cost of
/// guessing wrong in that direction is one run that asks a question; in the
/// other it is a developer silently moved off the repository they work in.
///
/// Reading a 404 as "gone" rests on one fact about the token this asks with.
/// GitHub answers 404 rather than 403 for a private repository a token cannot
/// see, so a narrowed token would report a repository that is perfectly
/// present — but `gh auth login` grants `repo` by default, which is what
/// `github_cli` already relies on and why it only ever has to *add* `read:org`.
/// A token without it is also one whose picker listing is missing every private
/// repository in the org, so the pin is not where that developer finds out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// GitHub answered for it.
    Yes,
    /// GitHub said there is no such repository for this account.
    Gone,
    /// We asked and could not tell, in these words.
    Unknown(String),
}

pub async fn access(ctx: &Ctx, repo: &Repo) -> Access {
    let gh = ctx.gh();
    if !tokio::fs::try_exists(&gh).await.unwrap_or(false) {
        return Access::Unknown("GitHub sign-in is not installed here yet".to_string());
    }

    let endpoint = format!("repos/{}", repo.slug());
    let args = ["api", endpoint.as_str(), "--jq", ".full_name"];
    let options = RunOptions {
        timeout: Some(PATIENCE),
        ..Default::default()
    };

    match ctx.runner.run(&gh, &args, &options).await {
        Err(error) => Access::Unknown(unanswered(&error)),
        Ok(output) if output.ok() => Access::Yes,
        Ok(output) => {
            let detail = output.stderr.trim();
            let detail = detail.lines().next().unwrap_or("gh api failed");
            // `gh`'s own words for it, which is `gh: Not Found (HTTP 404)`.
            // Matched on the status rather than on the sentence, because the
            // sentence is localised and the number is not.
            match detail.contains("404") {
                true => Access::Gone,
                false => Access::Unknown(detail.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Bounds, ctx_and_runner, ctx_with, install_owned_tools};
    use riabuild_runner::{CommandRunner, FakeRunner};

    const TWO_ROWS: &str = "Clubria/ai-builders-hub\t1755000000\tWhere every builder starts\n\
                            Clubria/payments\t1754900000\tBilling and payment flows\n";

    #[test]
    fn a_reply_becomes_one_entry_a_line() {
        let entries = parse(TWO_ROWS);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].repo.slug(), "Clubria/ai-builders-hub");
        assert_eq!(entries[0].pushed_at, 1755000000);
        assert_eq!(entries[0].description, "Where every builder starts");
        assert_eq!(entries[1].repo.slug(), "Clubria/payments");
    }

    #[test]
    fn a_repository_that_describes_itself_as_nothing_says_nothing() {
        // GitHub serves `null` for a repository with no description, which the
        // jq turns into an empty field. An empty string is what the box reads
        // as "there is no second line for this row".
        let entries = parse("Clubria/payments\t1754900000\t\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].description, "");
    }

    #[test]
    fn a_gh_that_answered_before_descriptions_were_asked_for_still_lists() {
        // Two fields rather than three. The picker must keep working against
        // it: a listing is a great deal more use than a description.
        let entries = parse("Clubria/payments\t1754900000\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pushed_at, 1754900000);
        assert_eq!(entries[0].description, "");
    }

    #[test]
    fn a_description_that_would_redraw_the_box_is_defused_on_the_way_in() {
        // Any member of the org can set one of these, and it is printed
        // straight onto a developer's terminal.
        let entries = parse("Clubria/payments\t1\tpay\x1b[2Jments\n");
        assert_eq!(entries[0].description, "pay[2Jments");

        // And an essay is cut to the room the box has, once, here.
        let entries = parse(&format!("Clubria/payments\t1\t{}\n", "a".repeat(500)));
        assert_eq!(entries[0].description.chars().count(), DESCRIPTION);
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

    async fn asked_about(runner: FakeRunner) -> Access {
        let (ctx, _home, _fake) = ctx_and_runner(runner).await;
        install_owned_tools(&ctx).await;
        access(&ctx, &Repo::parse("Clubria/payments").expect("parses")).await
    }

    #[tokio::test]
    async fn a_repository_github_answers_for_is_still_this_developers() {
        let runner = FakeRunner::new().containing("api repos/Clubria/payments", 0, "", "");
        assert_eq!(asked_about(runner).await, Access::Yes);
    }

    #[tokio::test]
    async fn a_repository_github_has_never_heard_of_is_gone() {
        // The whole point of the check: a repository that was archived,
        // renamed, or that this developer has been taken off. Only this answer
        // unpins one.
        let runner = FakeRunner::new().containing(
            "api repos/Clubria/payments",
            1,
            "",
            "gh: Not Found (HTTP 404)",
        );
        assert_eq!(asked_about(runner).await, Access::Gone);
    }

    #[tokio::test]
    async fn every_other_failure_leaves_the_answer_unknown() {
        // A token that expired overnight, an org that turned SAML on this
        // morning, a GitHub having a bad day. None of them is "you no longer
        // work on this repository", and reading them that way moves a developer
        // off the repository they are in the middle of.
        for said in [
            "gh: You are not logged into any GitHub hosts",
            "gh: Resource protected by organization SAML enforcement (HTTP 403)",
            "gh: Server Error (HTTP 500)",
        ] {
            let runner = FakeRunner::new().containing("api repos/Clubria/payments", 1, "", said);
            match asked_about(runner).await {
                Access::Unknown(detail) => assert!(detail.contains("gh:"), "{detail}"),
                other => panic!("{said:?} must not read as gone: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_machine_without_gh_yet_cannot_tell_either() {
        // Every machine's first run. `Access::Gone` here would unpin a
        // repository on the one run that has no way of knowing anything.
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let repo = Repo::parse("Clubria/payments").expect("parses");
        assert!(matches!(access(&ctx, &repo).await, Access::Unknown(_)));
    }
}

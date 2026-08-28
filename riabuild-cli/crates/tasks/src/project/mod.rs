//! Task 5 — the repository, cloned where the developer expects it.
//!
//! Where it is cloned *to* is `directory`: the one question this task puts,
//! and the two refusals that keep the answer inside the tree this developer
//! may write in.

mod directory;

use directory::choose_dir;

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_paths::contract_tilde;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use std::path::Path;
use std::time::Duration;

pub struct Project;

/// How long the clone itself may take.
///
/// `RunOptions`' default is a *ceiling* on a call that has hung — ten minutes,
/// on the grounds that nothing riabuild captures the output of runs longer than
/// that. The clone is the exception the ceiling was not written for: its honest
/// duration is a repository whose size riabuild does not know divided by a link
/// riabuild does not choose, and a first clone of a large repository over a
/// hotel connection takes longer than ten minutes without anything being wrong.
/// Held to the default it would fail as "riabuild timed out" for a developer
/// whose only problem is bandwidth — and on the one step where failing means
/// there is no checkout at all.
///
/// An hour rather than `None`, because the hang this is a bound against is
/// still possible: `gh` inherits no terminal here and its stdin is closed, so a
/// credential prompt ends in an error rather than a wait, but a wedged TCP
/// connection to a proxy does not. An hour is past every clone anyone has
/// waited out and far short of "for the rest of the session".
const CLONE_PATIENCE: Duration = Duration::from_secs(3600);

/// The options the clone runs under.
///
/// Named rather than inlined so that `a_clone_is_given_its_own_patience` pins
/// the bound this file chose, and a future change to `DEFAULT_TIMEOUT` cannot
/// silently re-cap it.
fn clone_options() -> RunOptions {
    RunOptions {
        timeout: Some(CLONE_PATIENCE),
        ..Default::default()
    }
}

/// `git -C <dir> remote get-url origin`, or `None` if this is not a checkout.
async fn origin_url(ctx: &Ctx, dir: &Path) -> Option<String> {
    let output = ctx
        .runner
        .run(
            "git",
            &["-C", &dir.to_string_lossy(), "remote", "get-url", "origin"],
            &RunOptions::default(),
        )
        .await
        .ok()?;
    output.ok().then(|| output.trimmed().to_string())
}

#[async_trait]
impl Task for Project {
    fn id(&self) -> TaskId {
        "project"
    }

    fn title(&self) -> &str {
        "Project checkout"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &["github_cli"]
    }

    /// First setup for a repository shows the path riabuild would use and
    /// takes Enter for yes — `directory::ask`, one of the two decisions
    /// riabuild offers rather than imposes. A question that is recorded
    /// instead of printed is one the developer never gets to answer.
    fn interactive(&self) -> bool {
        true
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory chosen yet"));
        };
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Ok(Status::needs(format!(
                "{} does not exist",
                contract_tilde(&dir, &ctx.paths.home())
            )));
        }
        if !tokio::fs::try_exists(&dir.join(".git"))
            .await
            .unwrap_or(false)
        {
            return Ok(Status::needs(format!(
                "{} is not a git checkout",
                contract_tilde(&dir, &ctx.paths.home())
            )));
        }

        // Before sign-in there is nothing to compare against; `login` runs
        // first and this task re-runs once it has.
        if ctx.org.is_none() {
            return Ok(Status::needs("waiting for sign-in"));
        }
        let repo = ctx.repo()?;
        match origin_url(ctx, &dir).await {
            None => Ok(Status::needs("that checkout has no `origin` remote")),
            Some(remote) if repo.matches_remote(&remote) => Ok(Status::Satisfied),
            // Reached by picking a repository whose checkout is not where this
            // one is, as well as by a developer moving a directory aside. Naming
            // the repository that was *asked for* is what makes the first case
            // read as an answer rather than a fault.
            Some(remote) => Ok(Status::needs(format!(
                "that checkout points at {remote}, not {repo}"
            ))),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let repo = ctx.repo()?;
        let home = ctx.paths.home();

        let dir = match ctx.project_dir() {
            Some(path) => path,
            None => {
                // Where this lands is riabuild's decision, and it differs per
                // platform, and per machine when several developers share one
                // Unix account — see `Ctx::default_checkout`. That answer is
                // then offered rather than taken silently, and the developer may
                // say otherwise within the bounds `choose_dir` enforces.
                let default = ctx.default_checkout().await;
                choose_dir(ctx, &home, default).await
            }
        };

        if tokio::fs::try_exists(&dir.join(".git"))
            .await
            .unwrap_or(false)
        {
            // Already a checkout: verify rather than clone over it. Cloning into
            // an existing directory would fail, and deleting it could destroy
            // uncommitted work.
            if let Some(remote) = origin_url(ctx, &dir).await
                && !repo.matches_remote(&remote)
            {
                return Err(Failure::new(
                    format!("using {} for {repo}", dir.display()),
                    "Move that directory aside, or set another path with `riabuild --project <path>`, then run `riabuild` again.",
                )
                .detail(format!("it is a checkout of {remote}"))
                .into());
            }
        } else {
            if let Some(parent) = dir.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            // No space before the ellipsis — every other progress line in
            // riabuild is written "Downloading Node 22…", and this one sat
            // among them looking misaligned.
            ctx.ui.note(&format!("Cloning {repo}…"));
            // Through `gh` so the developer's existing GitHub auth is reused and
            // nobody has to set up SSH keys to get started.
            let output = ctx
                .runner
                .run(
                    &ctx.gh(),
                    &["repo", "clone", repo.slug(), &dir.to_string_lossy()],
                    &clone_options(),
                )
                .await?;
            if !output.ok() {
                return Err(Failure::new(
                    format!("cloning {repo}"),
                    "Check you can open the repository on github.com, then run `riabuild` again."
                        .to_string(),
                )
                .command(format!("gh repo clone {repo}"))
                .detail(output.stderr)
                .into());
            }
            mark_owned(ctx, &dir).await?;
        }

        let chosen = dir.to_string_lossy().into_owned();
        let slug = repo.slug().to_string();
        ctx.update_config(|config| {
            config.set_checkout(&slug, chosen);
            // Fills a blank, and never overrules a choice. The picker writes
            // this for every run that puts its question; the run it does not is
            // a machine's *first*, where there was no session yet to name a
            // default with — and leaving it unset there would record a checkout
            // of a repository nothing in the file says this machine works on.
            if config.active_repo.is_none() {
                config.active_repo = Some(slug);
            }
        })
        .await?;
        Ok(())
    }
}

/// The file `Ctx::default_checkout` reads to recognise a checkout as this
/// developer's own.
///
/// Written here and read by `Ctx::owned_by_this_namespace`, which reaches for
/// this constant rather than spelling the name again: a filename is a wire
/// format between the run that writes it and every run that reads it
/// afterwards, and two literals for one wire format is a drift waiting to
/// happen — one of them renamed, and every existing checkout on every server
/// stops being recognised as its developer's own.
pub(crate) const OWNER_MARKER: &str = ".riabuild-owner";

/// Records which riabuild namespace a checkout belongs to — **on a server, and
/// only there**.
///
/// `default_checkout` reads this marker in exactly one place: inside the branch
/// that groups a checkout under the developer's own GitHub login, which only a
/// server takes. Several developers share one Unix account there, so a
/// directory that already exists at `~/Clubria/<login>/<repo>` is either this
/// developer's own tree from last run or somebody else's, and the marker is the
/// difference between recognising it and claiming `<login>-2` beside it for
/// ever.
///
/// On a laptop that branch is unreachable, so the marker was written on every
/// clone and read by nothing — a file riabuild left in the developer's working
/// tree, untracked, showing as `??` in the first `git status` they ran, and on
/// a server naming a path with their member id in it. A marker that is only
/// ever written is not a weaker version of one that is read; it is litter with
/// an identifier in it, and the fix is to stop producing it.
///
/// Where it *is* read it has to stay inside the checkout: what
/// `default_checkout` is asking about is a **path on disk**, one that may
/// predate riabuild or belong to a colleague, and an answer recorded anywhere
/// in riabuild's own state would only ever describe checkouts riabuild already
/// knows about. So the second half is keeping it out of the developer's way
/// rather than out of the tree — `.git/info/exclude`, which is local to the
/// clone, invisible to `git status`, and never `.gitignore`, which belongs to
/// the repository being cloned and not to riabuild. That is the same mechanism,
/// and the same call, `env_local` already uses for the `.env.<environment>`
/// files it writes into a checkout.
///
/// Excluded before it is written, for `env_local`'s reason: a file riabuild
/// leaves in a checkout must never exist inside one that would commit it.
async fn mark_owned(ctx: &mut Ctx, dir: &Path) -> Result<()> {
    if ctx.server.is_none() {
        return Ok(());
    }
    // A no-op after a real clone; it is here so a faked `gh repo clone` in
    // tests has somewhere to write the marker.
    tokio::fs::create_dir_all(dir).await?;
    crate::env_local::ensure_ignored(ctx, dir, OWNER_MARKER).await?;
    tokio::fs::write(
        dir.join(OWNER_MARKER),
        ctx.paths.root().to_string_lossy().as_bytes(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Bounds, ctx_with, write_file};
    use riabuild_api::Member;
    use riabuild_runner::FakeRunner;
    use riabuild_ui::Ui;
    use std::sync::Arc;

    fn member_named(login: &str) -> Member {
        Member {
            github_login: login.into(),
            member_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@clubria.dev".into(),
            role: "developer".into(),
            status: "active".into(),
        }
    }

    #[tokio::test]
    async fn a_laptop_checkout_is_unchanged() {
        let (ctx, home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(
            ctx.default_checkout().await,
            riabuild_paths::default_project_dir(home.path(), "ai-builders-hub")
        );
    }

    #[tokio::test]
    async fn a_server_checkout_is_grouped_by_developer() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        ctx.server = Some("build-01".into());
        ctx.member = Some(member_named("ada"));

        assert_eq!(
            ctx.default_checkout().await,
            home.path()
                .join("Clubria")
                .join("ada")
                .join("ai-builders-hub")
        );
    }

    #[tokio::test]
    async fn a_taken_default_is_claimed_beside_rather_than_shared() {
        // Two developers, one Unix account, and a login that already has a tree —
        // a reused GitHub login, or somebody who set it up by hand. Sharing it
        // would put two people's branches and one .env.local in one checkout.
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        ctx.server = Some("build-01".into());
        ctx.member = Some(member_named("ada"));
        let taken = home.path().join("Clubria").join("ada");
        tokio::fs::create_dir_all(taken.join("ai-builders-hub"))
            .await
            .expect("mkdir");
        tokio::fs::write(
            taken.join("ai-builders-hub/.riabuild-owner"),
            "someone-else",
        )
        .await
        .expect("write");

        assert_eq!(
            ctx.default_checkout().await,
            home.path()
                .join("Clubria")
                .join("ada-2")
                .join("ai-builders-hub")
        );
    }

    #[tokio::test]
    async fn a_re_run_recognises_its_own_checkout_by_the_owner_marker() {
        // The claiming loop must not push a developer's own tree to `-2` on a
        // second run — apply() writes `.riabuild-owner`, and this is what makes
        // that marker mean something.
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        ctx.server = Some("build-01".into());
        ctx.member = Some(member_named("ada"));
        let own = home
            .path()
            .join("Clubria")
            .join("ada")
            .join("ai-builders-hub");
        tokio::fs::create_dir_all(&own).await.expect("mkdir");
        tokio::fs::write(
            own.join(".riabuild-owner"),
            ctx.paths.root().to_string_lossy().as_bytes(),
        )
        .await
        .expect("write");

        assert_eq!(ctx.default_checkout().await, own);
    }

    #[tokio::test]
    async fn an_unchosen_project_needs_setting_up() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(matches!(
            Project.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn an_unchosen_checkout_goes_to_the_platform_default() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        Project.apply(&mut ctx).await.unwrap();

        let expected = riabuild_paths::default_project_dir(home.path(), "ai-builders-hub");
        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(expected.to_string_lossy().as_ref()),
            "the checkout must land where this platform puts it"
        );
        // Named after the repository, not the whole owner/repo slug.
        assert!(expected.ends_with("ai-builders-hub"), "{expected:?}");
    }

    #[tokio::test]
    async fn a_server_run_clones_into_the_developer_grouped_path_and_marks_it() {
        // Proves the actual wiring, not just `default_checkout` in isolation:
        // `apply()` on a server must land the clone under the developer's own
        // directory, and leave the marker that keeps a re-run from claiming a
        // `-2` beside it.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        ctx.server = Some("build-01".into());
        ctx.member = Some(member_named("ada"));
        Project.apply(&mut ctx).await.unwrap();

        let expected = home
            .path()
            .join("Clubria")
            .join("ada")
            .join("ai-builders-hub");
        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(expected.to_string_lossy().as_ref()),
            "a server checkout must be grouped under the developer, not the platform default"
        );
        let marker = tokio::fs::read_to_string(expected.join(OWNER_MARKER))
            .await
            .expect("apply() must leave an owner marker");
        assert_eq!(marker, ctx.paths.root().to_string_lossy());

        // And it is kept out of the developer's `git status`, locally, without
        // touching the `.gitignore` the repository owns.
        let exclude = tokio::fs::read_to_string(expected.join(".git/info/exclude"))
            .await
            .expect("the marker must be excluded locally");
        assert!(
            exclude.lines().any(|line| line.trim() == OWNER_MARKER),
            "{exclude:?}"
        );
        assert!(
            !tokio::fs::try_exists(expected.join(".gitignore"))
                .await
                .unwrap_or(false),
            "the repository's own .gitignore is not riabuild's to write"
        );
    }

    /// A laptop gets no marker at all.
    ///
    /// `default_checkout` reads it only in the branch that groups a checkout
    /// under a GitHub login, and only a server takes that branch — so on a
    /// laptop this file was written on every clone, read by nothing, and left
    /// sitting in the developer's own working tree as an untracked `??`
    /// naming a path inside `~/.riabuild`.
    #[tokio::test]
    async fn a_laptop_checkout_is_left_with_nothing_riabuild_put_there() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        Project.apply(&mut ctx).await.unwrap();

        let checkout = riabuild_paths::default_project_dir(home.path(), "ai-builders-hub");
        assert!(
            !tokio::fs::try_exists(checkout.join(OWNER_MARKER))
                .await
                .unwrap_or(false),
            "{} should not exist",
            checkout.join(OWNER_MARKER).display()
        );
    }

    #[tokio::test]
    async fn a_server_offers_the_namespaced_path_as_the_default() {
        // What the prompt *offers* is what every developer who presses Enter
        // gets, so it has to be the namespaced answer and not the platform
        // default that all of them share.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        ctx.server = Some("build-01".into());
        ctx.member = Some(member_named("ada"));
        // Scripted with *no* answers: `Ui::scripted` is interactive, so the
        // prompt is really asked and really recorded in `asked()`, and an empty
        // queue is a developer pressing Enter — precisely the case where the
        // offered default becomes the answer. `ctx_with`'s own `Ui` models an
        // unattended machine, which would skip the prompt entirely and prove
        // nothing about what it offers.
        ctx.ui = Ui::scripted([]);

        Project.apply(&mut ctx).await.unwrap();

        let expected = home
            .path()
            .join("Clubria")
            .join("ada")
            .join("ai-builders-hub");
        let asked = ctx.ui.asked();
        let offered = contract_tilde(&expected, home.path());
        let shared = contract_tilde(
            &riabuild_paths::default_project_dir(home.path(), "ai-builders-hub"),
            home.path(),
        );
        assert!(
            asked.iter().any(|question| question.contains(&offered)),
            "the offered default must be {offered}: {asked:?}"
        );
        assert!(
            !asked.iter().any(|question| question.contains(&shared)),
            "the shared platform default {shared} must never be offered on a server: {asked:?}"
        );
        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn a_server_refuses_an_answer_outside_the_developers_namespace() {
        // `riabuild remote` connects with `ssh -t`, so this prompt has a real
        // terminal on a server and a developer can type anything. An absolute
        // path into a colleague's directory would give the two of them one
        // working tree, one set of branches, and one `.env.local` of brokered
        // secrets — so it is refused and the namespaced default stands.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        ctx.server = Some("build-01".into());
        ctx.member = Some(member_named("ada"));
        let someone_else = home.path().join("Clubria").join("bob").join("hub");
        ctx.ui = Ui::scripted([someone_else.to_string_lossy().as_ref()]);

        Project.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(
                home.path()
                    .join("Clubria")
                    .join("ada")
                    .join("ai-builders-hub")
                    .to_string_lossy()
                    .as_ref()
            ),
            "a refused answer must fall back to this developer's own path"
        );
    }

    #[tokio::test]
    async fn a_server_still_lets_a_developer_choose_inside_their_own_directory() {
        // The refusal must not collapse into "no choice at all": the point of
        // asking is that a developer can name their own checkout, and anywhere
        // under their own directory is theirs alone.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        ctx.server = Some("build-01".into());
        ctx.member = Some(member_named("ada"));
        let mine = home.path().join("Clubria").join("ada").join("hub");
        ctx.ui = Ui::scripted([mine.to_string_lossy().as_ref()]);

        Project.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(mine.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn a_typed_answer_is_used_instead_of_the_default() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        let chosen = home.path().join("work/hub");
        ctx.ui = Ui::scripted([chosen.to_string_lossy().as_ref()]);

        Project.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(chosen.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn a_relative_answer_is_refused() {
        // Storing a relative path would make the checkout's location depend on
        // where riabuild happened to be run from.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        ctx.ui = Ui::scripted(["code/hub"]);

        Project.apply(&mut ctx).await.unwrap();

        let default = riabuild_paths::default_project_dir(home.path(), "ai-builders-hub");
        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(default.to_string_lossy().as_ref()),
            "a refused answer must fall back to the default"
        );
    }

    #[tokio::test]
    async fn a_directory_that_already_has_files_in_it_is_refused() {
        // `gh repo clone` will not clone into it, and the developer should
        // learn that while they are being asked, not after the clone fails.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        let occupied = home.path().join("work/hub");
        write_file(&occupied.join("notes.txt"), "mine\n").await;
        ctx.ui = Ui::scripted([occupied.to_string_lossy().as_ref()]);

        Project.apply(&mut ctx).await.unwrap();

        let default = riabuild_paths::default_project_dir(home.path(), "ai-builders-hub");
        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(default.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn an_existing_checkout_of_the_right_repo_is_accepted() {
        // Adopting a checkout the developer already has is the whole point of
        // being asked, so "it has files in it" must not refuse this one.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with(
            "git -C",
            0,
            "git@github.com:Clubria/ai-builders-hub.git",
            "",
        ))
        .await;
        let existing = home.path().join("work/hub");
        write_file(&existing.join(".git/HEAD"), "ref: refs/heads/main\n").await;
        ctx.ui = Ui::scripted([existing.to_string_lossy().as_ref()]);

        Project.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(existing.to_string_lossy().as_ref())
        );
    }

    /// The clone is not held to the ceiling every other captured call gets.
    ///
    /// Asserted against the literal rather than against `CLONE_PATIENCE`, which
    /// would agree with itself, and against `RunOptions::default()` beside it:
    /// the failure this exists to catch is not somebody editing the constant,
    /// it is the explicit bound being dropped so that the clone quietly inherits
    /// whatever the default happens to be that month.
    #[tokio::test]
    async fn a_clone_is_given_its_own_patience() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        let bounds = Bounds::default();
        ctx.runner = bounds.watching(ctx.runner.clone());

        Project.apply(&mut ctx).await.expect("clones");

        assert_eq!(bounds.of("repo clone"), Some(Duration::from_secs(3600)));
        assert_ne!(
            bounds.of("repo clone"),
            RunOptions::default().timeout,
            "a clone takes as long as the repository and the link say it does"
        );
    }

    #[tokio::test]
    async fn a_first_run_that_had_no_session_still_records_what_it_cloned() {
        // The picker is skipped on a machine with no session — there is no team
        // configuration to name a default with — so this is the one path where
        // nothing else has said which repository the checkout belongs to.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;

        Project.apply(&mut ctx).await.expect("clones");

        assert_eq!(
            ctx.config.active_repo.as_deref(),
            Some("Clubria/ai-builders-hub")
        );
        assert_eq!(
            ctx.project_dir(),
            Some(riabuild_paths::default_project_dir(
                home.path(),
                "ai-builders-hub"
            ))
        );
    }

    #[tokio::test]
    async fn a_repository_the_picker_chose_is_not_overruled_by_the_clone() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        ctx.update_config(|config| config.active_repo = Some("Clubria/payments".into()))
            .await
            .expect("record the choice");
        ctx.repo = Some(riabuild_api::Repo::parse("Clubria/payments").expect("parses"));

        Project.apply(&mut ctx).await.expect("clones");

        assert_eq!(ctx.config.active_repo.as_deref(), Some("Clubria/payments"));
        assert!(
            ctx.config.checkout_of("Clubria/payments").is_some(),
            "and the checkout is recorded under the repository that was picked"
        );
    }

    #[tokio::test]
    async fn an_explicit_project_path_still_wins() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        let chosen = home.path().join("elsewhere/hub");
        ctx.config.project_path = Some(chosen.to_string_lossy().into());
        Project.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.project_dir()
                .as_deref()
                .map(Path::to_string_lossy)
                .as_deref(),
            Some(chosen.to_string_lossy().as_ref()),
            "`riabuild --project` must not be overridden by the default"
        );
    }

    #[tokio::test]
    async fn a_missing_directory_is_detected() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        ctx.config.project_path = Some(home.path().join("code/hub").to_string_lossy().into());
        let status = Project.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("does not exist"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_checkout_is_detected() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let dir = home.path().join("code/hub");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        ctx.config.project_path = Some(dir.to_string_lossy().into());
        let status = Project.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not a git checkout"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_checkout_of_the_wrong_repo_is_detected() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let dir = home.path().join("code/hub");
        write_file(&dir.join(".git/HEAD"), "ref: refs/heads/main\n").await;
        ctx.config.project_path = Some(dir.to_string_lossy().into());
        ctx.runner = Arc::new(FakeRunner::new().with(
            "git -C",
            0,
            "git@github.com:Clubria/some-other-repo.git",
            "",
        ));
        let status = Project.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("some-other-repo"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn the_right_checkout_is_satisfied() {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let dir = home.path().join("code/hub");
        write_file(&dir.join(".git/HEAD"), "ref: refs/heads/main\n").await;
        ctx.config.project_path = Some(dir.to_string_lossy().into());
        ctx.runner = Arc::new(FakeRunner::new().with(
            "git -C",
            0,
            "git@github.com:Clubria/ai-builders-hub.git",
            "",
        ));
        assert_eq!(Project.check(&ctx).await.unwrap(), Status::Satisfied);
    }
}

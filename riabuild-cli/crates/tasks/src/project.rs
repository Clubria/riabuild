//! Task 5 — the repository, cloned where the developer expects it.

use super::{Ctx, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_paths::{contract_tilde, expand_tilde};
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use std::path::{Path, PathBuf};

pub struct Project;

/// How many times riabuild will re-ask before falling back to its own answer.
const ATTEMPTS: usize = 3;

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

/// Where the checkout should go, offering riabuild's answer and letting the
/// developer say otherwise.
///
/// The default is what riabuild would have done silently before, so a developer
/// with no opinion still makes no decision: Enter, no terminal, or an answer
/// riabuild cannot use all land on it.
///
/// `default` is `Ctx::default_checkout`'s answer, never
/// `paths::default_project_dir`'s. The two differ on a server, where several
/// developers share one Unix account and the platform default is one directory
/// all of them would land in — so offering it here would put one working tree,
/// one set of branches, and one `.env.local` of brokered secrets in front of
/// everybody who pressed Enter.
async fn choose_dir(ctx: &Ctx, home: &Path, default: PathBuf) -> PathBuf {
    let question = format!(
        "The repository will be installed at {}. Choose a different path? (press enter for default)",
        contract_tilde(&default, home)
    );

    // Bounded, because a developer who cannot give a usable path is better
    // served by riabuild picking one than by being asked forever.
    for _ in 0..ATTEMPTS {
        let Some(answer) = ctx.ui.ask(&question) else {
            break;
        };
        let chosen = expand_tilde(&answer, home);
        match objection(ctx, &chosen, &default).await {
            None => return chosen,
            Some(objection) => ctx.ui.warn(&objection),
        }
    }

    ctx.ui.note(&format!(
        "Using {} for the checkout",
        contract_tilde(&default, home)
    ));
    default
}

/// Why riabuild cannot clone into a path the developer typed, if it cannot.
///
/// Checked while they are still being asked. The alternative is learning it
/// from a failed `gh repo clone` several seconds later, by which point the
/// answer has been recorded and the developer has to work out how to change it.
async fn objection(ctx: &Ctx, path: &Path, default: &Path) -> Option<String> {
    if !path.is_absolute() {
        return Some(format!(
            "{} is relative — give a path starting with / or ~/",
            path.display()
        ));
    }

    if let Some(escape) = outside_the_namespace(ctx, path, default) {
        return Some(escape);
    }

    // A checkout already sitting there is not an obstacle: `apply` adopts one,
    // and adopting the checkout a developer already has is half the reason to
    // offer the choice at all.
    if tokio::fs::try_exists(path.join(".git"))
        .await
        .unwrap_or(false)
    {
        return None;
    }

    let Ok(mut reader) = tokio::fs::read_dir(path).await else {
        // Nothing there yet, which is the ordinary case.
        return None;
    };
    match reader.next_entry().await {
        Ok(Some(_)) => Some(format!(
            "{} already has files in it — git will not clone into it",
            path.display()
        )),
        _ => None,
    }
}

/// Why a typed path is not somewhere this developer may put a checkout, if it
/// is not. Always `None` on a laptop, where the whole filesystem is theirs.
///
/// On a server it is not. Several developers share one Unix account and are kept
/// apart only by which directories belong to whom: state under
/// `paths::remote_namespace`, checkouts under their own directory in
/// `paths::remote_project_dir`. The prompt above runs against a real terminal
/// there — `riabuild remote` connects with `ssh -t` — so without this an
/// absolute path typed at it walks straight out of the namespace, and the
/// developer ends up in a colleague's tree: one working tree, one set of
/// branches, and one `.env.local` holding the brokered Infisical secrets of
/// whoever ran riabuild last.
fn outside_the_namespace(ctx: &Ctx, path: &Path, default: &Path) -> Option<String> {
    // Laptops are untouched by this: there is no co-tenant to collide with, and
    // where a developer keeps their own checkout is their business. `?` rather
    // than an `if`, because clippy reads the explicit form as a `?` waiting to
    // happen — the meaning is "not a server, no objection".
    ctx.server.as_ref()?;
    let home = ctx.paths.home();

    // The developer's own directory under the org folder. Taken from the
    // *parent* of the default rather than rebuilt, so somebody whose GitHub
    // login was already claimed — `Ctx::default_checkout` hands them `<login>-2`
    // — is allowed their own directory and not the first one.
    let own = default.parent().unwrap_or(default);
    // `Path::starts_with` compares whole components, so `~/Clubria/ada-2` is not
    // inside `~/Clubria/ada`. A string prefix test would have said it was.
    if path.starts_with(own) {
        return None;
    }
    // The state namespace is this developer's alone too. An odd place for a
    // checkout, but not a shared one, so there is nothing here to refuse.
    if let Some(member) = ctx.member.as_ref()
        && path.starts_with(riabuild_paths::remote_namespace(&home, &member.member_id))
    {
        return None;
    }

    Some(format!(
        "{} is not yours on this server — several developers share this account, \
         so the checkout has to sit under {}",
        path.display(),
        contract_tilde(own, &home)
    ))
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
                    &RunOptions::default(),
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
            // Marks this checkout as this namespace's own, so a re-run of
            // `Ctx::default_checkout` recognises its own tree instead of
            // claiming the next suffix beside it every time. `create_dir_all`
            // is a no-op after a real clone (the directory already exists);
            // it exists here mainly so a faked `gh repo clone` in tests has
            // somewhere to write the marker.
            tokio::fs::create_dir_all(&dir).await?;
            tokio::fs::write(
                dir.join(".riabuild-owner"),
                ctx.paths.root().to_string_lossy().as_bytes(),
            )
            .await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
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
        let marker = tokio::fs::read_to_string(expected.join(".riabuild-owner"))
            .await
            .expect("apply() must leave an owner marker");
        assert_eq!(marker, ctx.paths.root().to_string_lossy());
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

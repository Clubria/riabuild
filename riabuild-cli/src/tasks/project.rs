//! Task 5 — the repository, cloned where the developer expects it.

use super::{Ctx, Status, Task, TaskId};
use crate::paths::{contract_tilde, expand_tilde};
use crate::runner::RunOptions;
use crate::ui::Failure;
use anyhow::Result;
use async_trait::async_trait;
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
async fn choose_dir(ctx: &Ctx, home: &Path, repo_name: &str) -> PathBuf {
    // Where this lands is riabuild's decision, and it differs per platform —
    // see `paths::default_project_dir`.
    let default = crate::paths::default_project_dir(home, repo_name);
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
        match objection(&chosen).await {
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
async fn objection(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return Some(format!(
            "{} is relative — give a path starting with / or ~/",
            path.display()
        ));
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
        let Some(org) = ctx.org.as_ref() else {
            return Ok(Status::needs("waiting for sign-in"));
        };
        match origin_url(ctx, &dir).await {
            None => Ok(Status::needs("that checkout has no `origin` remote")),
            Some(remote) if org.matches_remote(&remote) => Ok(Status::Satisfied),
            Some(remote) => Ok(Status::needs(format!(
                "that checkout points at {remote}, not {}",
                org.repo_slug
            ))),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let org = ctx.org()?.clone();
        let home = ctx.paths.home();

        let dir = match ctx.config.project_path.clone() {
            Some(path) => expand_tilde(&path, &home),
            None => choose_dir(ctx, &home, org.repo_name()).await,
        };

        if tokio::fs::try_exists(&dir.join(".git"))
            .await
            .unwrap_or(false)
        {
            // Already a checkout: verify rather than clone over it. Cloning into
            // an existing directory would fail, and deleting it could destroy
            // uncommitted work.
            if let Some(remote) = origin_url(ctx, &dir).await
                && !org.matches_remote(&remote)
            {
                return Err(Failure::new(
                    format!("using {} for the Clubria repo", dir.display()),
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
            ctx.ui.note(&format!("Cloning {}…", org.repo_slug));
            // Through `gh` so the developer's existing GitHub auth is reused and
            // nobody has to set up SSH keys to get started.
            let output = ctx
                .runner
                .run(
                    &ctx.gh(),
                    &["repo", "clone", &org.repo_slug, &dir.to_string_lossy()],
                    &RunOptions::default(),
                )
                .await?;
            if !output.ok() {
                return Err(Failure::new(
                    format!("cloning {}", org.repo_slug),
                    "Check you can open the repository on github.com, then run `riabuild` again."
                        .to_string(),
                )
                .command(format!("gh repo clone {}", org.repo_slug))
                .detail(output.stderr)
                .into());
            }
        }

        ctx.config.project_path = Some(dir.to_string_lossy().into_owned());
        ctx.config.save(ctx.paths.as_ref()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};
    use crate::ui::Ui;
    use std::sync::Arc;

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

        let expected = crate::paths::default_project_dir(home.path(), "ai-builders-hub");
        assert_eq!(
            ctx.config.project_path.as_deref(),
            Some(expected.to_string_lossy().as_ref()),
            "the checkout must land where this platform puts it"
        );
        // Named after the repository, not the whole owner/repo slug.
        assert!(expected.ends_with("ai-builders-hub"), "{expected:?}");
    }

    #[tokio::test]
    async fn a_typed_answer_is_used_instead_of_the_default() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        let chosen = home.path().join("work/hub");
        ctx.ui = Ui::scripted([chosen.to_string_lossy().as_ref()]);

        Project.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.config.project_path.as_deref(),
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

        let default = crate::paths::default_project_dir(home.path(), "ai-builders-hub");
        assert_eq!(
            ctx.config.project_path.as_deref(),
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

        let default = crate::paths::default_project_dir(home.path(), "ai-builders-hub");
        assert_eq!(
            ctx.config.project_path.as_deref(),
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
            ctx.config.project_path.as_deref(),
            Some(existing.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn an_explicit_project_path_still_wins() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("gh repo clone", 0, "", "")).await;
        let chosen = home.path().join("elsewhere/hub");
        ctx.config.project_path = Some(chosen.to_string_lossy().into());
        Project.apply(&mut ctx).await.unwrap();

        assert_eq!(
            ctx.config.project_path.as_deref(),
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

//! Task 5 — the repository, cloned where the developer expects it.

use super::{Ctx, Status, Task, TaskId};
use crate::paths::{contract_tilde, expand_tilde};
use crate::runner::RunOptions;
use crate::ui::Failure;
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

pub struct Project;

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

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory chosen yet"));
        };
        if !dir.exists() {
            return Ok(Status::needs(format!(
                "{} does not exist",
                contract_tilde(&dir, &ctx.paths.home())
            )));
        }
        if !dir.join(".git").exists() {
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

        let chosen = match ctx.config.project_path.clone() {
            Some(path) => path,
            None => {
                let default = org.default_project_path.clone();
                ctx.ui.note(&format!("Using {default} for the checkout"));
                default
            }
        };
        let dir = expand_tilde(&chosen, &home);

        if dir.join(".git").exists() {
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
                std::fs::create_dir_all(parent)?;
            }
            ctx.ui.note(&format!("Cloning {} …", org.repo_slug));
            // Through `gh` so the developer's existing GitHub auth is reused and
            // nobody has to set up SSH keys to get started.
            let output = ctx
                .runner
                .run(
                    "gh",
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
        ctx.config.save(ctx.paths.as_ref())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};
    use std::sync::Arc;

    #[tokio::test]
    async fn an_unchosen_project_needs_setting_up() {
        let (ctx, _home) = ctx_with(FakeRunner::new());
        assert!(matches!(
            Project.check(&ctx).await.unwrap(),
            Status::Needs(_)
        ));
    }

    #[tokio::test]
    async fn a_missing_directory_is_detected() {
        let (mut ctx, home) = ctx_with(FakeRunner::new());
        ctx.config.project_path = Some(home.path().join("code/hub").to_string_lossy().into());
        let status = Project.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("does not exist"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_checkout_is_detected() {
        let (mut ctx, home) = ctx_with(FakeRunner::new());
        let dir = home.path().join("code/hub");
        std::fs::create_dir_all(&dir).unwrap();
        ctx.config.project_path = Some(dir.to_string_lossy().into());
        let status = Project.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not a git checkout"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_checkout_of_the_wrong_repo_is_detected() {
        let (mut ctx, home) = ctx_with(FakeRunner::new());
        let dir = home.path().join("code/hub");
        write_file(&dir.join(".git/HEAD"), "ref: refs/heads/main\n");
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
        let (mut ctx, home) = ctx_with(FakeRunner::new());
        let dir = home.path().join("code/hub");
        write_file(&dir.join(".git/HEAD"), "ref: refs/heads/main\n");
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

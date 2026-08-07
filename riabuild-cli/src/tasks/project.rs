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
            None => {
                // Where this lands is riabuild's decision, and it differs per
                // platform, and per machine when several developers share one
                // Unix account — see `Ctx::default_checkout`.
                let default = ctx.default_checkout().await;
                ctx.ui.note(&format!(
                    "Using {} for the checkout",
                    contract_tilde(&default, &home)
                ));
                default
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

        ctx.config.project_path = Some(dir.to_string_lossy().into_owned());
        ctx.config.save(ctx.paths.as_ref()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Member;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};
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
            crate::paths::default_project_dir(home.path(), "ai-builders-hub")
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
            ctx.config.project_path.as_deref(),
            Some(expected.to_string_lossy().as_ref()),
            "a server checkout must be grouped under the developer, not the platform default"
        );
        let marker = tokio::fs::read_to_string(expected.join(".riabuild-owner"))
            .await
            .expect("apply() must leave an owner marker");
        assert_eq!(marker, ctx.paths.root().to_string_lossy());
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

//! Task 9 — one `.env.<environment>` per environment, from brokered credentials.
//!
//! The Infisical token is short-lived, arrives per use from riabuild-web, is
//! passed to `infisical` in its environment rather than its arguments (arguments
//! are world-readable through `ps`), and is never written to `~/.riabuild`.
//!
//! **Which** environments a developer gets is riabuild-web's answer, not this
//! task's: `/api/v1/org/config` names them for `check()` and
//! `/api/v1/secrets/token` names them again for `apply()`. A developer who may
//! see staging gets `.env.dev` and `.env.staging`; one who may not gets
//! `.env.dev` alone, and is never asked to hold a file Infisical would refuse
//! to fill. Deciding it here would put an authorization rule on the laptop and
//! duplicate one that already lives in Infisical's RBAC.
//!
//! The task id is still `env_local`, which is historical: it wrote a single
//! `.env.local` before environments were plural. The id is a `state.json` key
//! and an e2e handle, so it stays. An existing `.env.local` is left exactly
//! where it is — riabuild no longer refreshes it, but it is not riabuild's to
//! delete out of a developer's checkout either.

mod file;

pub(crate) use file::ensure_ignored;
pub use file::parses_as_dotenv;

use file::{env_file, env_file_name, is_ignored, is_world_or_group_readable, write_private};

use super::{Ctx, SecretScope, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_api::secrets::{self, is_safe_environment_name};
use riabuild_paths::config::modified_millis;
use riabuild_runner::RunOptions;
use riabuild_ui::{Failure, duration_words};

pub struct EnvLocal;

#[async_trait]
impl Task for EnvLocal {
    fn id(&self) -> TaskId {
        "env_local"
    }

    fn title(&self) -> &str {
        "Project secrets"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        &["login", "infisical_cli", "project"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(project) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory yet"));
        };
        let Some(org) = ctx.org.as_ref() else {
            return Ok(Status::needs("waiting for sign-in"));
        };
        // Read off the `Ctx` rather than fetched: `provision` asked once,
        // before the engine started. See `Ctx::load_secret_scope`.
        let environments = match &ctx.secret_scope {
            // Not a fallback to the single `.env.local` this task used to
            // write: a deployment that names no environments is one nobody has
            // updated, and quietly taking the old path would leave a developer
            // with a file riabuild has stopped refreshing and no sign of it.
            SecretScope::OrgWide if org.secret_environments.is_empty() => {
                return Ok(Status::needs(
                    "riabuild.clubria.com has not published its secret environments yet",
                ));
            }
            SecretScope::OrgWide => &org.secret_environments,
            // The whole point of the mapping table: a lead said this
            // repository has no environment variables, so a checkout with no
            // `.env.*` in it is provisioned rather than broken. Anything
            // already in the developer's checkout is left where it is —
            // riabuild does not delete a developer's files out from under them.
            SecretScope::Unmapped => return Ok(Status::Satisfied),
            // "We could not tell" never renders as "you have no secrets".
            SecretScope::Unavailable(_) => {
                return Ok(Status::needs(
                    "riabuild could not ask which secrets this repository uses",
                ));
            }
            // A mapped repository whose folders are in no environment this
            // developer can reach is a path nobody can act on from here.
            // `needs` sends it to `apply()`, which says so and stops the run —
            // rather than `Satisfied`, which would quietly hand somebody an
            // empty checkout on every run for ever.
            SecretScope::Mapped { environments, .. } if environments.is_empty() => {
                return Ok(Status::needs(
                    "no Infisical environment holds the folders this repository is mapped to",
                ));
            }
            SecretScope::Mapped { environments, .. } => environments,
        };

        // A folder moved is as stale as a secret rotated, and the file cannot
        // tell the difference — both leave contents that were right when they
        // were written and are not now.
        let stale_before = org.secrets_updated_at.max(ctx.secret_scope.mapped_at());

        for environment in environments {
            if !is_safe_environment_name(environment) {
                return Ok(Status::needs(format!(
                    "{environment:?} is not a usable environment name"
                )));
            }
            let name = env_file_name(environment);
            let file = env_file(&project, environment);

            if !tokio::fs::try_exists(&file).await.unwrap_or(false) {
                return Ok(Status::needs(format!("{name} is missing")));
            }
            let Ok(text) = tokio::fs::read_to_string(&file).await else {
                return Ok(Status::needs(format!("{name} cannot be read")));
            };
            if !parses_as_dotenv(&text) {
                return Ok(Status::needs(format!("{name} is not a readable env file")));
            }
            // Drift nothing else here can see. The contents can be perfect and
            // current while the file is `0644`, which is what a developer who
            // `touch`ed it first — or a riabuild older than the atomic write —
            // leaves behind, and it stays that way for ever because a refill
            // never creates the file again. On a shared server that is one
            // developer's brokered Infisical secrets readable by every other
            // account on the box.
            if is_world_or_group_readable(&file).await == Some(true) {
                return Ok(Status::needs(format!(
                    "{name} holds brokered secrets and is readable by other accounts"
                )));
            }
            // Rotation the file cannot see by itself: the team rotated
            // secrets, or moved this repository to another folder, after this
            // file was written.
            if modified_millis(&file).await < stale_before {
                return Ok(Status::needs(format!(
                    "the team changed these secrets after {name} was written"
                )));
            }
            if !is_ignored(ctx, &project, &name).await? {
                return Ok(Status::needs(format!("{name} is not ignored by git")));
            }
        }

        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let project = ctx
            .project_dir()
            .ok_or_else(|| anyhow::anyhow!("no project directory chosen"))?;
        let repo = ctx.repo()?;

        match &ctx.secret_scope {
            SecretScope::Unmapped => {
                ctx.note(format!(
                    "{} has no Infisical folder in the riabuild dashboard, so riabuild wrote no .env files for it",
                    repo.slug()
                ));
                return Ok(());
            }
            SecretScope::Unavailable(detail) => {
                return Err(Failure::new(
                    "fetching your project secrets",
                    "Run `riabuild` again. If it keeps failing, ask your team lead whether riabuild.clubria.com is up.",
                )
                .detail(format!(
                    "riabuild could not ask which secrets {} uses: {detail}",
                    repo.slug()
                ))
                .into());
            }
            SecretScope::Mapped { environments, .. } if environments.is_empty() => {
                return Err(Failure::new(
                    "fetching your project secrets",
                    "Ask your team lead to check this repository's Infisical folders in the riabuild dashboard.",
                )
                .detail(format!(
                    "no Infisical environment holds every folder mapped to {}",
                    repo.slug()
                ))
                .into());
            }
            SecretScope::Mapped { .. } | SecretScope::OrgWide => {}
        }

        let brokered = match &ctx.secret_scope {
            SecretScope::OrgWide => secrets::broker(&ctx.api).await?,
            _ => secrets::broker_for(&ctx.api, repo.slug()).await?,
        };
        // Read before the token, because an unmapped reply carries the
        // credential fields present and empty. This is the race `check()`
        // cannot close: a lead removing the mapping between the two calls.
        if brokered.configured == Some(false) {
            ctx.note(format!(
                "{} was unmapped while riabuild was running, so it wrote no .env files for it",
                repo.slug()
            ));
            return Ok(());
        }
        let environments = &brokered.environments;
        if environments.is_empty() {
            return Err(Failure::new(
                "fetching your project secrets",
                "Ask your team lead to deploy the current riabuild-web — this riabuild needs it to say which environments you may pull.",
            )
            .detail("riabuild.clubria.com named no secret environments")
            .into());
        }
        // Every name is checked before anything is written, so a bad list
        // cannot leave a checkout half-filled.
        for environment in environments {
            if !is_safe_environment_name(environment) {
                return Err(Failure::new(
                    "fetching your project secrets",
                    "Ask your team lead to check INFISICAL_ENVIRONMENT and INFISICAL_STAGING_ENVIRONMENT on the riabuild deployment.",
                )
                .detail(format!(
                    "riabuild.clubria.com named an environment riabuild will not turn into a filename: {environment:?}"
                ))
                .into());
            }
        }

        // Ignore them *before* writing them, so a secrets file never exists
        // inside a checkout that would commit it.
        for environment in environments.clone() {
            ensure_ignored(ctx, &project, &env_file_name(&environment)).await?;
        }

        // One environment's secrets can live in more than one folder, so a pull
        // is a fold over folders rather than a single export. The order is
        // riabuild-web's, and it is the order they are merged in: later wins.
        let paths = brokered.export_paths();

        ctx.ui.note("Fetching your secrets from Infisical…");
        for environment in environments {
            let mut exports: Vec<String> = Vec::with_capacity(paths.len());

            for path in &paths {
                // Every failure below names the folder as well as the
                // environment. A 404 on a folder that has been moved or
                // renamed is the likeliest way this task fails, and a message
                // that says only `--env=dev` leaves the one fact that would
                // explain it in nobody's hands.
                let attempted =
                    format!("infisical export --format=dotenv --env={environment} --path={path}");

                let output = ctx
                    .runner
                    .run(
                        &ctx.infisical(),
                        &[
                            "export",
                            "--format=dotenv",
                            &format!("--projectId={}", brokered.project_id),
                            &format!("--env={environment}"),
                            &format!("--path={path}"),
                        ],
                        &RunOptions {
                            cwd: Some(project.clone()),
                            // In the environment, not the argument list.
                            env: vec![
                                ("INFISICAL_TOKEN".into(), brokered.token.clone()),
                                (
                                    "INFISICAL_API_URL".into(),
                                    format!("{}/api", brokered.site_url),
                                ),
                            ],
                            ..Default::default()
                        },
                    )
                    .await?;

                if !output.ok() {
                    return Err(Failure::new(
                        format!("fetching your {environment} secrets"),
                        "Run `riabuild` again. If it keeps failing, ask your team lead to check your Infisical access.",
                    )
                    .command(attempted)
                    .detail(output.stderr)
                    .into());
                }

                // Per folder rather than over the merged file: a folder that
                // answers with nothing is a folder that has moved, and saying
                // so beats writing the half that did answer and leaving the
                // developer to find out which half at `pnpm dev`.
                if !parses_as_dotenv(&output.stdout) {
                    return Err(Failure::new(
                        format!("fetching your {environment} secrets"),
                        "Ask your team lead to check the team's Infisical project has secrets in it.",
                    )
                    .command(attempted)
                    .detail(format!(
                        "Infisical returned no secrets for the {environment} environment at {path}"
                    ))
                    .into());
                }

                exports.push(output.stdout);
            }

            // One folder writes back exactly what Infisical returned, which is
            // what every deployment before this got and what its developers'
            // files already look like. Merging is for the case that needs it.
            let contents = match exports.as_slice() {
                [only] => only.clone(),
                many => file::merge_dotenv(many),
            };

            write_private(&env_file(&project, environment), &contents).await?;
        }

        // The token itself is gone the moment this function returns; only the
        // fact that one was used is worth telling the developer about, and
        // both durations are relative because an epoch-ms timestamp is not
        // something a developer can read.
        let now = riabuild_paths::config::now_millis();
        for note in pull_notes(
            environments,
            brokered.expires_at.saturating_sub(now) / 60_000,
            (brokered.secrets_updated_at > 0)
                .then(|| now.saturating_sub(brokered.secrets_updated_at) / 60_000),
        ) {
            ctx.note(note);
        }
        Ok(())
    }
}

/// What the developer is told after a successful pull.
///
/// Pure, and separate from `apply()`, because `apply()` cannot run without a
/// live riabuild-web and Infisical — which would leave the one thing a
/// developer actually reads as the one thing no test covers.
///
/// Every environment past the first gets its own line. That is the indicator
/// that a second set of secrets came down: stated per environment rather than
/// as a fixed "and staging", because the names belong to the deployment, and a
/// developer who may not see staging must never be shown a line claiming they
/// have it.
fn pull_notes(
    environments: &[String],
    valid_minutes: u64,
    rotated_minutes_ago: Option<u64>,
) -> Vec<String> {
    let mut notes = Vec::new();
    let Some(base) = environments.first() else {
        return notes;
    };
    notes.push(format!(
        "secrets written to {} from the {base} environment (credential valid another {})",
        env_file_name(base),
        duration_words(valid_minutes),
    ));
    for environment in &environments[1..] {
        notes.push(format!(
            "{environment} secrets written to {}",
            env_file_name(environment),
        ));
    }
    if let Some(ago) = rotated_minutes_ago {
        notes.push(format!(
            "the team last rotated these secrets {} ago",
            duration_words(ago),
        ));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx_with, write_file};
    use riabuild_runner::FakeRunner;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn ignored_runner() -> FakeRunner {
        FakeRunner::new().with("git -C", 0, "", "")
    }

    #[test]
    fn accepts_a_real_dotenv_file() {
        assert!(parses_as_dotenv("FOO=bar\n# comment\n\nBAZ=qux\n"));
        assert!(parses_as_dotenv("export TOKEN=abc\n"));
    }

    #[test]
    fn rejects_html_and_empty_files() {
        // What a captive-portal or an error page looks like when it lands in a
        // file that is supposed to hold secrets.
        assert!(!parses_as_dotenv("<html><body>Access denied</body></html>"));
        assert!(!parses_as_dotenv(""));
        assert!(!parses_as_dotenv("# only comments\n"));
    }

    #[test]
    fn folders_are_merged_the_way_a_dotenv_loader_reads_them() {
        // The Infisical layout AI Builders moved to on 2026-08-29: the
        // `VITE_*` in one folder, the credentials in another, and one key in
        // both. The credential folder is exported last, so it is the one that
        // survives — the same rule the checkout's own pull script applies.
        let frontend =
            "VITE_SITE_URL='https://aib.club'\nVITE_CONVEX_URL='https://api.convex.dev.aib.club'\n"
                .to_string();
        let convex = "CONVEX_SELF_HOSTED_ADMIN_KEY='key'\nVITE_SITE_URL='https://dev.aib.club'\n"
            .to_string();

        let merged = file::merge_dotenv(&[frontend, convex]);

        assert_eq!(
            merged,
            "VITE_SITE_URL='https://dev.aib.club'\n\
             VITE_CONVEX_URL='https://api.convex.dev.aib.club'\n\
             CONVEX_SELF_HOSTED_ADMIN_KEY='key'\n",
        );
        // And the file a developer opens says each thing once, rather than
        // twice with nothing to say which the app gets.
        assert_eq!(merged.matches("VITE_SITE_URL=").count(), 1);
        assert!(parses_as_dotenv(&merged));
    }

    #[test]
    fn merging_keeps_the_order_the_folders_were_named_in() {
        // First appearance, so the folders stay recognisable in the result:
        // a key the second folder overrides keeps the first folder's position
        // rather than jumping to the end.
        let merged = file::merge_dotenv(&[
            "A=1\nB=2\n".to_string(),
            "B=3\nC=4\n".to_string(),
            "# a comment\n\nA=5\n".to_string(),
        ]);
        assert_eq!(merged, "A=5\nB=3\nC=4\n");
    }

    /// A checkout with a current, parseable file for each named environment.
    ///
    /// Written through `write_private`, not `write_file`, because "provisioned"
    /// now includes the mode: `write_file` lands at the umask, which `check()`
    /// is entitled to call drift.
    async fn provisioned_project(ctx: &mut Ctx, home: &TempDir, environments: &[&str]) -> PathBuf {
        let project = home.path().join("code/hub");
        for environment in environments {
            write_private(&env_file(&project, environment), "FOO=bar\n")
                .await
                .expect("write");
        }
        tokio::fs::create_dir_all(&project).await.unwrap();
        ctx.config.project_path = Some(project.to_string_lossy().into());
        project
    }

    /// The mode a file has now.
    #[cfg(unix)]
    async fn mode_of(path: &Path) -> Option<u32> {
        use std::os::unix::fs::PermissionsExt;
        let meta = tokio::fs::metadata(path).await.ok()?;
        Some(meta.permissions().mode() & 0o777)
    }

    /// Loosens a file the way a `touch`, an editor, or a riabuild older than
    /// the atomic write would have left it.
    #[cfg(unix)]
    async fn loosen(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
            .await
            .expect("chmod");
    }

    fn sees(ctx: &mut Ctx, environments: &[&str]) {
        if let Some(org) = ctx.org.as_mut() {
            org.secret_environments = environments.iter().map(|name| name.to_string()).collect();
        }
    }

    #[tokio::test]
    async fn a_missing_file_is_detected_and_named() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &[]).await;
        let status = EnvLocal.check(&ctx).await.unwrap();
        // Naming the file matters: with more than one, "it is missing" does
        // not tell the developer which pull failed.
        assert!(
            format!("{status:?}").contains(".env.dev is missing"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_developer_who_may_see_staging_is_not_satisfied_by_dev_alone() {
        // The regression this whole change would otherwise introduce: a
        // half-provisioned checkout reporting itself as done.
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev"]).await;
        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains(".env.staging is missing"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_developer_who_may_not_see_staging_is_satisfied_by_dev_alone() {
        // The other half: a candidate must not be held to a file Infisical
        // would refuse to fill, or the task can never go green for them.
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        sees(&mut ctx, &["dev"]);
        provisioned_project(&mut ctx, &home, &["dev"]).await;
        assert_eq!(EnvLocal.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn both_current_ignored_files_are_satisfied() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev", "staging"]).await;
        assert_eq!(EnvLocal.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_stale_env_local_from_an_older_riabuild_is_not_mistaken_for_a_pull() {
        // riabuild used to write `.env.local` and now does not. The old file is
        // left in the checkout — it is not riabuild's to delete — but it must
        // not satisfy anything either.
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        let project = provisioned_project(&mut ctx, &home, &[]).await;
        write_file(&project.join(".env.local"), "FOO=bar\n").await;
        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains(".env.dev is missing"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_rotated_secret_makes_an_existing_file_stale() {
        // The case a file-exists check misses entirely.
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev", "staging"]).await;
        if let Some(org) = ctx.org.as_mut() {
            org.secrets_updated_at = u64::MAX / 2;
        }
        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("changed"), "{status:?}");
    }

    /// A lead moving a repository to another Infisical folder is the same kind
    /// of staleness as a rotation, and the file can see neither.
    ///
    /// Without this the contents stay perfectly parseable, current-looking and
    /// wrong: `.env.dev` goes on holding the folder's secrets from before the
    /// move, on every run, for ever.
    #[tokio::test]
    async fn moving_a_repository_to_another_folder_makes_its_files_stale() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev"]).await;
        ctx.secret_scope = SecretScope::Mapped {
            environments: vec!["dev".into()],
            updated_at: u64::MAX / 2,
        };

        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("changed"), "{status:?}");
    }

    /// The whole point of the mapping table: a repository a lead gave no
    /// Infisical folder is provisioned, not broken.
    ///
    /// Before this, such a repository got the org's folders copied into its
    /// checkout, or — where Infisical had nothing to give — a hard failure on
    /// every single run, on a machine with nothing wrong with it.
    #[tokio::test]
    async fn an_unmapped_repository_wants_no_env_files_at_all() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &[]).await;
        ctx.secret_scope = SecretScope::Unmapped;

        assert_eq!(EnvLocal.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    /// And the files it already has are left alone rather than reported.
    ///
    /// riabuild stopping refreshing a file is not riabuild deleting it, and a
    /// `check()` that complained about one would be asking `apply()` to remove
    /// a developer's file — which it does not do.
    #[tokio::test]
    async fn unmapping_a_repository_does_not_disturb_files_already_there() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        let project = provisioned_project(&mut ctx, &home, &["dev", "staging"]).await;
        ctx.secret_scope = SecretScope::Unmapped;

        assert_eq!(EnvLocal.check(&ctx).await.unwrap(), Status::Satisfied);
        assert!(
            tokio::fs::try_exists(env_file(&project, "dev"))
                .await
                .unwrap()
        );
    }

    /// The environments come from the repository's folders, not from the org.
    ///
    /// `sees` still says dev and staging — the org-wide list — and this
    /// repository is mapped to folders that live in dev and prod. A `check()`
    /// reading the wrong one of those two would report a satisfied checkout
    /// with no `.env.prod` in it.
    #[tokio::test]
    async fn a_mapped_repository_expects_the_environments_its_own_folders_have() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev"]).await;
        ctx.secret_scope = SecretScope::Mapped {
            environments: vec!["dev".into(), "prod".into()],
            updated_at: 0,
        };

        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains(".env.prod is missing"),
            "{status:?}"
        );
    }

    /// And it is satisfied by exactly those, with no `.env.staging` in sight —
    /// the org-wide list would have demanded one.
    #[tokio::test]
    async fn a_mapped_repository_is_satisfied_without_the_orgs_environments() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev", "prod"]).await;
        ctx.secret_scope = SecretScope::Mapped {
            environments: vec!["dev".into(), "prod".into()],
            updated_at: 0,
        };

        assert_eq!(EnvLocal.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    /// "We could not tell" never renders as "you have no secrets".
    ///
    /// This is the distinction that makes `SecretScope::Unavailable` worth
    /// having as its own variant rather than folding into `Unmapped`: a laptop
    /// that could not reach riabuild-web would otherwise report a repository as
    /// deliberately having no environment variables, and go on doing so every
    /// run until somebody noticed the missing files themselves.
    #[tokio::test]
    async fn a_scope_riabuild_could_not_fetch_is_never_read_as_no_secrets() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &[]).await;
        ctx.secret_scope = SecretScope::Unavailable("riabuild-web is down".into());

        let status = EnvLocal.check(&ctx).await.unwrap();
        assert_ne!(status, Status::Satisfied, "{status:?}");
        assert!(
            format!("{status:?}").contains("could not ask"),
            "{status:?}"
        );
    }

    /// A mapped repository whose folders are in no environment is a typo
    /// somebody has to fix, and `check()` must not settle into reporting a
    /// satisfied machine with an empty checkout.
    #[tokio::test]
    async fn folders_no_environment_holds_are_reported_rather_than_accepted() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &[]).await;
        ctx.secret_scope = SecretScope::Mapped {
            environments: Vec::new(),
            updated_at: 0,
        };

        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("no Infisical environment"),
            "{status:?}"
        );
    }

    /// A deployment with no mapping table behaves exactly as it did before it
    /// existed. This is the whole compatibility story in one assertion.
    #[tokio::test]
    async fn a_deployment_without_the_mapping_table_still_uses_the_org_list() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev", "staging"]).await;
        assert_eq!(ctx.secret_scope, SecretScope::OrgWide, "the default");

        assert_eq!(EnvLocal.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_broken_second_file_is_caught_and_named() {
        // A loop that returned Satisfied as soon as the first file looked good
        // would miss this, and staging would go on being wrong forever. Asserted
        // through a corrupt file rather than a stale mtime, so the test does not
        // depend on the filesystem's timestamp resolution.
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        let project = provisioned_project(&mut ctx, &home, &["dev"]).await;
        // What a captive portal leaves behind in place of secrets.
        write_file(
            &project.join(".env.staging"),
            "<html><body>Access denied</body></html>",
        )
        .await;

        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains(".env.staging is not a readable env file"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_secrets_file_git_would_commit_is_a_failure() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        provisioned_project(&mut ctx, &home, &["dev", "staging"]).await;
        // `git check-ignore` exits non-zero: the file is *not* ignored.
        ctx.runner = Arc::new(FakeRunner::new().with("git -C", 1, "", ""));
        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not ignored"), "{status:?}");
    }

    #[tokio::test]
    async fn a_deployment_that_named_no_environments_is_reported_not_guessed() {
        // Never a quiet fall back to the single `.env.local` this task used to
        // write: that would leave a developer holding a file riabuild has
        // stopped refreshing, with nothing on screen saying so.
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        sees(&mut ctx, &[]);
        provisioned_project(&mut ctx, &home, &["dev", "staging"]).await;
        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("has not published its secret environments"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn an_environment_name_that_would_escape_the_checkout_is_refused() {
        // `.env.<name>` is joined onto the project directory, so this one would
        // otherwise be a write into the developer's home directory.
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        sees(&mut ctx, &["../../.bashrc"]);
        provisioned_project(&mut ctx, &home, &[]).await;
        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not a usable environment name"),
            "{status:?}"
        );
    }

    #[test]
    fn the_notes_say_which_file_each_environment_landed_in() {
        let notes = pull_notes(&["dev".to_string(), "staging".to_string()], 12, Some(120));
        assert!(notes[0].contains("secrets written to .env.dev from the dev environment"));
        // The indicator the developer reads to know staging came down too.
        assert_eq!(notes[1], "staging secrets written to .env.staging");
        assert!(notes[2].contains("last rotated"));
    }

    #[test]
    fn a_developer_who_pulled_only_dev_is_told_nothing_about_staging() {
        let notes = pull_notes(&["dev".to_string()], 12, None);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(!notes.concat().contains("staging"), "{notes:?}");
    }

    #[tokio::test]
    async fn excluding_the_file_does_not_touch_the_tracked_gitignore() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("git -C", 1, "", "")).await;
        let project = home.path().join("code/hub");
        write_file(&project.join(".git/HEAD"), "ref: refs/heads/main\n").await;
        write_file(&project.join(".gitignore"), "node_modules\n").await;

        ensure_ignored(&mut ctx, &project, ".env.dev")
            .await
            .unwrap();

        let exclude = tokio::fs::read_to_string(project.join(".git/info/exclude"))
            .await
            .unwrap();
        assert!(exclude.contains(".env.dev"));
        // The developer's next `git status` must look exactly as it did before.
        assert_eq!(
            tokio::fs::read_to_string(project.join(".gitignore"))
                .await
                .unwrap(),
            "node_modules\n"
        );
    }

    #[tokio::test]
    async fn every_environment_gets_its_own_exclude_line() {
        // Never a `.env.*` glob: that would also hide a tracked `.env.example`,
        // which is a file a repository is entitled to have.
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("git -C", 1, "", "")).await;
        let project = home.path().join("code/hub");
        write_file(&project.join(".git/HEAD"), "ref: refs/heads/main\n").await;

        ensure_ignored(&mut ctx, &project, ".env.dev")
            .await
            .unwrap();
        ensure_ignored(&mut ctx, &project, ".env.staging")
            .await
            .unwrap();

        let exclude = tokio::fs::read_to_string(project.join(".git/info/exclude"))
            .await
            .unwrap();
        let lines: Vec<_> = exclude.lines().collect();
        assert!(lines.contains(&".env.dev"), "{exclude:?}");
        assert!(lines.contains(&".env.staging"), "{exclude:?}");
        assert!(!exclude.contains(".env.*"), "{exclude:?}");
    }

    #[tokio::test]
    async fn excluding_twice_does_not_duplicate_the_line() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("git -C", 1, "", "")).await;
        let project = home.path().join("code/hub");
        write_file(&project.join(".git/HEAD"), "ref: refs/heads/main\n").await;

        ensure_ignored(&mut ctx, &project, ".env.dev")
            .await
            .unwrap();
        ensure_ignored(&mut ctx, &project, ".env.dev")
            .await
            .unwrap();

        let exclude = tokio::fs::read_to_string(project.join(".git/info/exclude"))
            .await
            .unwrap();
        assert_eq!(exclude.matches(".env.dev").count(), 1);
    }

    /// A `.env.dev` the developer created first is tightened, not merely left
    /// alone.
    ///
    /// This is the case `OpenOptions::mode` never covered: `mode` is the third
    /// argument to `open(2)` and the kernel reads it only when `O_CREAT`
    /// actually creates the file. A `touch .env.dev` before the first
    /// `riabuild`, or a file an older riabuild wrote at the umask, was refilled
    /// with brokered Infisical secrets on every run and stayed world-readable
    /// on every one of them — permanently, because a refill never creates it
    /// again.
    #[cfg(unix)]
    #[tokio::test]
    async fn refilling_a_loose_file_tightens_it() {
        let (_ctx, home) = ctx_with(ignored_runner()).await;
        let file = home.path().join("code/hub/.env.dev");
        write_file(&file, "STALE=1\n").await;
        loosen(&file).await;
        assert_eq!(mode_of(&file).await, Some(0o644), "the case being fixed");

        write_private(&file, "TOKEN=brokered\n")
            .await
            .expect("write");

        assert_eq!(mode_of(&file).await, Some(0o600), "{}", file.display());
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "TOKEN=brokered\n"
        );
    }

    /// And `check()` says so, rather than reporting a satisfied machine.
    ///
    /// Without this the repair above is unreachable: the contents are current
    /// and parseable, so nothing else in `check()` has anything to complain
    /// about, and `apply()` — the only thing that could tighten the file — is
    /// never run.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_loose_secrets_file_is_drift_check_reports() {
        let (mut ctx, home) = ctx_with(ignored_runner()).await;
        sees(&mut ctx, &["dev"]);
        let project = provisioned_project(&mut ctx, &home, &["dev"]).await;
        assert_eq!(
            EnvLocal.check(&ctx).await.unwrap(),
            Status::Satisfied,
            "the same machine, before the mode is loosened"
        );

        loosen(&project.join(".env.dev")).await;

        let status = EnvLocal.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("readable by other accounts"),
            "{status:?}"
        );
    }

    /// A reader holding the file open across a refill gets the whole previous
    /// file, never a truncated one.
    ///
    /// The hazard `truncate(true)` opened is not riabuild being interrupted —
    /// it is that `.env.<env>` is read by programs on their own schedule. A
    /// `pnpm dev` or a `direnv` that looks inside that window starts against
    /// half the secrets, or none, and says nothing about why. Landing by rename
    /// closes it: the old inode is intact until the instant the new name is the
    /// only one, so this open handle can still read every byte of it.
    #[tokio::test]
    async fn a_reader_holding_the_old_file_never_sees_a_truncated_one() {
        use tokio::io::AsyncReadExt;

        let (_ctx, home) = ctx_with(ignored_runner()).await;
        let file = home.path().join("code/hub/.env.dev");
        let before = "A=1\nB=2\nC=3\n";
        write_file(&file, before).await;

        let mut reader = tokio::fs::File::open(&file).await.expect("open");
        write_private(&file, "A=9\n").await.expect("write");

        let mut seen = String::new();
        reader.read_to_string(&mut seen).await.expect("read");
        assert_eq!(seen, before, "the reader was handed a half-written file");
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "A=9\n");
    }

    /// And nothing is left beside it. A temporary that survived would be a
    /// second copy of the brokered secrets, inside a checkout, under a name
    /// `ensure_ignored` never added to `.git/info/exclude`.
    #[tokio::test]
    async fn writing_secrets_leaves_no_temporary_behind() {
        let (_ctx, home) = ctx_with(ignored_runner()).await;
        let project = home.path().join("code/hub");
        write_private(&project.join(".env.dev"), "A=1\n")
            .await
            .expect("write");

        let mut entries = tokio::fs::read_dir(&project).await.expect("read_dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, vec![".env.dev"], "in {}", project.display());
    }
}

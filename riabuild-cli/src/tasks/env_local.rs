//! Task 9 — `.env.local`, filled from brokered credentials.
//!
//! The Infisical token is short-lived, arrives per use from riabuild-web, is
//! passed to `infisical` in its environment rather than its arguments (arguments
//! are world-readable through `ps`), and is never written to `~/.riabuild`.

use super::{Ctx, Status, Task, TaskId};
use crate::api::secrets;
use crate::config::modified_millis;
use crate::runner::RunOptions;
use crate::ui::{Failure, duration_words};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct EnvLocal;

/// True if the text is a readable dotenv file with at least one assignment.
pub fn parses_as_dotenv(text: &str) -> bool {
    let mut assignments = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim().trim_start_matches("export ").trim();
        if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return false;
        }
        assignments += 1;
    }
    assignments > 0
}

fn env_file(project: &Path) -> PathBuf {
    project.join(".env.local")
}

fn is_ignored(ctx: &Ctx, project: &Path) -> Result<bool> {
    let output = ctx.runner.run(
        "git",
        &[
            "-C",
            &project.to_string_lossy(),
            "check-ignore",
            "-q",
            ".env.local",
        ],
        &RunOptions::default(),
    )?;
    Ok(output.ok())
}

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

    fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(project) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory yet"));
        };
        let file = env_file(&project);
        if !file.exists() {
            return Ok(Status::needs(".env.local is missing"));
        }

        let Ok(text) = std::fs::read_to_string(&file) else {
            return Ok(Status::needs(".env.local cannot be read"));
        };
        if !parses_as_dotenv(&text) {
            return Ok(Status::needs(".env.local is not a readable env file"));
        }

        // Rotation the file cannot see by itself: the team rotated secrets after
        // this file was written.
        let Some(org) = ctx.org.as_ref() else {
            return Ok(Status::needs("waiting for sign-in"));
        };
        if modified_millis(&file) < org.secrets_updated_at {
            return Ok(Status::needs(
                "the team rotated secrets after this was written",
            ));
        }

        if !is_ignored(ctx, &project)? {
            return Ok(Status::needs(".env.local is not ignored by git"));
        }

        Ok(Status::Satisfied)
    }

    fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let project = ctx
            .project_dir()
            .ok_or_else(|| anyhow::anyhow!("no project directory chosen"))?;

        // Ignore it *before* writing it, so a secrets file never exists inside a
        // checkout that would commit it.
        ensure_ignored(ctx, &project)?;

        let brokered = secrets::broker(&ctx.api)?;
        ctx.ui.note("Fetching your secrets from Infisical…");

        let output = ctx.runner.run(
            "infisical",
            &[
                "export",
                "--format=dotenv",
                &format!("--projectId={}", brokered.project_id),
                &format!("--env={}", brokered.environment),
                &format!("--path={}", brokered.secret_path),
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
                stdin: None,
            },
        )?;

        if !output.ok() {
            return Err(Failure::new(
                "fetching your project secrets",
                "Run `riabuild` again. If it keeps failing, ask your team lead to check your Infisical access.",
            )
            .command("infisical export --format=dotenv")
            .detail(output.stderr)
            .into());
        }

        if !parses_as_dotenv(&output.stdout) {
            return Err(Failure::new(
                "fetching your project secrets",
                "Ask your team lead to check the team's Infisical project has secrets in it.",
            )
            .command("infisical export --format=dotenv")
            .detail("Infisical returned no secrets")
            .into());
        }

        write_private(&env_file(&project), &output.stdout)?;

        // The token itself is gone the moment this function returns; only the
        // fact that one was used is worth telling the developer about.
        let minutes = brokered
            .expires_at
            .saturating_sub(crate::config::now_millis())
            / 60_000;
        ctx.note(format!(
            "secrets written to .env.local from the {} environment (credential valid another {})",
            brokered.environment,
            duration_words(minutes),
        ));
        if brokered.secrets_updated_at > 0 {
            // Relative, for the same reason as above: an epoch-ms timestamp is
            // not something a developer can read.
            let ago =
                crate::config::now_millis().saturating_sub(brokered.secrets_updated_at) / 60_000;
            ctx.note(format!(
                "the team last rotated these secrets {} ago",
                duration_words(ago),
            ));
        }
        Ok(())
    }
}

/// Adds `.env.local` to `.git/info/exclude` rather than `.gitignore`.
///
/// `.gitignore` is a tracked file: editing it would dirty every developer's
/// checkout and show up in their next diff. `info/exclude` is local, private and
/// does exactly the same job.
fn ensure_ignored(ctx: &mut Ctx, project: &Path) -> Result<()> {
    if is_ignored(ctx, project)? {
        return Ok(());
    }
    let exclude = project.join(".git").join("info").join("exclude");
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut contents = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !contents.lines().any(|line| line.trim() == ".env.local") {
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(".env.local\n");
        std::fs::write(&exclude, contents)?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::{ctx_with, write_file};
    use std::sync::Arc;

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
    fn a_missing_file_is_detected() {
        let (mut ctx, home) = ctx_with(ignored_runner());
        let project = home.path().join("code/hub");
        std::fs::create_dir_all(&project).unwrap();
        ctx.config.project_path = Some(project.to_string_lossy().into());
        let status = EnvLocal.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("missing"), "{status:?}");
    }

    #[test]
    fn a_rotated_secret_makes_an_existing_file_stale() {
        // The case a file-exists check misses entirely.
        let (mut ctx, home) = ctx_with(ignored_runner());
        let project = home.path().join("code/hub");
        write_file(&project.join(".env.local"), "FOO=bar\n");
        ctx.config.project_path = Some(project.to_string_lossy().into());
        if let Some(org) = ctx.org.as_mut() {
            org.secrets_updated_at = u64::MAX / 2;
        }
        let status = EnvLocal.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("rotated"), "{status:?}");
    }

    #[test]
    fn a_secrets_file_git_would_commit_is_a_failure() {
        let (mut ctx, home) = ctx_with(ignored_runner());
        let project = home.path().join("code/hub");
        write_file(&project.join(".env.local"), "FOO=bar\n");
        ctx.config.project_path = Some(project.to_string_lossy().into());
        // `git check-ignore` exits non-zero: the file is *not* ignored.
        ctx.runner = Arc::new(FakeRunner::new().with("git -C", 1, "", ""));
        let status = EnvLocal.check(&ctx).unwrap();
        assert!(format!("{status:?}").contains("not ignored"), "{status:?}");
    }

    #[test]
    fn a_current_ignored_file_is_satisfied() {
        let (mut ctx, home) = ctx_with(ignored_runner());
        let project = home.path().join("code/hub");
        write_file(&project.join(".env.local"), "FOO=bar\n");
        ctx.config.project_path = Some(project.to_string_lossy().into());
        assert_eq!(EnvLocal.check(&ctx).unwrap(), Status::Satisfied);
    }

    #[test]
    fn excluding_the_file_does_not_touch_the_tracked_gitignore() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("git -C", 1, "", ""));
        let project = home.path().join("code/hub");
        write_file(&project.join(".git/HEAD"), "ref: refs/heads/main\n");
        write_file(&project.join(".gitignore"), "node_modules\n");

        ensure_ignored(&mut ctx, &project).unwrap();

        let exclude = std::fs::read_to_string(project.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains(".env.local"));
        // The developer's next `git status` must look exactly as it did before.
        assert_eq!(
            std::fs::read_to_string(project.join(".gitignore")).unwrap(),
            "node_modules\n"
        );
    }

    #[test]
    fn excluding_twice_does_not_duplicate_the_line() {
        let (mut ctx, home) = ctx_with(FakeRunner::new().with("git -C", 1, "", ""));
        let project = home.path().join("code/hub");
        write_file(&project.join(".git/HEAD"), "ref: refs/heads/main\n");

        ensure_ignored(&mut ctx, &project).unwrap();
        ensure_ignored(&mut ctx, &project).unwrap();

        let exclude = std::fs::read_to_string(project.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches(".env.local").count(), 1);
    }
}

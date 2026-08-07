//! riabuild — from "accepted a GitHub org invite" to "running Claude Code
//! against the Clubria codebase with working secrets", without the developer
//! making a single environment decision.

// `unwrap_used` is denied for the shipped binary in `Cargo.toml`. In tests a
// panic *is* the reporting mechanism for a failed precondition, so unwrapping a
// fixture there is correct and this exemption keeps the deny from forcing
// ceremony into every `#[cfg(test)]` module. The lint still applies to the
// binary target, which is the build that reaches a developer's laptop.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod api;
mod cli;
mod config;
mod download;
mod keychain;
mod paths;
mod remote;
mod runner;
mod scope;
mod shell;
mod shims;
mod tasks;
mod testing;
mod ui;
mod update;
mod version;

use anyhow::Result;
use api::{ApiError, org};
use clap::Parser;
use cli::{Cli, Command};
use config::{State, UserConfig};
use paths::{Paths, RealPaths, expand_tilde};
use runner::{CommandRunner, RealRunner};
use std::sync::Arc;
use tasks::{Ctx, engine};
use ui::{Failure, Ui};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let quiet = cli.quiet;

    match run(cli).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            let ui = Ui::new(quiet);
            // Failures carry the four parts a developer needs; anything else is
            // a bug in riabuild and is shown plainly rather than dressed up.
            match error.downcast_ref::<Failure>() {
                Some(failure) => ui.failure(failure),
                None => match error.downcast_ref::<ApiError>() {
                    Some(api_error) => ui.failure(
                        &Failure::new(api_error.message.clone(), api_error.action.clone())
                            .detail(format!("({})", api_error.code)),
                    ),
                    None => ui.failure(
                        &Failure::new(
                            "setting up your machine",
                            "Send this to your team lead — it is a bug in riabuild.",
                        )
                        .detail(format!("{error:#}")),
                    ),
                },
            }
            std::process::exit(1);
        }
    }
}

async fn run(cli: Cli) -> Result<i32> {
    let ui = Ui::new(cli.quiet);
    let scope = scope::Scope::detect();
    let paths: Arc<dyn Paths> = Arc::new(RealPaths::new()?);
    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
    // A server has no keyring an SSH session can unlock, so its own session
    // lives in a file in its namespace instead — see `scope.rs`.
    let session_token_file = scope.is_remote().then(|| paths.session_token_file());
    let keychain: Arc<dyn keychain::Keychain> =
        Arc::from(keychain::for_platform(runner.clone(), session_token_file));

    tokio::fs::create_dir_all(paths.root()).await?;

    let mut ctx = build_ctx(
        &scope,
        paths.clone(),
        runner.clone(),
        keychain.clone(),
        ui,
        UserConfig::load(paths.as_ref()).await,
        State::load(paths.as_ref()).await,
        cli.check || matches!(cli.command, Some(Command::Status)),
    );

    if let Some(project) = &cli.project {
        let expanded = expand_tilde(project, &paths.home());
        ctx.config.project_path = Some(expanded.to_string_lossy().into_owned());
        ctx.config.save(paths.as_ref()).await?;
    }

    match cli.command {
        Some(Command::Logout) => return logout(&mut ctx).await,
        Some(Command::Env) => return print_env(&ctx),
        Some(Command::Shell) => return open_shell(&mut ctx).await,
        Some(Command::Login) => {
            use tasks::Task;
            connect(&mut ctx).await?;
            tasks::login::Login.apply(&mut ctx).await?;
            ctx.ui.info("This machine is signed in to riabuild.");
            return Ok(0);
        }
        Some(Command::Remote { .. }) => {
            // Task 21 replaces this with `remote::run(&mut ctx, &cli, target,
            // action).await`. Until then this only needs to exist so the CLI
            // surface (Task 14) compiles and its own tests pass.
            return Ok(0);
        }
        Some(Command::Status) | None => {}
    }

    provision(&mut ctx, &cli).await
}

/// Assembles the `Ctx` a run works against.
///
/// Split out of `run` so the one field that comes from `Scope` — `server` —
/// is testable without standing up `RealPaths::new()`, a real `ApiClient`, or
/// a platform keychain. `run` is the only caller. `Ctx.server` is the only
/// remote-mode fact a task is allowed to branch on (see `tasks::Ctx::server`),
/// and this is the one place it is set from the environment riabuild actually
/// found itself in — hardcoding `None` here is the regression that leaves
/// per-developer checkout namespacing (`paths::remote_project_dir`,
/// `Ctx::default_checkout`) dead on every server despite compiling and
/// passing every other test. See ruling R11 in
/// `.superpowers/sdd/2026-08-06-remote-mode/decisions.md`.
#[allow(clippy::too_many_arguments)]
fn build_ctx(
    scope: &scope::Scope,
    paths: Arc<dyn Paths>,
    runner: Arc<dyn CommandRunner>,
    keychain: Arc<dyn keychain::Keychain>,
    ui: Ui,
    config: UserConfig,
    state: State,
    dry_run: bool,
) -> Ctx {
    Ctx {
        paths,
        runner,
        keychain,
        api: api::ApiClient::new(cli::VERSION),
        ui,
        config,
        state,
        org: None,
        member: None,
        server: scope.server.clone(),
        cli_version: cli::VERSION.to_string(),
        web_url: api::web_url(),
        env: Vec::new(),
        notes: Vec::new(),
        dry_run,
    }
}

/// Asks riabuild-web who this machine belongs to, before any task runs.
///
/// A missing or expired session is not an error here — the `login` task exists
/// to fix exactly that. Anything else (suspended, removed from the org) is
/// surfaced immediately, because no amount of provisioning will help.
async fn connect(ctx: &mut Ctx) -> Result<()> {
    let Some(token) = ctx.keychain.get().await? else {
        return Ok(());
    };
    ctx.api.set_token(Some(token));

    match ctx.api.me().await {
        Ok(member) => {
            ctx.member = Some(member);
            ctx.org = Some(org::fetch_config(&ctx.api).await?);
            Ok(())
        }
        Err(error) => match error.downcast_ref::<ApiError>() {
            Some(api_error) if api_error.needs_login() => {
                ctx.api.set_token(None);
                Ok(())
            }
            _ => Err(error),
        },
    }
}

async fn provision(ctx: &mut Ctx, cli: &Cli) -> Result<i32> {
    ctx.ui.banner("Clubria");
    connect(ctx).await?;
    describe_session(ctx);

    // A managed server has no package manager watching this binary, so it must
    // never try to replace itself — see `scope.rs` and `tasks::Ctx::server`.
    if let Some(org) = &ctx.org
        && ctx.server.is_none()
    {
        match update::decide(
            &ctx.cli_version,
            &org.min_cli_version,
            &org.latest_cli_version,
            update::already_updated(),
        ) {
            update::Action::Continue => {}
            update::Action::Upgrade { to, mandatory } => {
                update::upgrade_and_reexec(ctx.runner.as_ref(), &ctx.ui, &to, mandatory).await?;
            }
        }
    }

    ctx.ui.heading("Checking this machine");
    let registry = tasks::registry();
    let outcome = engine::run_all(&registry, ctx).await?;

    shims::write_all(ctx).await?;

    let notes = std::mem::take(&mut ctx.notes);
    if !notes.is_empty() {
        ctx.ui.heading("Worth knowing");
        for note in notes {
            ctx.ui.note(&note);
        }
    }

    log_run(ctx, &outcome).await;

    if ctx.dry_run {
        ctx.ui.info("");
        // "9 item(s) already correct, 0 would be set up." made a fine machine
        // read like a to-do list. The all-clear deserves to say so plainly.
        ctx.ui.info(&if outcome.applied.is_empty() {
            "Everything on this machine is already set up.".to_string()
        } else {
            format!(
                "{} already correct, {} still to set up.",
                ui::plural(outcome.satisfied.len() as u64, "item"),
                outcome.applied.len(),
            )
        });
        return Ok(0);
    }

    if cli.no_shell {
        return Ok(0);
    }
    open_shell(ctx).await
}

/// Who riabuild thinks this machine belongs to, and where the token lives.
///
/// Printed on every run because "riabuild is using the wrong account" is
/// otherwise invisible until something fails for a confusing reason.
fn describe_session(ctx: &Ctx) {
    let Some(member) = &ctx.member else {
        ctx.ui
            .note("not signed in yet — riabuild will open your browser");
        return;
    };
    ctx.ui.note(&format!(
        "signed in as {} <{}> · {} · token in your {}",
        member.display_name(),
        member.email,
        member.role,
        ctx.keychain.describe(),
    ));
}

/// One line per run in `~/.riabuild/logs/riabuild.log`.
///
/// Deliberately never fatal: failing to write a log must not fail a setup that
/// otherwise worked. It exists so "send me your riabuild log" is a useful thing
/// for a team lead to ask.
async fn log_run(ctx: &Ctx, outcome: &engine::Outcome) {
    use tokio::io::AsyncWriteExt;
    let path = ctx.paths.log_file();
    let Some(parent) = path.parent() else { return };
    if tokio::fs::create_dir_all(parent).await.is_err() {
        return;
    }
    let line = format!(
        "{} riabuild {} satisfied={} applied=[{}]\n",
        config::now_secs(),
        ctx.cli_version,
        outcome.satisfied.len(),
        outcome.applied.join(","),
    );
    if let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        let _ = file.write_all(line.as_bytes()).await;
    }
}

async fn open_shell(ctx: &mut Ctx) -> Result<i32> {
    if shell::already_inside() {
        // Nesting would stack PATH entries and leave the developer two `exit`s
        // away from their own terminal.
        ctx.ui
            .info("You are already in the Clubria environment. Type `exit` to leave it.");
        return Ok(0);
    }
    // The banner itself comes from the generated rcfile, inside the new shell —
    // printing it here too is what made every developer see it twice. This blank
    // line is only separation from the task list above.
    ctx.ui.info("");
    shell::spawn(ctx).await
}

async fn logout(ctx: &mut Ctx) -> Result<i32> {
    ctx.keychain.delete().await?;
    ctx.config.session_expires_at = None;
    ctx.config.save(ctx.paths.as_ref()).await?;
    ctx.state.forget("login");
    ctx.state.save(ctx.paths.as_ref()).await?;
    ctx.ui
        .info("This machine is signed out. Run `riabuild` to sign in again.");
    Ok(0)
}

/// Prints the environment as `export` lines, for a developer who would rather
/// paste it into their own shell than use riabuild's.
fn print_env(ctx: &Ctx) -> Result<i32> {
    for (key, value) in shell::environment(ctx) {
        println!("export {key}={}", shell_quote(&value));
    }
    Ok(0)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keychain::MemoryKeychain;
    use crate::runner::FakeRunner;
    use tempfile::TempDir;

    fn ctx_for(scope: &scope::Scope) -> Ctx {
        let home = TempDir::new().expect("tempdir");
        let paths: Arc<dyn Paths> = Arc::new(RealPaths::rooted_at(home.path()));
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let keychain: Arc<dyn keychain::Keychain> = Arc::new(MemoryKeychain::default());
        build_ctx(
            scope,
            paths,
            runner,
            keychain,
            Ui::new(true),
            UserConfig::default(),
            State::default(),
            false,
        )
    }

    #[test]
    fn a_remote_scope_reaches_ctx_server() {
        // This is the assertion R11 exists for: a `Ctx` built from a remote
        // `Scope` must carry the server's name, not the `server: None` this
        // wiring used to hardcode. Revert `build_ctx`'s `server:` line to
        // `None` and this fails.
        let scope = scope::Scope::read(Some("build-01"));
        let ctx = ctx_for(&scope);
        assert_eq!(ctx.server.as_deref(), Some("build-01"));
    }

    #[test]
    fn a_laptop_scope_leaves_ctx_server_empty() {
        let scope = scope::Scope::read(None);
        let ctx = ctx_for(&scope);
        assert_eq!(ctx.server, None);
    }
}

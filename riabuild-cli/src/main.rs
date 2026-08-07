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
mod archive;
mod channel;
mod cli;
mod config;
mod download;
mod fs_move;
mod keychain;
mod move_project;
mod paths;
mod reset;
mod runner;
mod shell;
mod shims;
mod tasks;
mod testing;
mod tools;
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
    let paths: Arc<dyn Paths> = Arc::new(RealPaths::new()?);

    // Dispatched before the setup flow: the shim runs on every Ctrl+V, so it
    // must not check the machine, talk to the API, or print a banner.
    if let Some(Command::Channel { action }) = &cli.command {
        return channel::dispatch(action, cli.quiet).await;
    }

    // Dispatched before anything creates or reads the tree. riabuild must not
    // recreate the directory it is about to remove, and a reset must not depend
    // on a config or state file that may be the reason it was asked for.
    if let Some(Command::Reset { yes }) = &cli.command {
        return reset::run(
            paths.as_ref(),
            &ui,
            reset::Request {
                assume_yes: *yes,
                dry_run: cli.check,
                inside_shell: shell::already_inside(),
            },
        )
        .await;
    }

    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
    let keychain: Arc<dyn keychain::Keychain> = Arc::from(keychain::for_platform(runner.clone()));

    tokio::fs::create_dir_all(paths.root()).await?;

    let mut ctx = Ctx {
        paths: paths.clone(),
        runner: runner.clone(),
        keychain: keychain.clone(),
        api: api::ApiClient::new(cli::VERSION),
        ui,
        config: UserConfig::load(paths.as_ref()).await,
        state: State::load(paths.as_ref()).await,
        org: None,
        member: None,
        cli_version: cli::VERSION.to_string(),
        web_url: api::web_url(),
        env: Vec::new(),
        notes: Vec::new(),
        dry_run: cli.check || matches!(cli.command, Some(Command::Status)),
    };

    if let Some(project) = &cli.project {
        let expanded = expand_tilde(project, &paths.home());
        ctx.config.project_path = Some(expanded.to_string_lossy().into_owned());
        ctx.config.save(paths.as_ref()).await?;
    }

    match cli.command {
        Some(Command::Logout) => return logout(&mut ctx).await,
        Some(Command::Env) => return print_env(&ctx),
        Some(Command::Shell) => return open_shell(&mut ctx).await,
        Some(Command::MoveProject { path }) => {
            return move_project::run(&mut ctx, path.as_deref()).await;
        }
        Some(Command::Login) => {
            use tasks::Task;
            connect(&mut ctx).await?;
            tasks::login::Login.apply(&mut ctx).await?;
            ctx.ui.info("This machine is signed in to riabuild.");
            return Ok(0);
        }
        Some(Command::Reset { .. }) => unreachable!("reset returns before the tree is touched"),
        Some(Command::Channel { .. }) => {
            unreachable!("the channel returns before the setup flow starts")
        }
        Some(Command::Status) | None => {}
    }

    provision(&mut ctx, &cli).await
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

    if let Some(org) = &ctx.org {
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

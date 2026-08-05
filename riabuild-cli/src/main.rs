//! riabuild — from "accepted a GitHub org invite" to "running Claude Code
//! against the Clubria codebase with working secrets", without the developer
//! making a single environment decision.

mod api;
mod cli;
mod config;
mod download;
mod keychain;
mod paths;
mod runner;
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

fn main() {
    let cli = Cli::parse();
    let quiet = cli.quiet;

    match run(cli) {
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

fn run(cli: Cli) -> Result<i32> {
    let ui = Ui::new(cli.quiet);
    let paths: Arc<dyn Paths> = Arc::new(RealPaths::new()?);
    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
    let keychain: Arc<dyn keychain::Keychain> = Arc::from(keychain::for_platform(runner.clone()));

    std::fs::create_dir_all(paths.root())?;

    let mut ctx = Ctx {
        paths: paths.clone(),
        runner: runner.clone(),
        keychain: keychain.clone(),
        api: api::ApiClient::new(cli::VERSION),
        ui,
        config: UserConfig::load(paths.as_ref()),
        state: State::load(paths.as_ref()),
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
        ctx.config.save(paths.as_ref())?;
    }

    match cli.command {
        Some(Command::Logout) => return logout(&mut ctx),
        Some(Command::Env) => return print_env(&ctx),
        Some(Command::Shell) => return open_shell(&mut ctx),
        Some(Command::Login) => {
            use tasks::Task;
            connect(&mut ctx)?;
            tasks::login::Login.apply(&mut ctx)?;
            ctx.ui.info("This machine is signed in to riabuild.");
            return Ok(0);
        }
        Some(Command::Status) | None => {}
    }

    provision(&mut ctx, &cli)
}

/// Asks riabuild-web who this machine belongs to, before any task runs.
///
/// A missing or expired session is not an error here — the `login` task exists
/// to fix exactly that. Anything else (suspended, removed from the org) is
/// surfaced immediately, because no amount of provisioning will help.
fn connect(ctx: &mut Ctx) -> Result<()> {
    let Some(token) = ctx.keychain.get()? else {
        return Ok(());
    };
    ctx.api.set_token(Some(token));

    match ctx.api.me() {
        Ok(member) => {
            ctx.member = Some(member);
            ctx.org = Some(org::fetch_config(&ctx.api)?);
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

fn provision(ctx: &mut Ctx, cli: &Cli) -> Result<i32> {
    ctx.ui.banner("Clubria");
    connect(ctx)?;
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
                update::upgrade_and_reexec(ctx.runner.as_ref(), &ctx.ui, &to, mandatory)?;
            }
        }
    }

    ctx.ui.heading("Checking this machine");
    let registry = tasks::registry();
    let outcome = engine::run_all(&registry, ctx)?;

    shims::write_all(ctx)?;

    let notes = std::mem::take(&mut ctx.notes);
    if !notes.is_empty() {
        ctx.ui.heading("Worth knowing");
        for note in notes {
            ctx.ui.note(&note);
        }
    }

    log_run(ctx, &outcome);

    if ctx.dry_run {
        ctx.ui.info("");
        ctx.ui.info(&format!(
            "{} item(s) already correct, {} would be set up.",
            outcome.satisfied.len(),
            outcome.applied.len()
        ));
        return Ok(0);
    }

    if cli.no_shell {
        return Ok(0);
    }
    open_shell(ctx)
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
fn log_run(ctx: &Ctx, outcome: &engine::Outcome) {
    use std::io::Write;
    let path = ctx.paths.log_file();
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let line = format!(
        "{} riabuild {} satisfied={} applied=[{}]\n",
        config::now_secs(),
        ctx.cli_version,
        outcome.satisfied.len(),
        outcome.applied.join(","),
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn open_shell(ctx: &mut Ctx) -> Result<i32> {
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
    shell::spawn(ctx)
}

fn logout(ctx: &mut Ctx) -> Result<i32> {
    ctx.keychain.delete()?;
    ctx.config.session_expires_at = None;
    ctx.config.save(ctx.paths.as_ref())?;
    ctx.state.forget("login");
    ctx.state.save(ctx.paths.as_ref())?;
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

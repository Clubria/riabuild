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
mod gh_session;
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
use runner::{CommandRunner, RealRunner, RunOptions};
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

    // Only a remote scope claims a GitHub session — see `gh_session`. This is
    // deliberately unconditional over every subcommand a remote-scoped
    // invocation might run, not just the shell — with one exception.
    // `internal gh-sweep`/`internal seed-github` are short plumbing
    // invocations the laptop runs *before* the interactive shell exists (see
    // `holds_gh_session_marker`): if either claimed a marker the same way
    // the shell does, its own exit would find no other marker yet and wipe
    // the GitHub credential moments after `internal seed-github` wrote it —
    // the exact "earlier draft got it backwards" bug `gh_session.rs`'s module
    // doc warns about. Those two only `attach`, which never claims or
    // releases anything.
    let gh_dir: Option<std::path::PathBuf>;
    let mut gh_marker: Option<gh_session::GhSession> = None;
    if scope.is_remote() {
        let runtime = gh_session::choose_runtime_dir(
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            std::env::var("TMPDIR").ok().as_deref(),
        )?;
        let member_id = member_id_from_root(paths.as_ref())?;
        if holds_gh_session_marker(&cli) {
            let session =
                gh_session::GhSession::open(&runtime, &member_id, std::process::id()).await?;
            gh_dir = Some(session.config_dir());
            gh_marker = Some(session);
        } else {
            gh_dir = Some(gh_session::GhSession::attach(&runtime, &member_id).await?);
        }
    } else {
        gh_dir = None;
    }

    // Bound before the shadowing `match` below, which moves `runner` in both
    // arms. `base_runner` is the unwrapped `RealRunner`: `kill -0` (run by
    // `close`, via `sweep`) needs no namespace environment, and closing has
    // to work even once the scoped runner built below is gone.
    let base_runner = runner.clone();
    let runner: Arc<dyn CommandRunner> = match &gh_dir {
        Some(dir) => Arc::new(runner::ScopedRunner::new(
            runner,
            vec![
                ("GH_CONFIG_DIR".into(), dir.to_string_lossy().into_owned()),
                (
                    "GIT_CONFIG_GLOBAL".into(),
                    paths
                        .root()
                        .join("gitconfig")
                        .to_string_lossy()
                        .into_owned(),
                ),
            ],
        )),
        None => runner,
    };

    let mut ctx = build_ctx(
        &scope,
        paths.clone(),
        runner,
        keychain.clone(),
        ui,
        UserConfig::load(paths.as_ref()).await,
        State::load(paths.as_ref()).await,
        cli.check || matches!(cli.command, Some(Command::Status)),
    );

    let code = run_inner(&cli, &mut ctx).await;

    if let Some(session) = gh_marker
        && let Err(error) = session.close(base_runner).await
    {
        // Not `let _`: a credential that failed to wipe is exactly the thing
        // the developer needs told about.
        ctx.ui.warn(&format!(
            "could not remove this session's GitHub sign-in: {error}"
        ));
    }

    code
}

/// The member id a remote-scoped `RIABUILD_ROOT` is namespaced under — the
/// last path component of `paths.root()`.
///
/// Never falls back to an empty string: `gh_session::open`/`attach` join this
/// verbatim onto `riabuild-gh-`, so an empty id is exactly what would make
/// every developer on a shared server collide onto one runtime directory —
/// and share each other's GitHub credential. `paths::root_for` already
/// refuses a `RIABUILD_ROOT` that isn't absolute (Task 6), so the only way
/// `file_name()` comes back empty here is a root of `/` itself, which is
/// worth a clear, actionable error rather than a silent collision.
fn member_id_from_root(paths: &dyn Paths) -> Result<String> {
    paths
        .root()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            Failure::new(
                "working out which developer this server session belongs to",
                "This is a bug in riabuild — send your team lead the value of RIABUILD_ROOT on that server.",
            )
            .into()
        })
}

/// Whether this invocation goes on to hold the interactive environment shell
/// open.
///
/// This is the single source of truth `holds_gh_session_marker` defers to,
/// and it is also, deliberately, the exact condition `provision`'s own tail
/// uses to decide whether to call `open_shell` — see the table in
/// `task-19-brief.md:24-30`: `internal gh-sweep`, the seeding run, and the
/// *setup* run (`riabuild --no-shell` on the server, which is what Task 21's
/// `remote::flow::run` sends over its first SSH hop) all answer `false`; only
/// the interactive shell run answers `true`. Getting this wrong the other
/// way — granting a marker to a `--no-shell` run — is what would make the
/// setup run's own exit sweep away a credential `seed_github` had just
/// written on an earlier hop, before the shell ever sees it.
fn opens_shell(cli: &Cli) -> bool {
    match &cli.command {
        Some(Command::Shell) => true,
        Some(
            Command::Internal { .. }
            | Command::Login
            | Command::Logout
            | Command::Env
            | Command::Remote { .. }
            | Command::Status,
        ) => false,
        None => !cli.check && !cli.no_shell,
    }
}

/// Whether this invocation is allowed to claim (and later release) the
/// GitHub-session marker `gh_session::open`/`close` guard.
///
/// Only the invocation that goes on to hold the interactive environment
/// shell open should ever do that — see `gh_session.rs`'s module doc. Every
/// other invocation — the hidden `internal` subcommands (`gh-sweep`,
/// `seed-github`), and just as importantly the *setup* run, which is an
/// ordinary default-flow invocation with `--no-shell` set — calls `attach`
/// instead, which never claims or releases anything.
fn holds_gh_session_marker(cli: &Cli) -> bool {
    opens_shell(cli)
}

/// Everything `run` does after a remote scope's GitHub session (if any) is
/// open, so `run` can guarantee `close` runs on every return from here —
/// including an error, not just the successful paths dotted through the
/// match below.
async fn run_inner(cli: &Cli, ctx: &mut Ctx) -> Result<i32> {
    if let Some(project) = &cli.project {
        let expanded = expand_tilde(project, &ctx.paths.home());
        ctx.config.project_path = Some(expanded.to_string_lossy().into_owned());
        ctx.config.save(ctx.paths.as_ref()).await?;
    }

    match &cli.command {
        Some(Command::Logout) => return logout(ctx).await,
        Some(Command::Env) => return print_env(ctx),
        Some(Command::Shell) => return open_shell(ctx).await,
        Some(Command::Login) => {
            use tasks::Task;
            connect(ctx).await?;
            tasks::login::Login.apply(ctx).await?;
            ctx.ui.info("This machine is signed in to riabuild.");
            return Ok(0);
        }
        Some(Command::Remote { target, action, .. }) => {
            return remote::run(ctx, cli, target.clone(), action.clone()).await;
        }
        Some(Command::Internal {
            action: cli::InternalAction::GhSweep,
        }) => {
            // Run by the laptop before seeding, so a dead session's leftovers
            // go before the new credential arrives rather than after.
            let runtime = gh_session::choose_runtime_dir(
                std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
                std::env::var("TMPDIR").ok().as_deref(),
            )?;
            let dir =
                gh_session::GhSession::attach(&runtime, &member_id_from_root(ctx.paths.as_ref())?)
                    .await?;
            gh_session::sweep(&dir, ctx.runner.clone(), config::now_secs()).await?;
            return Ok(0);
        }
        Some(Command::Internal {
            action: cli::InternalAction::SeedGithub,
        }) => {
            // `tokio::io`, not `std::io`: a blocking read on the current-thread
            // runtime stalls every other future on it, which is the invariant in
            // riabuild-cli/CLAUDE.md.
            use tokio::io::AsyncReadExt;
            let mut token = String::new();
            tokio::io::stdin().read_to_string(&mut token).await?;
            // `gh` writes its own `hosts.yml`, with its own permissions, into
            // the `GH_CONFIG_DIR` the scoped runner supplies — riabuild never
            // hand-writes that file. The token reaches `gh` only on stdin,
            // never in argv (`ps` is world-readable) and never logged.
            let output = ctx
                .runner
                .run(
                    "gh",
                    &["auth", "login", "--with-token"],
                    &RunOptions {
                        stdin: Some(token.trim().as_bytes().to_vec()),
                        ..Default::default()
                    },
                )
                .await?;
            return Ok(if output.ok() { 0 } else { 1 });
        }
        Some(Command::Status) | None => {}
    }

    provision(ctx, cli).await
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
pub(crate) async fn connect(ctx: &mut Ctx) -> Result<()> {
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

    if !opens_shell(cli) {
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
        // write_all reporting success only means the bytes reached
        // tokio::fs::File's internal buffer, not that the background
        // write() syscall ran — flush is what waits for that.
        let _ = async {
            file.write_all(line.as_bytes()).await?;
            file.flush().await
        }
        .await;
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

    #[test]
    fn member_id_comes_from_the_roots_last_component() {
        let paths = RealPaths::with_root("/home/dev", "/home/dev/.riabuild-remote/550e8400");
        assert_eq!(member_id_from_root(&paths).expect("id"), "550e8400");
    }

    #[test]
    fn a_root_with_no_final_component_is_a_failure_not_an_empty_id() {
        // An empty member id is what would make every developer on a shared
        // server collide onto one runtime directory (and each other's
        // GitHub credential) — this must hard-error, never fall back.
        let paths = RealPaths::with_root("/", "/");
        let error = member_id_from_root(&paths).expect_err("no component to read");
        assert!(
            error.downcast_ref::<Failure>().is_some(),
            "must be the actionable Failure, not a generic error: {error}"
        );
    }

    /// A `Cli` with every field but `command`/`no_shell`/`check` at its
    /// ordinary default, for the marker-predicate tests below — those three
    /// are the only fields `opens_shell` reads.
    fn cli_for(command: Option<Command>, no_shell: bool, check: bool) -> Cli {
        Cli {
            command,
            project: None,
            check,
            quiet: false,
            no_shell,
        }
    }

    #[test]
    fn internal_plumbing_never_claims_the_gh_session_marker() {
        // This is the fix for the bug described in `gh_session.rs`'s module
        // doc: if `internal seed-github` claimed a marker the same way the
        // interactive shell does, its own exit would wipe the credential it
        // had just written. Reverting `holds_gh_session_marker` to always
        // return `true` reproduces that bug and fails this test.
        assert!(!holds_gh_session_marker(&cli_for(
            Some(Command::Internal {
                action: cli::InternalAction::SeedGithub,
            }),
            false,
            false,
        )));
        assert!(!holds_gh_session_marker(&cli_for(
            Some(Command::Internal {
                action: cli::InternalAction::GhSweep,
            }),
            false,
            false,
        )));
    }

    #[test]
    fn the_setup_run_never_claims_the_gh_session_marker_either() {
        // The critical fix from Task 21's review: the *setup* run — an
        // ordinary default-flow invocation with `--no-shell` set, exactly
        // what `remote::flow::run` sends over its first SSH hop — used to be
        // granted a marker by the old `Command`-only predicate. If it were,
        // its own exit would sweep away the credential `seed_github` had
        // just written on an earlier SSH hop, before the interactive shell
        // (a third, later hop) ever saw it.
        assert!(!holds_gh_session_marker(&cli_for(None, true, false)));
        // `--check` never opens a shell either, for the same reason.
        assert!(!holds_gh_session_marker(&cli_for(None, false, true)));
        assert!(!holds_gh_session_marker(&cli_for(
            Some(Command::Status),
            false,
            false
        )));
    }

    #[test]
    fn every_other_command_still_claims_the_gh_session_marker() {
        assert!(holds_gh_session_marker(&cli_for(None, false, false)));
        assert!(holds_gh_session_marker(&cli_for(
            Some(Command::Shell),
            false,
            false
        )));
    }
}

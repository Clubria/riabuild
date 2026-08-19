//! riabuild — from "accepted a GitHub org invite" to "running Claude Code
//! against the Clubria codebase with working secrets", without the developer
//! making a single environment decision.
//!
//! This file is the wiring only: parse argv, assemble the `Ctx` a run works
//! against — including the GitHub-session envelope a remote scope executes
//! inside — and dispatch to whichever module implements the command. The
//! default flow lives in `provision.rs`, the hidden `internal` subcommands in
//! `internal.rs`, and `riabuild remote` in `remote/`. Only `logout`, `env`,
//! and `connect` are small enough to have stayed here.

// `unwrap_used` is denied for the shipped binary in `Cargo.toml`. In tests a
// panic *is* the reporting mechanism for a failed precondition, so unwrapping a
// fixture there is correct and this exemption keeps the deny from forcing
// ceremony into every `#[cfg(test)]` module. The lint still applies to the
// binary target, which is the build that reaches a developer's laptop.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod cli;
mod dispatch;
mod fs_move;
mod internal;
mod move_project;
mod provision;
mod reset;
mod update;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};
use provision::{open_shell, provision};
use std::sync::Arc;

// The library crates, under the names this file has always called them by.
// `riabuild-cli` is wiring: it names every crate and implements none of them.
use riabuild_gh_session as gh_session;
use riabuild_keychain as keychain;
use riabuild_runner as runner;
use riabuild_tasks as tasks;
use riabuild_tasks::scope;
use riabuild_tasks::shell;

use riabuild_api::ApiError;
use riabuild_paths::config::{State, UserConfig};
use riabuild_paths::{Paths, RealPaths, expand_tilde};
use riabuild_runner::{CommandRunner, RealRunner};
use riabuild_tasks::Ctx;
use riabuild_ui::{Failure, Ui};

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

    // Dispatched before the setup flow: the shim runs on every Ctrl+V, so it
    // must not check the machine, talk to the API, or print a banner.
    if let Some(Command::Channel { action }) = &cli.command {
        return dispatch::channel(action, cli.quiet).await;
    }

    // Dispatched here for the same reason as the channel shim above, and more
    // sharply: `ssh` runs this from inside an authentication attempt, several
    // times per `riabuild remote`. Checking the machine or calling the API
    // first would put that work between the developer and every connection —
    // and `ssh` reads this process's stdout as the password, so a banner on it
    // would *be* the answer.
    if let Some(Command::Internal {
        action: cli::InternalAction::Askpass { prompt },
    }) = &cli.command
    {
        let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
        return internal::askpass(paths.as_ref(), runner, &prompt.join(" ")).await;
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
    // A server has no keyring an SSH session can unlock, so its own session
    // lives in a file in its namespace instead — see `scope.rs`.
    let session_token_file = scope.server_session_token_file(paths.as_ref())?;
    // The second path is for a machine that is *not* a managed server and
    // still has no keyring: a headless Linux box someone installed riabuild on
    // directly. Chosen here, before `login` runs, so such a machine never
    // reaches a browser approval for a token it would then have to discard.
    let keychain: Arc<dyn keychain::Keychain> = Arc::from(
        keychain::for_platform(
            runner.clone(),
            session_token_file,
            paths.session_token_file(),
        )
        .await,
    );

    tokio::fs::create_dir_all(paths.root()).await?;

    // Only a remote scope claims a GitHub session — see `gh_session`. This is
    // deliberately unconditional over every subcommand a remote-scoped
    // invocation might run, not just the shell — with one exception.
    // `internal gh-sweep`/`internal seed-github` are short plumbing
    // invocations the laptop runs *before* the interactive shell exists (see
    // `holds_gh_session_marker`): if either claimed a marker the same way
    // the shell does, its own exit would find no other marker yet and wipe
    // the GitHub credential moments after `internal seed-github` wrote it —
    // the exact "earlier draft got it backwards" bug `gh_session`'s module
    // doc warns about. Those two only `attach`, which never claims or
    // releases anything.
    let gh_dir: Option<std::path::PathBuf>;
    let mut gh_marker: Option<gh_session::GhSession> = None;
    if scope.is_remote() {
        let runtime = gh_session::choose_runtime_dir(
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            std::env::var("TMPDIR").ok().as_deref(),
        )
        .await?;
        let member_id = scope::member_id_from_root(paths.as_ref())?;
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

    let mut ctx = Ctx::new(
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
pub(crate) fn opens_shell(cli: &Cli) -> bool {
    match &cli.command {
        Some(Command::Shell) => true,
        // `Channel` and `Reset` return from `run` before a `Ctx` exists, so
        // they never reach here — named anyway rather than swept into a
        // wildcard, so that adding a subcommand that *should* open a shell is
        // a compile error rather than a silently wrong `false`.
        Some(
            Command::Internal { .. }
            | Command::Login
            | Command::Logout
            | Command::Env
            | Command::Remote { .. }
            | Command::MoveProject { .. }
            | Command::Channel { .. }
            | Command::Reset { .. }
            | Command::Claude { .. }
            | Command::Status,
        ) => false,
        None => !cli.check && !cli.no_shell,
    }
}

/// Whether this invocation is allowed to claim (and later release) the
/// GitHub-session marker `gh_session::open`/`close` guard.
///
/// Only the invocation that goes on to hold the interactive environment
/// shell open should ever do that — see `gh_session`'s module doc. Every
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
    keep_current(cli, ctx).await?;
    remember_repo(cli, ctx).await?;
    remember_project(cli, ctx).await?;

    match &cli.command {
        Some(Command::Logout) => return logout(ctx).await,
        Some(Command::Env) => return print_env(ctx),
        Some(Command::Shell) => return open_shell(ctx).await,
        Some(Command::Login) => {
            use tasks::Task;
            ctx.connect().await?;
            tasks::login::Login.apply(ctx).await?;
            ctx.ui.info("This machine is signed in to riabuild.");
            return Ok(0);
        }
        Some(Command::Remote {
            target,
            action,
            accept_host_key,
        }) => {
            let request = dispatch::remote_request(cli, target.clone(), accept_host_key.clone());
            return dispatch::remote(ctx, action.clone(), request).await;
        }
        Some(Command::Internal {
            action: cli::InternalAction::GhSweep,
        }) => return internal::gh_sweep(ctx).await,
        Some(Command::Internal {
            action: cli::InternalAction::SeedGithub,
        }) => return internal::seed_github(ctx).await,
        Some(Command::Internal {
            action: cli::InternalAction::NgrokToken,
        }) => return internal::ngrok_token(ctx).await,
        Some(Command::MoveProject { path }) => {
            return move_project::run(ctx, path.as_deref()).await;
        }
        // Deliberately not behind `connect`: this manages local directories and
        // talks only to Claude Code, so it must work with no riabuild session,
        // no network, and a machine nothing has provisioned.
        Some(Command::Claude { action }) => {
            return dispatch::claude(ctx, action.clone()).await;
        }
        Some(Command::Reset { .. }) => unreachable!("reset returns before the tree is touched"),
        Some(Command::Channel { .. }) => {
            unreachable!("the channel returns before the setup flow starts")
        }
        Some(Command::Internal {
            action: cli::InternalAction::Askpass { .. },
        }) => unreachable!("askpass answers before a Ctx is ever built"),
        Some(Command::Status) | None => {}
    }

    provision(ctx, cli).await
}

/// Replaces this binary with the release the org publishes, before the command
/// the developer typed runs.
///
/// Every command, not just the setup flow: a developer who lives in `riabuild
/// remote` and `riabuild claude` would otherwise never run the one command
/// that updates riabuild, and go on driving servers from a build months old.
/// [`update::applies_to`] holds the four exceptions and the reasoning for
/// them.
///
/// Placed at the top of `run_inner` rather than in `run`, so that a mandatory
/// upgrade that *fails* still returns through the caller that closes a remote
/// scope's GitHub session. An upgrade that succeeds never returns at all —
/// `upgrade_and_reexec` execs — which is safe here for the reason
/// [`update::action_for`] gives: the runs that hold that session are servers,
/// and servers do not update.
///
/// The connect is soft, and that is the whole difference between this and the
/// check `provision` used to own. `riabuild claude list` is documented to work
/// with no riabuild session, no network, and a machine nothing has
/// provisioned; a laptop that cannot reach riabuild-web has no floor to be
/// below, so there is nothing to decide and nothing worth saying. The flows
/// that genuinely need the API still call `connect` themselves and still fail
/// loudly when it cannot answer.
async fn keep_current(cli: &Cli, ctx: &mut Ctx) -> Result<()> {
    if !update::applies_to(cli.command.as_ref()) {
        return Ok(());
    }
    // Swallowed on purpose, and only here: whatever riabuild-web could not
    // tell us, "you are running an old riabuild" is not something to guess at,
    // and it is never worth failing a command over.
    if ctx.connect().await.is_err() {
        return Ok(());
    }
    if let update::Action::Upgrade { to, mandatory } = update::action_for(ctx) {
        update::upgrade_and_reexec(ctx.runner.as_ref(), &ctx.ui, &to, mandatory).await?;
    }
    Ok(())
}

/// Remembers `--project`, unless the path names a directory on a *server*.
///
/// `riabuild remote --project /srv/checkout build-01` is asking for a checkout
/// at `/srv/checkout` on `build-01`: `remote::flow` forwards the string
/// unexpanded over SSH, and the server's own riabuild resolves it there.
/// Writing it into this laptop's `config.json` as well — which this used to do
/// unconditionally, before the `match` below ever dispatched `Command::Remote`
/// — pointed the next plain `riabuild` here at a directory that only exists on
/// the far side of the connection.
async fn remember_project(cli: &Cli, ctx: &mut Ctx) -> Result<()> {
    let Some(project) = &cli.project else {
        return Ok(());
    };
    if matches!(cli.command, Some(Command::Remote { .. })) {
        return Ok(());
    }
    let expanded = expand_tilde(project, &ctx.paths.home());
    let chosen = expanded.to_string_lossy().into_owned();
    // Recorded against the repository this run is about — `--repo`'s answer
    // when there is one, otherwise whatever this machine last worked on, and
    // the org default on a machine that has never chosen. A path is a fact
    // about one checkout, and there can now be several.
    match ctx.repo().ok() {
        Some(repo) => {
            let slug = repo.slug().to_string();
            ctx.update_config(|config| config.set_checkout(&slug, chosen))
                .await
        }
        // Not signed in, so there is no repository to key it by yet. The single
        // path is what an older riabuild wrote and what `project_dir` still
        // reads, and the picker migrates it as soon as there is a session.
        None => {
            ctx.update_config(|config| config.project_path = Some(chosen))
                .await
        }
    }
}

/// Remembers `--repo`, unless the repository names one on a *server*.
///
/// The same reasoning as `remember_project`: `riabuild remote --repo payments
/// build-01` is asking for `payments` on `build-01`, and `remote::flow` forwards
/// the flag over SSH for the server's own riabuild to act on. Writing it here as
/// well would switch *this* laptop to a repository the developer was talking
/// about somewhere else.
///
/// A value this laptop cannot parse fails the run rather than being dropped: it
/// was typed on this command line, and silently provisioning a different
/// repository than the one asked for is the one outcome nobody could debug.
async fn remember_repo(cli: &Cli, ctx: &mut Ctx) -> Result<()> {
    let Some(named) = &cli.repo else {
        return Ok(());
    };
    if matches!(cli.command, Some(Command::Remote { .. })) {
        return Ok(());
    }
    // The org default supplies the owner for a bare name. With no session there
    // is nothing to supply it, so `owner/repo` is required — which is the form a
    // script would use anyway.
    let owner = ctx
        .org
        .as_ref()
        .and_then(|org| org.default_repo().ok())
        .map(|default| default.owner().to_string());
    let repo = match owner {
        Some(owner) => riabuild_api::Repo::parse_with_owner(named, &owner),
        None => riabuild_api::Repo::parse(named),
    }
    .map_err(|error| {
        riabuild_ui::Failure::new(
            format!("reading --repo {named}"),
            "Give it as `owner/repo`, or a bare repository name once this machine is signed in.",
        )
        .detail(format!("{error}"))
    })?;

    tasks::repo::pick::adopt_named(ctx, repo).await
}

async fn logout(ctx: &mut Ctx) -> Result<i32> {
    ctx.keychain.delete().await?;
    ctx.update_config(|config| config.session_expires_at = None)
        .await?;
    ctx.update_state(|state| state.forget("login")).await?;
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
    use riabuild_keychain::MemoryKeychain;
    use riabuild_runner::FakeRunner;
    use tempfile::TempDir;

    /// Hands the `TempDir` back as well: dropping it deletes the tree the
    /// `Ctx`'s `Paths` point at, so a test that writes anything (`config.save`)
    /// needs it alive for the duration.
    fn ctx_for(scope: &scope::Scope) -> (Ctx, TempDir) {
        let home = TempDir::new().expect("tempdir");
        let paths: Arc<dyn Paths> = Arc::new(RealPaths::rooted_at(home.path()));
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let keychain: Arc<dyn keychain::Keychain> = Arc::new(MemoryKeychain::default());
        let ctx = Ctx::new(
            scope,
            paths,
            runner,
            keychain,
            Ui::new(true),
            UserConfig::default(),
            State::default(),
            false,
        );
        (ctx, home)
    }

    #[test]
    fn a_remote_scope_reaches_ctx_server() {
        // This is the assertion R11 exists for: a `Ctx` built from a remote
        // `Scope` must carry the server's name, not the `server: None` this
        // wiring used to hardcode. Revert `Ctx::new`'s `server:` line to
        // `None` and this fails.
        let scope = scope::Scope::read(Some("build-01"));
        let (ctx, _home) = ctx_for(&scope);
        assert_eq!(ctx.server.as_deref(), Some("build-01"));
    }

    #[test]
    fn a_laptop_scope_leaves_ctx_server_empty() {
        let scope = scope::Scope::read(None);
        let (ctx, _home) = ctx_for(&scope);
        assert_eq!(ctx.server, None);
    }

    #[tokio::test]
    async fn a_remote_projects_path_belongs_to_the_server_not_this_laptop() {
        // `riabuild remote --project /srv/checkout build-01` names a path on
        // `build-01`. `remote::flow` forwards the raw string over SSH; writing
        // it here as well pointed the next plain `riabuild` on this laptop at
        // a directory that exists only on the server. Delete the
        // `Command::Remote` guard in `remember_project` and this fails.
        let (mut ctx, _home) = ctx_for(&scope::Scope::read(None));
        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "build-01",
            "--project",
            "/srv/checkout",
        ]);
        assert_eq!(cli.project.as_deref(), Some("/srv/checkout"));

        remember_project(&cli, &mut ctx)
            .await
            .expect("nothing to remember is not an error");

        assert_eq!(
            ctx.config.project_path, None,
            "the laptop's own checkout path must be untouched"
        );
        assert!(
            !ctx.paths.config_file().exists(),
            "and config.json must not have been written at all"
        );
    }

    #[tokio::test]
    async fn a_local_project_path_is_still_expanded_and_remembered() {
        // The other direction, so the guard cannot be satisfied by never
        // saving anything.
        let (mut ctx, _home) = ctx_for(&scope::Scope::read(None));
        let cli = Cli::parse_from(["riabuild", "--project", "~/code/hub"]);
        remember_project(&cli, &mut ctx).await.expect("remembers");
        assert_eq!(
            ctx.config.project_path,
            Some(
                ctx.paths
                    .home()
                    .join("code/hub")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert!(ctx.paths.config_file().exists());
    }

    #[tokio::test]
    async fn a_named_repository_is_remembered_for_the_next_run_too() {
        let (mut ctx, _home) = ctx_for(&scope::Scope::read(None));
        let cli = Cli::parse_from(["riabuild", "--repo", "Clubria/payments"]);

        remember_repo(&cli, &mut ctx).await.expect("remembers");

        assert_eq!(
            ctx.repo.as_ref().map(|repo| repo.slug()),
            Some("Clubria/payments"),
            "every repository-scoped task reads this, not the flag"
        );
        assert_eq!(
            ctx.config.active_repo.as_deref(),
            Some("Clubria/payments"),
            "and the next run's default is what this run worked on"
        );
    }

    #[tokio::test]
    async fn a_named_repository_on_a_remote_run_is_not_recorded_here() {
        // `riabuild remote --repo payments build-01` is about `build-01`:
        // `flow/connect.rs` forwards the flag, and the server's own riabuild
        // acts on it. Recording it here would switch this laptop to a
        // repository the developer was talking about somewhere else — the same
        // bug `remember_project`'s guard exists for.
        let (mut ctx, _home) = ctx_for(&scope::Scope::read(None));
        let cli = Cli::parse_from([
            "riabuild",
            "--repo",
            "Clubria/payments",
            "remote",
            "build-01",
        ]);

        remember_repo(&cli, &mut ctx)
            .await
            .expect("nothing to remember here is not an error");

        assert_eq!(ctx.config.active_repo, None);
        assert!(ctx.repo.is_none());
        assert!(
            !ctx.paths.config_file().exists(),
            "and config.json must not have been written at all"
        );
    }

    #[tokio::test]
    async fn a_repository_riabuild_cannot_use_fails_the_run_rather_than_being_dropped() {
        // Silently provisioning a different repository than the one named on the
        // command line is the outcome nobody could debug.
        let (mut ctx, _home) = ctx_for(&scope::Scope::read(None));
        let cli = Cli::parse_from(["riabuild", "--repo", "Clubria/.."]);

        let error = remember_repo(&cli, &mut ctx)
            .await
            .expect_err("must not be accepted");
        assert!(format!("{error:#}").contains("--repo"), "{error:#}");
    }

    #[tokio::test]
    async fn a_bare_name_with_no_session_says_what_form_to_use() {
        // The org default supplies the owner for a bare name, and there is no
        // org default until this machine has signed in.
        let (mut ctx, _home) = ctx_for(&scope::Scope::read(None));
        let cli = Cli::parse_from(["riabuild", "--repo", "payments"]);

        let error = remember_repo(&cli, &mut ctx)
            .await
            .expect_err("no owner to complete it with");
        assert!(format!("{error:#}").contains("owner/repo"), "{error:#}");
    }

    /// A `Cli` with every field but `command`/`no_shell`/`check` at its
    /// ordinary default, for the marker-predicate tests below — those three
    /// are the only fields `opens_shell` reads.
    fn cli_for(command: Option<Command>, no_shell: bool, check: bool) -> Cli {
        Cli {
            command,
            project: None,
            repo: None,
            check,
            quiet: false,
            no_shell,
        }
    }

    #[test]
    fn internal_plumbing_never_claims_the_gh_session_marker() {
        // This is the fix for the bug described in `gh_session`'s module
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

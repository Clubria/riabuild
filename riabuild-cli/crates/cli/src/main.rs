//! riabuild — from "accepted a GitHub org invite" to "running Claude Code
//! against the Clubria codebase with working secrets", without the developer
//! making a single environment decision.
//!
//! This file is the wiring only: parse argv, assemble the `Ctx` a run works
//! against — including the GitHub-session envelope a remote scope executes
//! inside — and dispatch to whichever module implements the command. The
//! default flow lives in `provision.rs`, the hidden `internal` subcommands in
//! `internal.rs`, and `riabuild remote` in `remote/`. Only `login`, `logout`,
//! `env` and `connect` are small enough to have stayed here. `startup.rs`
//! assembles the GitHub-session envelope a remote scope executes inside, and
//! `preamble.rs` holds the three things every run does before dispatch.

// The panic lints are denied for the shipped binary in `Cargo.toml`. In tests a
// panic *is* the reporting mechanism for a failed precondition, so unwrapping a
// fixture there is correct and this exemption keeps the deny from forcing
// ceremony into every `#[cfg(test)]` module. The lints still apply to the
// binary target, which is the build that reaches a developer's laptop.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

mod cli;
mod dispatch;
mod fs_move;
mod internal;
mod lock;
mod move_project;
mod preamble;
mod provision;
mod reset;
mod startup;
mod update;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};
use preamble::{keep_current, remember_project, remember_repo};
use provision::{open_shell, provision};
use startup::opens_shell;
use std::sync::Arc;

// The library crates, under the names this file has always called them by.
// `riabuild-cli` is wiring: it names every crate and implements none of them.
use riabuild_keychain as keychain;
use riabuild_tasks as tasks;
use riabuild_tasks::scope;
use riabuild_tasks::shell;

use riabuild_api::ApiError;
use riabuild_paths::config::{State, UserConfig};
use riabuild_paths::{Paths, RealPaths};
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

    // Dispatched here for the sharpest version of the same reason again: these
    // two are the server's half of a mosh session, and their stdout is a
    // *protocol* — one line, and after it, for `mosh-tcp2udp`, the session's
    // own framed datagrams. A banner on that stream is not an untidy line, it
    // is a corrupted session. Neither reads the tree, so neither needs a `Ctx`.
    //
    // And `launch` for the first reason again, at the volume it is reached:
    // it is what `~/.riabuild/bin/claude` execs, so it runs every time a
    // developer types `claude`. The shell script it replaces checked the
    // machine, read nothing, and printed nothing, and this must be the same —
    // no banner, no `state.json`, no API, and no self-update check, which is
    // why `update::applies_to` already excepts every `internal` subcommand.
    if let Some(Command::Internal { action }) = &cli.command {
        match action {
            cli::InternalAction::UdpEcho => return riabuild_remote::mosh::udp_echo().await,
            cli::InternalAction::MoshTcp2Udp { port } => {
                return riabuild_remote::mosh::tcp2udp(*port).await;
            }
            cli::InternalAction::Launch { .. } => {
                let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
                return internal::launch(runner.as_ref(), action).await;
            }
            // And `completions` for the same two reasons at once: its stdout
            // is a script a shell sources, and it runs inside Homebrew's build
            // sandbox and a `dpkg-deb` staging tree, where there is no
            // `~/.riabuild` for a `Ctx` to read.
            cli::InternalAction::Completions { shell } => {
                return internal::completions(*shell);
            }
            _ => {}
        }
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

    // The envelope a remote-scoped run executes inside, and the one claim only
    // the shell run takes — see `startup`.
    let gh = startup::open_gh_session(&scope, &cli, paths.as_ref()).await?;

    // Bound before the shadowing binding below, which moves `runner`.
    // `base_runner` is the unwrapped `RealRunner`: `kill -0` (run by `close`,
    // via `sweep`) needs no namespace environment, and closing has to work
    // even once the scoped runner built below is gone.
    let base_runner = runner.clone();
    let runner = startup::scoped_runner(runner, gh.dir.as_deref(), paths.as_ref());

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

    if let Some(session) = gh.marker
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
        // Beside `claude` rather than behind `connect`, and for the same
        // reason: it reads local directories and talks only to Claude Code, so
        // it must work with no riabuild session and no network.
        Some(Command::Paths) => return dispatch::paths(ctx).await,
        Some(Command::Shell) => return open_shell(ctx).await,
        Some(Command::Login) => return login(ctx).await,
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
        // Needs a `Ctx` — it reads the keychain and posts to `/api/v1` — so it
        // is dispatched here rather than beside `launch`. It is still an
        // `internal` subcommand, which is what keeps `update::applies_to` from
        // turning a flush every minute into a version check every minute.
        Some(Command::Internal {
            action: cli::InternalAction::UsageFlush,
        }) => return internal::usage::flush(ctx).await,
        // Behind `connect`, but softly and inside itself: an invocation that
        // needs no credential — `infisical --version`, `infisical scan` — must
        // not wait on the network first, and one that cannot get a credential
        // still hands the developer their command rather than a riabuild error.
        Some(Command::Internal {
            action: cli::InternalAction::Infisical { args },
        }) => return internal::infisical::run(ctx, args).await,
        // Behind a `Ctx` because it brokers a credential, unlike `launch`: the
        // token comes from riabuild-web, so this one does need the API client
        // and the session. Everything it prints goes to stderr — the process
        // becomes ngrok, and ngrok's stdout is the developer's.
        Some(Command::Internal {
            action: cli::InternalAction::Ngrok { binary, args },
        }) => return internal::ngrok(ctx, binary.clone(), args.clone()).await,
        // Not behind `connect`: a turn is the harness riabuild already installed
        // working in the checkout riabuild already cloned, and it has to keep
        // running on a laptop that has gone offline since the window opened.
        Some(Command::Internal {
            action:
                cli::InternalAction::AgentTurn {
                    session,
                    prompt_file,
                },
        }) => return internal::agent_turn(ctx, session, prompt_file).await,
        // Not behind `connect` either, and for a second reason on top of the
        // one above: this process's stdout is a JSON-RPC pipe Claude Code is
        // parsing, and `connect` prints.
        Some(Command::Internal {
            action: cli::InternalAction::McpCodex { profile },
        }) => return internal::mcp_codex(ctx, *profile).await,
        Some(Command::MoveProject { path }) => {
            return move_project::run(ctx, path.as_deref()).await;
        }
        // Deliberately not behind `connect`: this manages local directories and
        // talks only to Claude Code, so it must work with no riabuild session,
        // no network, and a machine nothing has provisioned.
        Some(Command::Claude { action }) => {
            return dispatch::claude(ctx, action.clone()).await;
        }
        // Not behind `connect` either, and for the same reason: the harnesses
        // and the checkout are already on this machine.
        Some(Command::Agents { prompt }) => {
            return dispatch::agents(ctx, prompt.clone()).await;
        }
        Some(Command::Reset { .. }) => unreachable!("reset returns before the tree is touched"),
        Some(Command::Channel { .. }) => {
            unreachable!("the channel returns before the setup flow starts")
        }
        Some(Command::Internal {
            action:
                cli::InternalAction::Askpass { .. }
                | cli::InternalAction::UdpEcho
                | cli::InternalAction::MoshTcp2Udp { .. }
                | cli::InternalAction::Launch { .. }
                | cli::InternalAction::Completions { .. },
        }) => unreachable!("the stdout-is-a-payload subcommands answer before a Ctx is built"),
        Some(Command::Status) | None => {}
    }

    provision(ctx, cli).await
}

/// `riabuild login` — the device-code flow, and the token it stores.
///
/// The `--check` guard is first, ahead of the connect: `--check` is documented
/// as changing nothing, and the sign-in flow opens a browser, waits for a
/// human to approve a session, and writes the token it is given. There is no
/// smaller part of that worth doing under a flag that promises to do none of
/// it.
async fn login(ctx: &mut Ctx) -> Result<i32> {
    use tasks::Task;
    if ctx.dry_run {
        ctx.ui.info(
            "--check: this would sign this machine in to riabuild, which opens a browser and \
             stores the session it approves. Nothing was changed.",
        );
        return Ok(0);
    }
    ctx.connect().await?;
    tasks::login::Login.apply(ctx).await?;
    ctx.ui.info("This machine is signed in to riabuild.");
    Ok(0)
}

/// `riabuild logout` — forget this machine's session.
///
/// Under `--check` it says so and stops. Deleting the keychain entry, clearing
/// the recorded expiry and dropping the `login` task's state are three
/// separate writes, and a flag documented as changing nothing performed all
/// three.
async fn logout(ctx: &mut Ctx) -> Result<i32> {
    if ctx.dry_run {
        ctx.ui.info(&format!(
            "--check: this would sign this machine out by deleting its riabuild session from {}. \
             Nothing was changed.",
            ctx.keychain.describe()
        ));
        return Ok(0);
    }
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
        println!("export {key}={}", shell::shell_quote(&value));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_keychain::MemoryKeychain;
    use riabuild_runner::FakeRunner;
    use riabuild_tasks::testing::{ctx_and_runner, org_config};
    use tempfile::TempDir;

    /// A laptop below its team's floor, whose connect cannot complete.
    ///
    /// The unreadable keychain is how `connect` is made to fail without a
    /// network: it is the first thing `connect` touches. What it stands in for
    /// is the real case — `/me` answering 409 `cli_too_old` — which leaves
    /// `ctx.org` set and returns an error in exactly the same shape.
    async fn locked_out() -> (Ctx, TempDir) {
        let (mut ctx, home, _runner) = ctx_and_runner(FakeRunner::new()).await;
        ctx.cli_version = "2026.07.30".into();
        let mut org = org_config();
        org.min_cli_version = "2026.08.04".into();
        org.latest_cli_version = "2026.08.12".into();
        ctx.org = Some(org);
        ctx.member = None;
        ctx.keychain = Arc::new(MemoryKeychain::unreadable());
        (ctx, home)
    }
    #[tokio::test]
    async fn a_riabuild_below_the_floor_still_reaches_its_own_upgrade() {
        // Raising `minCliVersion` used to lock every older riabuild out of the
        // one thing that could clear it. `/me` enforces the floor, so
        // `Ctx::connect` failed; `keep_current` returned on that failure; and
        // `update::action_for` — the only code that can decide to upgrade —
        // was never reached. Restore the `if ctx.connect().await.is_err() {
        // return }` early return in `version_action` and this fails.
        let (mut ctx, _home) = locked_out().await;
        let cli = Cli::parse_from(["riabuild"]);

        assert_eq!(
            preamble::version_action(&cli, &mut ctx).await,
            update::Action::Upgrade {
                to: "2026.08.12".into(),
                mandatory: true
            }
        );
    }
    #[tokio::test]
    async fn a_laptop_that_learned_nothing_at_all_is_still_left_alone() {
        // The other direction, so the fix above cannot be "always upgrade".
        // A laptop that could not reach riabuild-web has no floor to be below,
        // and `riabuild claude` is documented to work with no session, no
        // network and a machine nothing has provisioned.
        let (mut ctx, _home) = locked_out().await;
        ctx.org = None;
        let cli = Cli::parse_from(["riabuild"]);

        assert_eq!(
            preamble::version_action(&cli, &mut ctx).await,
            update::Action::Continue
        );
    }
    #[tokio::test]
    async fn check_signs_this_machine_out_of_nothing() {
        // `riabuild --check logout` deleted the keychain entry, cleared the
        // recorded expiry and dropped the `login` task's state — three writes
        // under a flag documented as changing nothing.
        let (mut ctx, _home, _runner) = ctx_and_runner(FakeRunner::new()).await;
        ctx.dry_run = true;

        let code = logout(&mut ctx).await.expect("a dry run succeeds");

        assert_eq!(code, 0);
        assert_eq!(
            ctx.keychain.get().await.unwrap().as_deref(),
            Some("rb_test_token"),
            "the session must survive --check"
        );
        assert!(!ctx.paths.config_file().exists(), "and so must config.json");
        assert!(!ctx.paths.state_file().exists(), "and state.json");
    }
    #[tokio::test]
    async fn check_signs_this_machine_in_to_nothing() {
        // `riabuild --check login` ran the whole device-code flow: a browser,
        // a human approving a session, and a token stored at the end of it.
        let (mut ctx, _home, runner) = ctx_and_runner(FakeRunner::new()).await;
        ctx.keychain = Arc::new(MemoryKeychain::default());
        ctx.dry_run = true;

        let code = login(&mut ctx).await.expect("a dry run succeeds");

        assert_eq!(code, 0);
        assert_eq!(
            ctx.keychain.get().await.unwrap(),
            None,
            "--check must not store a session"
        );
        assert!(
            runner.calls().is_empty(),
            "and must open no browser to obtain one"
        );
        assert!(!ctx.paths.config_file().exists());
    }
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
}

//! `riabuild internal ...` — the hidden subcommands riabuild invokes on itself
//! over SSH. Not for people.
//!
//! Two of them concern the per-session GitHub credential on a server:
//! `gh-sweep` clears what a session that died without cleaning up left behind,
//! and `seed-github` takes the token the laptop pipes over and hands it to
//! `gh`. The marker mechanics they sit on are in `gh_session`; the laptop side
//! that invokes them is in `remote/`.
//!
//! Two more are run by generated shims on every invocation of the tool they
//! stand in front of, so that a credential riabuild-web holds reaches that tool
//! and no filesystem: `ngrok-token` prints the team's authtoken for
//! `~/.riabuild/bin/ngrok`, and `infisical` *is* `~/.riabuild/bin/infisical` —
//! it brokers an Infisical credential for one command and starts infisical with
//! it. Both must keep their stdout clean, and for the same reason: one is read
//! as the value, and the other is the developer's `infisical export > .env`.

pub(crate) mod infisical;

use anyhow::Result;
use riabuild_gh_session as gh_session;
use riabuild_paths::config;
use riabuild_runner::RunOptions;
use riabuild_tasks::Ctx;
use riabuild_tasks::scope;

pub(crate) async fn gh_sweep(ctx: &Ctx) -> Result<i32> {
    // Run by the laptop before seeding, so a dead session's leftovers
    // go before the new credential arrives rather than after.
    let runtime = gh_session::choose_runtime_dir(
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("TMPDIR").ok().as_deref(),
    )
    .await?;
    let dir =
        gh_session::GhSession::attach(&runtime, &scope::member_id_from_root(ctx.paths.as_ref())?)
            .await?;
    gh_session::sweep(&dir, ctx.runner.clone(), config::now_secs()).await?;
    Ok(0)
}

pub(crate) async fn seed_github(ctx: &mut Ctx) -> Result<i32> {
    // `tokio::io`, not `std::io`: a blocking read on the current-thread
    // runtime stalls every other future on it, which is the invariant in
    // riabuild-cli/CLAUDE.md.
    use tokio::io::AsyncReadExt;
    let mut token = String::new();
    tokio::io::stdin().read_to_string(&mut token).await?;
    accept_github_token(ctx, &token).await
}

/// The server half of `remote::seed::seed_github`: hands the GitHub token the
/// laptop piped over SSH on to `gh`, again on stdin.
///
/// The token reaches `gh` only on stdin — never in argv, because `ps` is
/// world-readable and on a shared server it shows every other developer's
/// command lines — and is never logged. `gh` writes its own `hosts.yml`, with
/// its own permissions, into the `GH_CONFIG_DIR` the scoped runner supplies;
/// riabuild never hand-writes that file.
///
/// Taking the token as an argument rather than reading stdin itself is what
/// makes that guarantee assertable: the caller above reads the *process's*
/// stdin, which under `cargo test` is the terminal, so a test driving the
/// subcommand end to end would block on EOF instead of asserting anything.
///
/// `ctx.gh()` rather than the string `"gh"`: during provisioning
/// `~/.riabuild/bin` is not on `PATH`, so a bare name would find whatever the
/// server happens to have — or, far more likely on a freshly provisioned
/// server, nothing at all.
///
/// That second case used to lose the lend entirely, and it was the common one.
/// On a *first* `riabuild remote` against a server the laptop seeds before it
/// starts the setup pass — see `remote::flow::connect_and_setup` — and the
/// setup pass is what installs `gh`. So the seed ran against a path that did
/// not exist yet, failed, and the laptop printed "it will sign in itself":
/// the developer approved a GitHub device code for a credential the laptop was
/// holding all along, on exactly the run where lending it was worth most.
/// Installing `gh` here rather than depending on a later pass is what makes the
/// seed self-sufficient; the setup pass then finds `gh` present, signed in and
/// a member, and `github_cli::check` reports it satisfied without applying
/// anything.
async fn accept_github_token(ctx: &mut Ctx, token: &str) -> Result<i32> {
    if !tokio::fs::try_exists(&ctx.gh()).await.unwrap_or(false) {
        riabuild_tasks::github_cli::install(ctx).await?;
    }
    let output = ctx
        .runner
        .run(
            &ctx.gh(),
            &["auth", "login", "--with-token"],
            &RunOptions {
                stdin: Some(token.trim().as_bytes().to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(if output.ok() { 0 } else { 1 })
}

/// Prints the team's ngrok authtoken, and nothing else.
///
/// Run by `~/.riabuild/bin/ngrok` on every invocation, inside a command
/// substitution. **The token is the only thing on stdout**, for the same reason
/// it is in `askpass` below: the caller reads this process's stdout as the
/// value. Anything riabuild has to say goes to stderr, where the developer sees
/// it and the shim does not capture it.
///
/// A failure here is deliberately not fatal to `ngrok`. The shim leaves the
/// variable unset and runs it anyway, so `ngrok --version` and `ngrok help`
/// work on a machine with no network, and the developer meets the explanation
/// rather than an empty `NGROK_AUTHTOKEN` that reads as "not authenticated".
pub(crate) async fn ngrok_token(ctx: &mut Ctx) -> Result<i32> {
    println!("{}", fetch_ngrok_token(ctx).await?);
    Ok(0)
}

/// `riabuild internal launch <harness>` — one Claude Code, Codex or Grok Build
/// launch.
///
/// What every launcher in `~/.riabuild/bin` execs, and the whole reason none of
/// them is a shell script any more. The flags carry what riabuild resolved when
/// that launcher was written; `shims::launch` makes the decisions and then
/// `exec`s the harness, so this function does not return on a successful launch.
///
/// Takes no `Ctx` on purpose. It runs every time a developer types `claude`,
/// and the shell script it replaces read no config, opened no socket and
/// printed nothing — so building a `Ctx` here would put `config.json`,
/// `state.json` and a keychain probe in front of every session.
pub(crate) async fn launch(
    runner: &dyn riabuild_runner::CommandRunner,
    action: &crate::cli::InternalAction,
) -> Result<i32> {
    use riabuild_tasks::shims::Plan;
    use std::path::PathBuf;

    let crate::cli::InternalAction::Launch {
        harness,
        home,
        binary,
        bin_dir,
        settings,
        checkouts,
        default_checkout,
        args,
    } = action
    else {
        unreachable!("launch is dispatched on its own variant");
    };

    let plan = Plan {
        settings: settings.as_deref().map(PathBuf::from),
        checkouts: checkouts.iter().map(PathBuf::from).collect(),
        default_checkout: default_checkout.as_deref().map(PathBuf::from),
        args: args.clone(),
        ..Plan::new(
            (*harness).into(),
            PathBuf::from(home),
            binary.clone(),
            PathBuf::from(bin_dir),
        )
    };
    riabuild_tasks::shims::launch::run(runner, &plan).await
}

/// `riabuild internal ngrok` — ngrok, with the team's authtoken in its
/// environment and nowhere else.
///
/// The shim used to do this in shell, around `internal ngrok-token`'s stdout:
/// a command substitution into a variable, an `if -n` to decide between
/// exporting and unsetting it, and then an `exec`. Doing it here means the
/// credential is fetched by the process that goes on to *become* ngrok, so it
/// is in no argument list, on no pipe, and in no shell variable — and "print
/// nothing else on stdout" stops being a rule some other subcommand has to keep
/// on this one's behalf.
///
/// **A fetch that fails is not fatal.** ngrok is started with the variable
/// *absent* rather than empty, because an empty `NGROK_AUTHTOKEN` reads to
/// ngrok as "not authenticated" and would override a token the developer had
/// configured for themselves. So `ngrok --version` and `ngrok help` still work
/// on a plane, signed out, or before a lead has set one — with riabuild's own
/// explanation on stderr above whatever ngrok says next.
pub(crate) async fn ngrok(ctx: &mut Ctx, binary: String, args: Vec<String>) -> Result<i32> {
    let token = match fetch_ngrok_token(ctx).await {
        Ok(token) => Some(token),
        Err(error) => {
            // stderr, not stdout: this process is about to become ngrok, and
            // ngrok's stdout belongs to the developer's own pipeline.
            eprintln!("{error:#}");
            None
        }
    };

    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    ctx.runner
        .exec_replacing(
            &binary,
            &borrowed,
            &RunOptions {
                env: token
                    .map(|token| vec![("NGROK_AUTHTOKEN".to_string(), token)])
                    .unwrap_or_default(),
                ..Default::default()
            },
        )
        .await
}

/// The team's ngrok authtoken, or why it could not be had.
///
/// Shared with [`ngrok_token`], which is the same fetch with the answer going
/// to stdout instead of into an environment.
async fn fetch_ngrok_token(ctx: &mut Ctx) -> Result<String> {
    ctx.connect().await?;
    if ctx.org.is_none() {
        return Err(not_signed_in().into());
    }
    Ok(riabuild_api::ngrok::fetch_authtoken(&ctx.api)
        .await
        .map_err(explain)?
        .token)
}

/// `riabuild internal agent-turn` — one turn of one agent session.
///
/// Started detached by `riabuild agents` and by nothing else. Deliberately not
/// behind `connect`: the harness is already installed, the checkout is already
/// cloned, and a turn has to keep running on a laptop that lost its network
/// after the window opened.
///
/// The binary is resolved here rather than recorded on the session, because a
/// versioned path moves with every riabuild upgrade — a session started last
/// week must run this week's Claude Code, not a directory that no longer exists.
/// The *profile* is the opposite and is recorded: it is what resume depends on.
pub(crate) async fn agent_turn(ctx: &mut Ctx, session: &str, prompt_file: &str) -> Result<i32> {
    let store = riabuild_agents::store::Store::new(ctx.paths.as_ref());
    let record = store.read(session).await?;
    let Some(kind) = record.harness() else {
        anyhow::bail!("session {session} names a harness this riabuild does not know");
    };
    let program = match kind {
        riabuild_harness::Kind::Claude => ctx.claude(),
        riabuild_harness::Kind::Codex => ctx.codex(),
        riabuild_harness::Kind::Grok => ctx.grok(),
    };
    riabuild_agents::turn::run(
        ctx.runner.as_ref(),
        &store,
        session,
        &program,
        std::path::Path::new(prompt_file),
    )
    .await
}

fn not_signed_in() -> riabuild_ui::Failure {
    riabuild_ui::Failure::new(
        "fetching the team's ngrok authtoken",
        "Run `riabuild` to sign this machine in, then try again.",
    )
}

/// Turns what the server said into what the developer should do about it.
///
/// Only the cases the server cannot phrase for itself are reworded. A
/// `not_configured` is the one worth naming here: nothing is broken, and the
/// person who can fix it is not the person reading the message.
fn explain(error: anyhow::Error) -> anyhow::Error {
    let Some(api_error) = error.downcast_ref::<riabuild_api::ApiError>() else {
        return error;
    };
    if api_error.code == "not_configured" {
        return riabuild_ui::Failure::new(
            "fetching the team's ngrok authtoken",
            "Ask your team lead to set one in the riabuild dashboard, under org settings.              ngrok will run unauthenticated until they do.",
        )
        .into();
    }
    error
}

/// Answers the password prompt `ssh` is holding open, and remembers the
/// answer.
///
/// Run by `ssh` itself through `SSH_ASKPASS`, not by a person and not over
/// SSH like the two above — so it is dispatched in `main::run` before a `Ctx`
/// exists, alongside `channel` and `reset`. It must not check the machine,
/// read config, or talk to the API: it runs inside an authentication attempt,
/// several times per `riabuild remote`, and anything slow here is a pause
/// before every connection.
///
/// **The answer is the only thing on stdout.** `ssh` reads the first line of
/// this process's stdout as the password, so the prompt goes to `/dev/tty`
/// (see `ui::secret`) and every diagnostic goes to stderr.
pub(crate) async fn askpass(
    paths: &dyn riabuild_paths::Paths,
    runner: std::sync::Arc<dyn riabuild_runner::CommandRunner>,
    prompt: &str,
) -> Result<i32> {
    use riabuild_remote::askpass::{ACCOUNT_VAR, answer, store};

    let account = std::env::var(ACCOUNT_VAR).unwrap_or_default();
    let store = store(runner, paths, &account).await?;
    let answer = answer(store.as_ref(), prompt, riabuild_ui::secret::ask_secret).await?;

    if let Some(why) = answer.not_saved {
        eprintln!("riabuild could not save that password ({why}); it will ask again.");
    }
    println!("{}", answer.secret);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_error(code: &str, message: &str) -> anyhow::Error {
        riabuild_api::ApiError {
            status: 404,
            code: code.into(),
            message: message.into(),
            action: "Do the thing.".into(),
        }
        .into()
    }

    #[test]
    fn a_team_with_no_ngrok_token_is_told_who_can_set_one() {
        // The server's own 404 is accurate and useless to the person reading
        // it: nothing on this machine is broken, and the fix belongs to
        // somebody else.
        let explained = explain(api_error("not_configured", "No ngrok authtoken is set."));
        let rendered = format!("{explained}");
        assert!(rendered.contains("team lead"), "{rendered}");
        assert!(rendered.contains("dashboard"), "{rendered}");
    }

    #[test]
    fn an_error_the_server_already_phrased_is_passed_through_untouched() {
        // The server knows why it refused; the CLI does not. Rewording a 403
        // here would replace an accurate sentence with a guess.
        let explained = explain(api_error(
            "not_org_member",
            "You are no longer in the Clubria GitHub organisation.",
        ));
        assert!(
            format!("{explained}").contains("no longer in the Clubria GitHub organisation"),
            "{explained}"
        );
    }
    use riabuild_keychain::{self as keychain, MemoryKeychain};
    use riabuild_paths::config::{State, UserConfig};
    use riabuild_paths::{Paths, RealPaths};
    use riabuild_runner::{CommandRunner, FakeRunner};
    use riabuild_ui::Ui;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// The absolute `gh` a `Ctx` rooted at `home` will run.
    ///
    /// Computed here rather than hard-coded because `FakeRunner` matches on an
    /// invocation *prefix*: the stub has to carry the same absolute path
    /// `ctx.gh()` produces, or the call falls through to the default and the
    /// test asserts nothing about the command it meant to pin.
    fn gh_path(home: &std::path::Path) -> String {
        RealPaths::rooted_at(home)
            .tool_dir("gh", riabuild_fetch::tools::GH_VERSION)
            .join(riabuild_fetch::tools::GH_MEMBER)
            .to_string_lossy()
            .into_owned()
    }

    /// Takes the `TempDir` rather than making one: the caller needs the home
    /// directory *before* the `Ctx` exists, to build a `FakeRunner` stub around
    /// `gh_path`. Dropping it deletes the tree the `Ctx`'s `Paths` point at, so
    /// the caller keeps it alive for the duration, along with its own handle on
    /// the `FakeRunner` — what these tests assert on is *what was run*, not the
    /// result.
    /// Puts a file where `ctx.gh()` looks, so `accept_github_token` takes its
    /// "already installed" branch instead of reaching for the network.
    ///
    /// The other branch — a server where `gh` is genuinely absent, which is the
    /// case the install exists for — cannot be unit-tested here: it downloads a
    /// real release through `tools::install`, which has no seam to fake.
    async fn pretend_gh_is_installed(home: &TempDir) {
        let gh = std::path::PathBuf::from(gh_path(home.path()));
        tokio::fs::create_dir_all(gh.parent().expect("gh has a parent"))
            .await
            .expect("tool dir");
        tokio::fs::write(&gh, b"#!/bin/sh\n").await.expect("gh");
    }

    fn ctx_with_runner(home: &TempDir, scope: &scope::Scope, fake: Arc<FakeRunner>) -> Ctx {
        let paths: Arc<dyn Paths> = Arc::new(RealPaths::rooted_at(home.path()));
        let runner: Arc<dyn CommandRunner> = fake;
        let keychain: Arc<dyn keychain::Keychain> = Arc::new(MemoryKeychain::default());
        Ctx::new(
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

    #[tokio::test]
    async fn the_github_token_reaches_gh_on_stdin_and_never_in_argv() {
        // On a shared server `ps` shows every developer's command lines, so a
        // token in argv is a token handed to everyone logged in. Both halves
        // are asserted deliberately: dropping `stdin:` from the call site
        // leaves argv clean, so an argv-only test stays green while `gh` is
        // handed an empty pipe; passing the token as an extra argument as well
        // would leave stdin correct, so a stdin-only test stays green while
        // `ps` leaks it.
        let home = TempDir::new().expect("tempdir");
        let gh = gh_path(home.path());
        let fake =
            Arc::new(FakeRunner::new().with(&format!("{gh} auth login --with-token"), 0, "", ""));
        pretend_gh_is_installed(&home).await;
        let mut ctx = ctx_with_runner(&home, &scope::Scope::read(Some("build-01")), fake.clone());

        let token = "gho_averysecretgithubtoken";
        assert_eq!(
            accept_github_token(&mut ctx, &format!("{token}\n"))
                .await
                .expect("gh runs"),
            0
        );

        assert_eq!(
            fake.stdin_text_of(&format!("{gh} auth login")).as_deref(),
            Some(token),
            "the token must arrive on stdin, trailing newline trimmed"
        );
        for call in fake.calls() {
            assert!(
                !call.contains(token),
                "the token must not appear in any argument list: {call}"
            );
        }
        // The absolute path, not the bare name: `~/.riabuild/bin` is not on
        // `PATH` during provisioning, so a bare `gh` would find the server's
        // own — or, on a server riabuild has just built, nothing at all.
        assert_eq!(fake.calls(), vec![format!("{gh} auth login --with-token")]);
    }

    #[tokio::test]
    async fn a_gh_that_rejects_the_token_is_a_nonzero_exit() {
        // The failure has to travel back over SSH as an exit code — a seeding
        // run that reported success while `gh` refused the token would leave
        // the shell hop to discover it, with no credential and no explanation.
        let home = TempDir::new().expect("tempdir");
        let gh = gh_path(home.path());
        let fake = Arc::new(FakeRunner::new().with(
            &format!("{gh} auth login --with-token"),
            1,
            "",
            "bad token",
        ));
        pretend_gh_is_installed(&home).await;
        let mut ctx = ctx_with_runner(&home, &scope::Scope::read(Some("build-01")), fake);
        assert_eq!(
            accept_github_token(&mut ctx, "gho_expired")
                .await
                .expect("gh runs"),
            1
        );
    }
}

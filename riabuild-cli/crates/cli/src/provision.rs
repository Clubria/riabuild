//! The default flow: `riabuild` with no subcommand.
//!
//! Connect, say whose machine this is, run every task, write the shims, log
//! the run, and hand the developer their shell. `main.rs` decides *which* flow
//! an invocation is; this is the one that provisions — and it is also where
//! riabuild's own upgrade used to live, before `main::keep_current` widened it
//! to every command.

mod report;

use report::{describe_repo, describe_session, dry_run_summary, log_run};

use crate::cli::Cli;
use crate::lock::provisioning_lock;
use crate::{opens_shell, tasks};
use anyhow::Result;
use riabuild_tasks::shell;
use riabuild_tasks::shims;
use riabuild_tasks::{Ctx, engine};

pub(crate) async fn provision(ctx: &mut Ctx, cli: &Cli) -> Result<i32> {
    ctx.ui.banner("Clubria");
    ctx.connect().await?;
    describe_session(ctx);
    ask_which_repository(ctx, cli).await?;
    describe_repo(ctx);
    load_secret_scope(ctx).await;

    // The update check that used to stand here now runs for *every* command,
    // from `main::keep_current`, before this function is reached. It has not
    // been dropped — it has been widened, and `update::action_for` carries the
    // "a managed server never replaces its own binary" guard that used to be
    // spelled here.
    //
    // Acquired after that check, because a `flock` survives `exec` and
    // `upgrade_and_reexec` replaces this process image: taking it first would
    // carry the lock into the new process with no guard tracking it and
    // nothing left to release it.
    let provisioning = provisioning_lock(ctx.paths.as_ref(), &ctx.ui, ctx.dry_run).await?;

    ctx.ui.heading("Checking this machine");
    let registry = tasks::registry();
    let limits = match cli.jobs {
        Some(jobs) => engine::Limits { jobs },
        None => engine::Limits::default(),
    };
    let (outcome, ran) = engine::run_all_with_outcome(&registry, ctx, limits).await;
    let finished = after_the_tasks(ctx, &outcome, ran).await;

    // Released here, before every return below it and before `open_shell` above
    // all: that call awaits the developer's interactive shell for as long as
    // their window stays open, and a lock held across it would make the second
    // window wait on a human rather than on a download.
    drop(provisioning);

    if ctx.dry_run {
        ctx.ui.blank();
        ctx.ui.info(&dry_run_summary(&outcome));
        ctx.ui.blank();
    }

    // After the summary, so a `--check` that could not check everything still
    // says what it *did* find before it reports why it stopped short.
    finished?;

    if ctx.dry_run {
        return Ok(0);
    }

    if !opens_shell(cli) {
        // A run that ends here rather than in a shell is, on a server, the
        // remote end of `ssh -t … riabuild --no-shell`: the moment it returns,
        // the laptop's `ssh` prints `Connection to … closed.` under whatever
        // the last task said. Nothing on the laptop can put a line above that
        // message — this is the only side that runs before it.
        ctx.ui.blank();
        return Ok(0);
    }
    open_shell(ctx).await
}

/// Everything a run does once the tasks have all had their turn — **whatever**
/// they did.
///
/// The engine carries on past a failed task: a developer who walked away from
/// the Claude sign-in still gets a Codex, a checkout and a toolchain. This is
/// the other half of that. `provision` used to write `engine::run_all(…)?`, so
/// one failure short-circuited the launchers, the "Worth knowing" notes and the
/// log line — the machine was left half provisioned *and* without the `claude`
/// launcher for the accounts that were set up perfectly well, which is most of
/// what carrying on was for.
///
/// Split out with the engine's two answers as parameters because that is what
/// makes the ordering testable: a test can hand it a failing run and assert
/// that the machine was still landed on the way out.
///
/// The run's own failure is the one returned. It carries the remedy the
/// developer can act on, and a launcher that could not be written on a machine
/// whose toolchain never installed is that failure's symptom rather than a
/// second fault to report beside it.
async fn after_the_tasks(ctx: &mut Ctx, outcome: &engine::Outcome, ran: Result<()>) -> Result<()> {
    let landed = write_launchers(ctx).await;

    let notes = std::mem::take(&mut ctx.notes);
    if !notes.is_empty() {
        ctx.ui.heading("Worth knowing");
        for note in notes {
            ctx.ui.note(&note);
        }
    }

    log_run(ctx, outcome).await;

    ran?;
    landed
}

/// Which repository this run is about, asked before any task looks at a checkout.
///
/// Three runs go straight past it:
///
/// - `--repo` was given, so the answer is already on the command line and
///   `main::remember_repo` has recorded it.
/// - `--check` and `riabuild status`, which report and change nothing —
///   `config.json` is part of "nothing", and a question is a poor thing to put to
///   somebody who asked for a report.
/// - a machine with no session yet, where there is no team configuration to name
///   a default with and no GitHub sign-in to list anything through. That is the
///   first run on every machine: it provisions the org default, and the run after
///   it — which has both — puts the question.
async fn ask_which_repository(ctx: &mut Ctx, cli: &Cli) -> Result<()> {
    if cli.repo.is_some() || ctx.dry_run || ctx.org.is_none() {
        return Ok(());
    }
    riabuild_tasks::repo::choose(ctx).await?;
    Ok(())
}

/// Which Infisical folders this run's repository takes its secrets from, asked
/// once, here.
///
/// It has to be after `ask_which_repository` — the answer is about the
/// repository the picker settled on — and before the task engine, because
/// `env_local::check()` reads it and a `check()` that fetched for itself would
/// be a `check()` no test could run without a network. One request per run
/// rather than the two a `check()`/`apply()` pair would make.
///
/// A machine with no session asks nothing: `scope_for` would 401, and the
/// org-wide default this leaves in place is what the first run on every machine
/// already uses.
async fn load_secret_scope(ctx: &mut Ctx) {
    if ctx.org.is_none() {
        return;
    }
    ctx.load_secret_scope().await;
}

/// The Claude Code launchers, written unless this run promised to change nothing.
///
/// `--check` is documented as "check everything and report, changing nothing",
/// and `shims::write_all` deletes as well as writes: `prune` removes the old `c`
/// launcher and every `claude-<n>` past the end of the account list. The sharp
/// edge is the machine most in need of a `--check`: `UserConfig::load` answers an
/// unparseable `config.json` with `Default`, so `claude_accounts` is empty, and
/// `prune(bin, 0)` would take `claude` and all nine numbered launchers with it
/// during a run that promised to touch nothing.
///
/// Nothing under `--check` consumes the launchers — `provision` returns before
/// the environment shell is spawned — so skipping them costs the dry run
/// nothing.
///
/// The clipboard and browser shims come with them, on a session that has a
/// channel. The condition is deliberately the *same* one `shell::browser_for`
/// uses to export `BROWSER`, because the two have to move together: a server
/// where `BROWSER` points at `~/.riabuild/bin/xdg-open` and that file was never
/// written is worse off than one with no `BROWSER` at all. Unset, Claude Code
/// falls back to printing the URL; set and dangling, it execs a missing file and
/// the sign-in simply fails. Left unwired, this is a channel that comes up, pins
/// its socket, exports its variable — and shadows nothing, so every Ctrl+V
/// quietly reaches the server's own clipboard instead of the laptop's.
async fn write_launchers(ctx: &Ctx) -> Result<()> {
    let socket = std::env::var(riabuild_channel::SOCKET_ENV).ok();
    write_launchers_with(ctx, socket.as_deref()).await
}

/// The body, with the channel socket as a parameter rather than read from the
/// environment — so a test can drive both answers without mutating the
/// environment of a suite that runs every test in one process.
async fn write_launchers_with(ctx: &Ctx, socket: Option<&str>) -> Result<()> {
    if ctx.dry_run {
        return Ok(());
    }
    shims::write_all(ctx).await?;
    // Resolved once, before the first write: every shim below names this
    // path, and a run that cannot answer the question must fail rather
    // than fall back to a bare `riabuild` that resolves to some other
    // machine's copy or to nothing.
    let riabuild = shims::running_binary()?;
    // `agents` is written on every run rather than only where a channel exists,
    // because most agent sessions are opened on a laptop and a laptop has no
    // socket. It reaches nothing across the wire, so there is no condition for
    // it to be conditional on.
    shims::write_agents_shim(ctx, &riabuild).await?;
    if socket.is_some_and(|socket| !socket.is_empty()) {
        shims::write_clipboard_shims(ctx, &riabuild).await?;
        shims::write_browser_shim(ctx, &riabuild).await?;
        // And riabuild itself, so the developer whose session this is has the
        // command too. Written here rather than beside the other owned tools
        // because this is the condition under which riabuild is running on a
        // machine that did not install it: a server reaches its binary through
        // a versioned path nothing puts on `PATH`, so `riabuild claude new`
        // there was `command not found` — or, worse, silently ran whichever
        // riabuild a package manager had left on the box.
        shims::write_tool(ctx, "riabuild", &riabuild).await?;
    }
    Ok(())
}

pub(crate) async fn open_shell(ctx: &mut Ctx) -> Result<i32> {
    open_shell_from(ctx, shell::already_inside()).await
}

/// The body, with "is riabuild already inside its own shell" as a parameter
/// rather than read here — the same shape `reset::Request::inside_shell` takes,
/// and for the same reason: this repository is worked on from inside the
/// environment shell, so a test that read the real `RIABUILD_SHELL` would take
/// the nesting branch on a maintainer's laptop and pass without ever reaching
/// the line it was written to pin.
async fn open_shell_from(ctx: &mut Ctx, already_inside: bool) -> Result<i32> {
    if already_inside {
        // Nesting would stack PATH entries and leave the developer two `exit`s
        // away from their own terminal.
        ctx.ui
            .info("You are already in the Clubria environment. Type `exit` to leave it.");
        return Ok(0);
    }
    // The banner itself comes from the generated rcfile, inside the new shell —
    // printing it here too is what made every developer see it twice. This blank
    // line is only separation from the task list above.
    //
    // A server prints none. `riabuild shell` there is the far side of a handoff
    // the laptop already spaced — `remote::shell::open` prints its blank line
    // immediately before `ssh` or `mosh` — and mosh starts the session on a
    // screen of its own, so a blank line here is not separation from anything.
    // It is the first line of the session, above the accounts box, with nothing
    // over it; and when mosh gives the terminal back it is still there, one
    // line further from `[mosh is exiting.]` than the developer asked for.
    if ctx.server.is_none() {
        ctx.ui.blank();
    }
    shell::spawn(ctx).await
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;
    use riabuild_tasks::accounts;
    use riabuild_tasks::testing::ctx_with;
    use riabuild_ui as ui;

    /// A `Cli` for the picker tests: only `check` and `repo` are read by
    /// `ask_which_repository`.
    fn cli_with(check: bool, repo: Option<&str>) -> Cli {
        Cli {
            jobs: None,
            command: None,
            project: None,
            repo: repo.map(str::to_string),
            check,
            quiet: false,
            no_shell: true,
        }
    }
    /// A run one of whose tasks failed still lands everything that did work.
    ///
    /// The regression: `provision` propagated `engine::run_all` with `?` right
    /// where it was called, so the launchers, the notes and the log line were
    /// all skipped by the first failure — on the machine that needed them most.
    /// The engine had already been changed to carry on past a failed task; this
    /// is the half of that change that reaches the developer's disk.
    #[tokio::test]
    async fn a_run_with_a_failed_task_still_lands_what_worked() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.ui = ui::Ui::new(false);
        ctx.config.claude_accounts = vec![accounts::new_id(), accounts::new_id()];
        ctx.notes
            .push("your Codex sign-in expires on Friday".to_string());
        let outcome = engine::Outcome {
            satisfied: vec!["login"],
            applied: vec!["github_cli"],
            failed: vec!["claude_accounts"],
            skipped: vec!["claude_trust"],
        };

        let error = after_the_tasks(
            &mut ctx,
            &outcome,
            Err(anyhow::anyhow!("the browser sign-in was never finished")),
        )
        .await
        .expect_err("the run still fails");

        assert!(
            error.to_string().contains("browser sign-in"),
            "the engine's own failure is what reaches the developer: {error}"
        );

        let bin = ctx.paths.bin_dir();
        for launcher in ["claude", "claude-1", "claude-2"] {
            assert!(
                tokio::fs::try_exists(bin.join(launcher)).await.unwrap(),
                "{launcher} was skipped by a failure that had nothing to do with it"
            );
        }

        let log = tokio::fs::read_to_string(ctx.paths.log_file())
            .await
            .expect("a failed run is logged too");
        assert!(log.contains("applied=[github_cli]"), "{log}");
        assert!(log.contains("failed=[claude_accounts]"), "{log}");
        assert!(log.contains("skipped=[claude_trust]"), "{log}");
    }
    /// And a run where everything worked reports no failure of its own.
    #[tokio::test]
    async fn a_run_that_worked_returns_what_the_launchers_said() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.ui = ui::Ui::new(false);
        ctx.config.claude_accounts = vec![accounts::new_id()];
        after_the_tasks(&mut ctx, &engine::Outcome::default(), Ok(()))
            .await
            .expect("nothing failed");
        assert!(
            tokio::fs::try_exists(ctx.paths.bin_dir().join("claude"))
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn a_dry_run_never_asks_which_repository() {
        // `--check` and `riabuild status` report and change nothing, and a
        // question is a poor thing to put to somebody who asked for a report.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.ui = ui::Ui::scripted(["payments"]);
        ctx.dry_run = true;

        ask_which_repository(&mut ctx, &cli_with(true, None))
            .await
            .expect("asks nothing");

        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
        assert_eq!(ctx.config.active_repo, None);
    }
    #[tokio::test]
    async fn a_named_repository_is_not_asked_about_again() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.ui = ui::Ui::scripted(["payments"]);

        ask_which_repository(&mut ctx, &cli_with(false, Some("Clubria/payments")))
            .await
            .expect("asks nothing");

        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }
    #[tokio::test]
    async fn a_machine_with_no_session_yet_is_not_asked() {
        // Every machine's first run: there is no team configuration to name a
        // default with, and no GitHub sign-in to list anything through. It
        // provisions the org default, and the run after it puts the question.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.org = None;
        ctx.ui = ui::Ui::scripted(["payments"]);

        ask_which_repository(&mut ctx, &cli_with(false, None))
            .await
            .expect("asks nothing");

        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
        assert_eq!(ctx.config.active_repo, None);
    }
    /// The far side of a handoff prints no spacing of its own.
    ///
    /// A server's `riabuild shell` is reached by `ssh` or `mosh`, and
    /// `remote::shell::open` has already printed the blank line in front of it
    /// on the laptop. One here as well is not a second separator: mosh draws
    /// the session on a screen of its own, so it is simply the first line of
    /// that screen with nothing above it, and it is still sitting there in
    /// front of `[mosh is exiting.]` when mosh gives the terminal back.
    #[tokio::test]
    async fn a_server_starts_its_session_on_the_first_line() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.ui = ui::Ui::new(false);
        ctx.server = Some("build-01".into());

        open_shell_from(&mut ctx, false).await.expect("opens");

        assert_eq!(
            ctx.ui.blanks(),
            0,
            "the laptop that opened the connection already printed the gap"
        );
    }
    /// And the laptop, where there is no connection and nobody else to print
    /// it, still separates its own task list from the shell it opens.
    #[tokio::test]
    async fn a_laptop_still_separates_its_task_list_from_the_shell() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.ui = ui::Ui::new(false);
        assert!(ctx.server.is_none());

        open_shell_from(&mut ctx, false).await.expect("opens");

        assert_eq!(ctx.ui.blanks(), 1);
    }
    /// The last mile of the clipboard channel, and the one nothing else covers.
    /// Everything upstream can be correct — tunnel up, socket namespaced,
    /// `RIABUILD_CHANNEL_SOCKET` exported — and a developer still gets the
    /// server's own clipboard on every Ctrl+V, because `~/.riabuild/bin` shadows
    /// nothing until these are written. `BROWSER` is the sharper half: it is
    /// exported under this same condition, so a shim that is missing turns a
    /// sign-in that would have printed its URL into one that execs a file that
    /// is not there.
    #[tokio::test]
    async fn a_session_with_a_channel_gets_the_shims_that_channel_needs() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_launchers_with(&ctx, Some("/home/dev/.riabuild-remote/m1/channel.sock"))
            .await
            .unwrap();

        let bin = ctx.paths.bin_dir();
        for tool in shims::CLIPBOARD_TOOLS {
            assert!(
                tokio::fs::try_exists(bin.join(tool)).await.unwrap(),
                "{tool} must be shadowed where there is a channel to carry it"
            );
        }
        assert!(
            tokio::fs::try_exists(bin.join(shims::BROWSER_TOOL))
                .await
                .unwrap(),
            "BROWSER points here, so it has to exist"
        );

        // riabuild is the one tool riabuild does not put on `PATH` — it lives
        // in a versioned directory of its own — so on a server `riabuild` was
        // `command not found`, or silently some other copy the box happened to
        // carry. Every shim above now names the binary in full; this is what
        // gives the developer whose session it is the command as well.
        assert!(
            tokio::fs::try_exists(bin.join("riabuild")).await.unwrap(),
            "a server reaches riabuild through a path nothing else puts on PATH"
        );

        // The property the whole fix rests on: not one of these may go looking
        // for riabuild on `PATH`, because that is what resolved to nothing on a
        // server without a system copy and to the wrong version on one with.
        for name in shims::CLIPBOARD_TOOLS
            .iter()
            .copied()
            .chain([shims::BROWSER_TOOL, "riabuild"])
        {
            let script = tokio::fs::read_to_string(bin.join(name)).await.unwrap();
            let exec = script
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("exec "))
                .unwrap_or_else(|| panic!("{name} has no exec line:\n{script}"));
            // Single-quoted, which is what every generated script quotes a path
            // with: a `$`, a backtick or a `"` in a home directory would be
            // expanded inside double quotes and is inert inside these.
            assert!(
                exec.starts_with("exec '/"),
                "{name} does not name an absolute binary: {exec}"
            );
        }
    }
    /// A laptop shadows neither: its clipboard and its browser are already the
    /// developer's own, and a shim pointing down a channel that does not exist
    /// would break both.
    #[tokio::test]
    async fn a_session_with_no_channel_shadows_neither_clipboard_nor_browser() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_launchers_with(&ctx, None).await.unwrap();
        let bin = ctx.paths.bin_dir();
        assert!(!tokio::fs::try_exists(bin.join("xclip")).await.unwrap());
        assert!(
            !tokio::fs::try_exists(bin.join(shims::BROWSER_TOOL))
                .await
                .unwrap()
        );
        // An empty variable is not a channel — the same rule `browser_for` uses.
        write_launchers_with(&ctx, Some("")).await.unwrap();
        assert!(!tokio::fs::try_exists(bin.join("xclip")).await.unwrap());
    }
    #[tokio::test]
    async fn a_dry_run_writes_launchers_for_the_accounts_it_found() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id(), accounts::new_id()];
        write_launchers(&ctx).await.unwrap();

        ctx.dry_run = true;
        write_launchers(&ctx).await.unwrap();

        let bin = ctx.paths.bin_dir();
        assert!(tokio::fs::try_exists(bin.join("claude")).await.unwrap());
        assert!(tokio::fs::try_exists(bin.join("claude-2")).await.unwrap());
    }
    #[tokio::test]
    async fn a_dry_run_on_a_machine_with_an_unreadable_config_deletes_no_launcher() {
        // The reason this is gated at all. `UserConfig::load` answers an
        // unparseable `config.json` with `Default`, so `claude_accounts` is
        // empty on exactly the machine a developer would run `riabuild --check`
        // on — and `shims::prune` would then delete `claude` and every
        // `claude-1..9` during a run documented as changing nothing.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id(), accounts::new_id()];
        write_launchers(&ctx).await.unwrap();

        ctx.config.claude_accounts.clear();
        ctx.dry_run = true;
        write_launchers(&ctx).await.unwrap();

        let bin = ctx.paths.bin_dir();
        assert!(
            tokio::fs::try_exists(bin.join("claude")).await.unwrap(),
            "--check deleted the primary launcher"
        );
        assert!(
            tokio::fs::try_exists(bin.join("claude-1")).await.unwrap(),
            "--check deleted a numbered launcher"
        );
        assert!(
            tokio::fs::try_exists(bin.join("claude-2")).await.unwrap(),
            "--check deleted a numbered launcher"
        );
    }
}

//! The default flow: `riabuild` with no subcommand.
//!
//! Connect, say whose machine this is, run every task, write the shims, log
//! the run, and hand the developer their shell. `main.rs` decides *which* flow
//! an invocation is; this is the one that provisions — and it is also where
//! riabuild's own upgrade used to live, before `main::keep_current` widened it
//! to every command.

use crate::cli::Cli;
use crate::{opens_shell, tasks};
use anyhow::Result;
use riabuild_paths::config;
use riabuild_tasks::shell;
use riabuild_tasks::shims;
use riabuild_tasks::{Ctx, engine};
use riabuild_ui as ui;

/// The lock a provisioning run holds across its tasks, or `None` under `--check`.
///
/// Two runs would otherwise both find node missing and both download it —
/// roughly 130 MB per lost race, into a directory nothing sweeps.
///
/// Not taken under `--check`, which writes nothing and must never make another
/// window wait. Not machine-wide either: the path comes from `root()`, which is
/// namespaced per developer on a server, so one lock for the box would let one
/// developer block another under the single Unix account they share — a denial
/// of service wearing robustness as a disguise. Two developers installing the
/// same toolchain concurrently is already safe; `archive/staging.rs` unpacks
/// beside the target and renames.
async fn provisioning_lock(ctx: &Ctx) -> Result<Option<riabuild_paths::filelock::FileLock>> {
    if ctx.dry_run {
        return Ok(None);
    }
    let path = ctx.paths.provision_lock_file();
    // The callback borrows the `Ui` rather than owning it — `Ui` is not `Clone`,
    // and does not need to be for a line printed before the wait begins.
    let lock = riabuild_paths::filelock::FileLock::acquire(&path, || {
        ctx.ui
            .info("Waiting for the riabuild already setting up this machine…");
    })
    .await
    .map_err(|error| {
        ui::Failure::new(
            "waiting for another riabuild to finish",
            "close the other riabuild, or run this again once it has finished",
        )
        .detail(format!("{error:#}"))
    })?;
    Ok(Some(lock))
}

pub(crate) async fn provision(ctx: &mut Ctx, cli: &Cli) -> Result<i32> {
    ctx.ui.banner("Clubria");
    ctx.connect().await?;
    describe_session(ctx);
    ask_which_repository(ctx, cli).await?;
    describe_repo(ctx);

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
    let provisioning = provisioning_lock(ctx).await?;

    ctx.ui.heading("Checking this machine");
    let registry = tasks::registry();
    let outcome = engine::run_all(&registry, ctx).await?;

    write_launchers(ctx).await?;

    let notes = std::mem::take(&mut ctx.notes);
    if !notes.is_empty() {
        ctx.ui.heading("Worth knowing");
        for note in notes {
            ctx.ui.note(&note);
        }
    }

    log_run(ctx, &outcome).await;

    // Released here, before every return below it and before `open_shell` above
    // all: that call awaits the developer's interactive shell for as long as
    // their window stays open, and a lock held across it would make the second
    // window wait on a human rather than on a download.
    drop(provisioning);

    if ctx.dry_run {
        ctx.ui.blank();
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
        ctx.ui.blank();
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
    if socket.is_some_and(|socket| !socket.is_empty()) {
        // Resolved once, before the first write: every shim below names this
        // path, and a run that cannot answer the question must fail rather
        // than fall back to a bare `riabuild` that resolves to some other
        // machine's copy or to nothing.
        let riabuild = shims::running_binary()?;
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

/// Who riabuild thinks this machine belongs to, and where the token lives.
///
/// Printed on every run because "riabuild is using the wrong account" is
/// otherwise invisible until something fails for a confusing reason.
fn describe_session(ctx: &Ctx) {
    let Some(member) = &ctx.member else {
        ctx.ui
            .note("not signed in yet — riabuild will give you a code to approve");
        return;
    };
    ctx.ui.note(&format!(
        "signed in as {} <{}> · {} · token in {}",
        member.display_name(),
        member.email,
        member.role,
        ctx.keychain.describe(),
    ));
}

/// Which repository this run is about, and where its checkout is.
///
/// Printed for the same reason `describe_session` is: with more than one
/// repository in play, "riabuild is working on the wrong one" is otherwise
/// invisible until a task says something that reads as a fault.
fn describe_repo(ctx: &Ctx) {
    let Ok(repo) = ctx.repo() else {
        return;
    };
    let home = ctx.paths.home();
    let checkout = match ctx.project_dir() {
        Some(dir) => riabuild_paths::contract_tilde(&dir, &home),
        None => "not cloned yet".to_string(),
    };
    ctx.ui.note(&format!("working on {repo} · {checkout}"));
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
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;
    use riabuild_tasks::accounts;
    use riabuild_tasks::testing::ctx_with;

    /// A `Cli` for the picker tests: only `check` and `repo` are read by
    /// `ask_which_repository`.
    fn cli_with(check: bool, repo: Option<&str>) -> Cli {
        Cli {
            command: None,
            project: None,
            repo: repo.map(str::to_string),
            check,
            quiet: false,
            no_shell: true,
        }
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

    /// `--check` changes nothing, so it must never make a second window wait.
    #[tokio::test]
    async fn a_dry_run_takes_no_provisioning_lock() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.dry_run = true;

        let taken = provisioning_lock(&ctx).await.expect("dry run");

        assert!(
            taken.is_none(),
            "a run that promises to change nothing must not hold the provisioning lock"
        );
    }

    /// Dropping the guard is what `provision` does before the shell handoff, so
    /// a second window has to find the lock free immediately afterwards.
    #[tokio::test]
    async fn a_real_run_takes_the_lock_and_releases_it_when_dropped() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;

        let taken = provisioning_lock(&ctx).await.expect("acquire");
        assert!(
            taken.is_some(),
            "a real run holds the lock across its tasks"
        );
        drop(taken);

        let waited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&waited);
        let _second = riabuild_paths::filelock::FileLock::acquire(
            &ctx.paths.provision_lock_file(),
            move || flag.store(true, std::sync::atomic::Ordering::SeqCst),
        )
        .await
        .expect("second acquire");

        assert!(
            !waited.load(std::sync::atomic::Ordering::SeqCst),
            "the lock was still held after the guard was dropped"
        );
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
            assert!(
                exec.starts_with(r#"exec "/"#),
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

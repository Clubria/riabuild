//! The default flow: `riabuild` with no subcommand.
//!
//! Connect, say whose machine this is, upgrade if the org requires it, run
//! every task, write the shims, log the run, and hand the developer their
//! shell. `main.rs` decides *which* flow an invocation is; this is the one
//! that provisions.

use crate::cli::Cli;
use crate::config;
use crate::shell;
use crate::shims;
use crate::tasks::{Ctx, engine};
use crate::ui;
use crate::update;
use crate::{connect, opens_shell, tasks};
use anyhow::Result;

pub(crate) async fn provision(ctx: &mut Ctx, cli: &Cli) -> Result<i32> {
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

    write_launchers(ctx).await?;

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
    let socket = std::env::var(crate::channel::SOCKET_ENV).ok();
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
        shims::write_clipboard_shims(ctx).await?;
        shims::write_browser_shim(ctx).await?;
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

pub(crate) async fn open_shell(ctx: &mut Ctx) -> Result<i32> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;

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

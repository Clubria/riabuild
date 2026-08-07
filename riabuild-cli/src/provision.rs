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

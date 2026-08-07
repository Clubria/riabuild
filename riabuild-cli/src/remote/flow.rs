//! `riabuild remote` — which of its three jobs was asked for, and what this
//! laptop has to be able to do before any of them touches a server.
//!
//! Kept separate from `mod.rs`, which already carries `Remote` and the
//! shell-safety primitives every step here builds commands with — folding the
//! orchestration in as well would have pushed that file well past the
//! crate's ~300-line production budget. The teardown half lives one door
//! further along in `forget.rs`, and the connect sequence itself — choose a
//! server, pin its host key, authorise, install, hand over a shell — one
//! door the other way in `flow/connect.rs`, both split out of this file for
//! the same reason and by the same precedent.

mod connect;

use super::{forget, store};
use crate::cli::{Cli, Command, RemoteAction};
use crate::tasks::{Ctx, Status, Task};
use crate::ui::Failure;
use anyhow::{Result, anyhow};

/// `riabuild remote` — the whole flow.
pub async fn run(
    ctx: &mut Ctx,
    cli: &Cli,
    target: Option<String>,
    action: Option<RemoteAction>,
) -> Result<i32> {
    let mut store = store::Store::load(ctx.paths.as_ref()).await;

    match action {
        Some(RemoteAction::List) => return store::list(ctx, &store),
        Some(RemoteAction::Forget { name }) => {
            // Needs `ctx.member`/`ctx.api`'s bearer token, both of which
            // `connect` populates: the API revoke below authenticates as
            // this laptop's own session, and the server-side cleanup needs
            // to know whose namespace it is clearing. The default flow below
            // also calls `connect`, but only after this match already
            // returned for `list`/`forget`.
            crate::connect(ctx).await?;
            let member = ctx
                .member
                .clone()
                .ok_or_else(|| anyhow!("riabuild does not know who you are yet"))?;
            forget::forget_remote(
                ctx.paths.as_ref(),
                ctx.runner.clone(),
                &ctx.ui,
                &ctx.api,
                &member.member_id,
                &mut store,
                &name,
            )
            .await?;
            return Ok(0);
        }
        None => {}
    }

    // `main::connect` is what populates `ctx.member` and `ctx.org`, and it only
    // runs from `provision` — which this command never reaches. Without it,
    // `ctx.org()?` below fails on every single `riabuild remote` with "riabuild
    // has not loaded the team configuration yet".
    crate::connect(ctx).await?;

    // The laptop then runs exactly two tasks: sign-in, because it mints the
    // server's session, and GitHub, because the server borrows this laptop's
    // sign-in. `github_cli`'s check also re-verifies org membership, so a
    // departed developer fails here rather than on somebody's server.
    ensure_local_prerequisites(ctx).await?;

    let accept_host_key = accept_host_key_of(cli);
    connect::connect_and_setup(ctx, cli, &mut store, target, accept_host_key).await
}

/// The `--accept-host-key` value for this invocation, if any.
///
/// Scoped to `Command::Remote` (R13 in `decisions.md`), not a global `Cli`
/// field, so it is reached by matching `cli.command` rather than reading a
/// top-level field.
fn accept_host_key_of(cli: &Cli) -> Option<&str> {
    match &cli.command {
        Some(Command::Remote {
            accept_host_key, ..
        }) => accept_host_key.as_deref(),
        _ => None,
    }
}

/// The two tasks a laptop runs before it touches a server.
async fn ensure_local_prerequisites(ctx: &mut Ctx) -> Result<()> {
    for task in [
        Box::new(crate::tasks::login::Login) as Box<dyn Task>,
        Box::new(crate::tasks::github_cli::GithubCli),
    ] {
        if let Status::Needs(reason) = task.check(ctx).await? {
            ctx.ui.working(task.title(), &reason.describe());
            task.apply(ctx).await?;
            // The invariant: apply is always followed by a re-run of check.
            if let Status::Needs(reason) = task.check(ctx).await? {
                return Err(Failure::new(
                    format!("getting {} ready on this laptop", task.title()),
                    "Run `riabuild` on this machine and see what it says.",
                )
                .detail(reason.describe())
                .into());
            }
            ctx.ui.applied(task.title());
        } else {
            ctx.ui.satisfied(task.title());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    const GOOD_FINGERPRINT: &str = "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y";

    #[test]
    fn accept_host_key_is_read_out_of_command_remote_not_a_global_field() {
        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        assert_eq!(accept_host_key_of(&cli), Some(GOOD_FINGERPRINT));

        let no_flag = Cli::parse_from(["riabuild", "remote", "build-01"]);
        assert_eq!(accept_host_key_of(&no_flag), None);

        let other_command = Cli::parse_from(["riabuild", "status"]);
        assert_eq!(accept_host_key_of(&other_command), None);
    }
}

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

use super::{Request, forget, store};
use anyhow::{Result, anyhow};
use riabuild_tasks::{Ctx, Status, Task};
use riabuild_ui::Failure;

/// `riabuild remote list`
pub async fn list(ctx: &mut Ctx) -> Result<i32> {
    let store = store::Store::load(ctx.paths.as_ref()).await;
    store::list(ctx, &store)
}

/// `riabuild remote forget <server>`
pub async fn forget_server(ctx: &mut Ctx, name: &str) -> Result<i32> {
    let mut store = store::Store::load(ctx.paths.as_ref()).await;

    // Needs `ctx.member`/`ctx.api`'s bearer token, both of which `connect`
    // populates: the API revoke below authenticates as this laptop's own
    // session, and the server-side cleanup needs to know whose namespace it is
    // clearing.
    ctx.connect().await?;
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
        name,
    )
    .await?;
    Ok(0)
}

/// `riabuild remote [server]` — the whole flow.
pub async fn run(ctx: &mut Ctx, request: Request) -> Result<i32> {
    let mut store = store::Store::load(ctx.paths.as_ref()).await;

    // `Ctx::connect` is what populates `ctx.member` and `ctx.org`, and it only
    // runs from `provision` — which this command never reaches. Without it,
    // `ctx.org()?` below fails on every single `riabuild remote` with "riabuild
    // has not loaded the team configuration yet".
    ctx.connect().await?;

    // The laptop then runs exactly two tasks: sign-in, because it mints the
    // server's session, and GitHub, because the server borrows this laptop's
    // sign-in. `github_cli`'s check also re-verifies org membership, so a
    // departed developer fails here rather than on somebody's server.
    ensure_local_prerequisites(ctx).await?;

    connect::connect_and_setup(ctx, &request, &mut store).await
}

/// The two tasks a laptop runs before it touches a server.
async fn ensure_local_prerequisites(ctx: &mut Ctx) -> Result<()> {
    for task in [
        Box::new(riabuild_tasks::login::Login) as Box<dyn Task>,
        Box::new(riabuild_tasks::github_cli::GithubCli),
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

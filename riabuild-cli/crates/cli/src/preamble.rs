//! The three things every run does before the command the developer typed.
//!
//! `run_inner` opens with these in order: replace this binary if the org
//! publishes a newer one, then record what `--repo` and `--project` said, so
//! that whatever runs next is already about the right repository in the right
//! checkout. Each of the two `remember`s stands aside for a remote
//! invocation, which is asking about a machine that is not this one.

use anyhow::Result;

use riabuild_paths::expand_tilde;
use riabuild_tasks::{self as tasks, Ctx};

use crate::cli::{Cli, Command};
use crate::update;

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
pub(crate) async fn keep_current(cli: &Cli, ctx: &mut Ctx) -> Result<()> {
    match version_action(cli, ctx).await {
        update::Action::Upgrade { to, mandatory } => {
            update::upgrade_and_reexec(ctx.runner.as_ref(), &ctx.ui, &to, mandatory).await
        }
        update::Action::Continue => Ok(()),
    }
}

/// What `keep_current` decided, with the upgrade itself left to it.
///
/// Separated so the decision can be asserted: performing the other half ends
/// in `process::exit`, which a test cannot survive.
///
/// **The connect error is swallowed rather than returned, and `action_for` is
/// consulted either way.** That is the fix for the lockout, and it is one half
/// of a pair — `Ctx::connect` fetches `/org/config` before `/me`, so a build
/// below `minCliVersion` comes back here with the floor loaded *and* an error,
/// which is precisely the case that most needs the upgrade below to run. An
/// early return on the error is what made a raised floor permanent: `/me`
/// enforces it, so `connect` failed, so the upgrade that would clear it was
/// never even considered.
///
/// Everything else that can make a connect fail — no network, a dashboard
/// that is down — leaves `ctx.org` `None`, and `action_for` answers
/// `Continue` for exactly that reason. Whatever riabuild-web could not tell
/// us, "you are running an old riabuild" is not something to guess at, and it
/// is never worth failing a command over.
pub(crate) async fn version_action(cli: &Cli, ctx: &mut Ctx) -> update::Action {
    if !update::applies_to(cli.command.as_ref()) {
        return update::Action::Continue;
    }
    let _ = ctx.connect().await;
    update::action_for(ctx)
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
pub(crate) async fn remember_project(cli: &Cli, ctx: &mut Ctx) -> Result<()> {
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
pub(crate) async fn remember_repo(cli: &Cli, ctx: &mut Ctx) -> Result<()> {
    // `--repo` with nothing after it names no repository: it is a request to be
    // asked, which `provision::ask_which_repository` acts on and this does not.
    let Some(named) = cli.named_repo() else {
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

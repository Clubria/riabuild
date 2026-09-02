//! Task 7 — the developer's Claude Code accounts.
//!
//! riabuild creates the account directories and never writes into anyone's
//! `settings.json`. Org policy is layered at launch by the `claude-<n>`
//! launchers instead — see `org_settings` for why a recurring deep-merge is the
//! wrong shape.
//!
//! Account 1 is the one this task insists on: it must exist, and it must be
//! signed in. riabuild's job is "running Claude Code against our codebase", and
//! a signed-out Claude Code is not that. Accounts 2 upward are the developer's
//! own business — the account box reports them and this task ignores them.

mod install;
mod sign_in;

use install::install_claude;
use sign_in::sign_in;

use super::{Ctx, Resource, Status, Task, TaskId};
use crate::accounts::{self, status::Identity};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_runner::RunOptions;
use riabuild_ui::Failure;
use riabuild_version as version;

/// The version every behaviour this task depends on was verified against.
///
/// Not an arbitrary bump: `claude auth status --json`, `claude auth login`, and
/// the per-`CLAUDE_CONFIG_DIR` keychain scoping that makes two accounts two
/// independent sign-ins were only ever confirmed on 2.1.223. A developer on
/// 2.0.x may not have `auth status --json` at all, which this task now treats as
/// a hard failure rather than a misread. Raising the floor costs nothing —
/// `install_claude` installs whatever npm calls latest.
const MIN_VERSION: &str = "2.1.223";

pub struct ClaudeAccounts;

/// Whether riabuild has to install Claude Code before it can be used.
///
/// The existence test comes first and is not optional. `RealRunner::run` returns
/// `Err` when the program is not there — a spawn failure, not an exit code — so
/// asking `--version` first makes the missing-binary case propagate an `anyhow`
/// chain instead of reaching `install_claude`. The task whose job is installing
/// Claude Code would abort before it could. `github_cli` and `toolchain` gate on
/// `try_exists` for the same reason.
///
/// An installed copy below the floor routes here too: `install_claude` is the
/// upgrade path as well as the install path.
async fn install_needed(ctx: &Ctx) -> Result<bool> {
    let claude = ctx.claude();
    if !tokio::fs::try_exists(&claude).await.unwrap_or(false) {
        return Ok(true);
    }
    let reported = ctx
        .runner
        .run(&claude, &["--version"], &RunOptions::default())
        .await?;
    Ok(!reported.ok() || !version::at_least(reported.trimmed(), MIN_VERSION))
}

#[async_trait]
impl Task for ClaudeAccounts {
    fn id(&self) -> TaskId {
        "claude_accounts"
    }

    fn title(&self) -> &str {
        "Claude Code accounts"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // Claude Code is installed with the Node riabuild owns, so the
        // toolchain has to exist first.
        &["toolchain"]
    }

    fn writes(&self) -> &[Resource] {
        &["node_prefix"]
    }

    /// `claude auth login` opens a browser, and `sign_in` reads
    /// `ui.interactive()` before it does. This is the task the registry's
    /// own comment calls "the one task that waits on a browser".
    fn interactive(&self) -> bool {
        true
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        // Existence before invocation — see `install_needed`. What makes this
        // safe is the dependency edge and not the string: `depends_on
        // (["toolchain"])` pins a Node first, so `ctx.claude()` is an absolute
        // path under the tree riabuild owns by the time this runs. The bare
        // name it falls back to before a Node is pinned would *not* be safe
        // here — `try_exists("claude")` resolves against the current directory,
        // so a checkout containing a file called `claude` satisfies it.
        let claude = ctx.claude();
        if !tokio::fs::try_exists(&claude).await.unwrap_or(false) {
            return Ok(Status::needs("Claude Code is not installed"));
        }
        let reported = ctx
            .runner
            .run(&claude, &["--version"], &RunOptions::default())
            .await?;
        if !reported.ok() {
            return Ok(Status::needs("Claude Code is not installed"));
        }
        if !version::at_least(reported.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!(
                "Claude Code {} is older than {MIN_VERSION}",
                reported.trimmed()
            )));
        }

        let ids = &ctx.config.claude_accounts;
        let Some(primary) = ids.first() else {
            return Ok(Status::needs("no Claude Code account yet"));
        };
        // Both of these name the account they are about: each is a condition a
        // developer has to act on by hand, and "an account is not registered"
        // does not say which of nine directories to deal with.
        for (index, id) in ids.iter().enumerate() {
            if !tokio::fs::try_exists(ctx.paths.claude_profile_dir(id))
                .await
                .unwrap_or(false)
            {
                return Ok(Status::needs(format!(
                    "Claude Code account {}'s directory is missing ({id})",
                    index + 1
                )));
            }
        }
        // A directory nothing recorded is drift in the other direction: real
        // sessions and a real login that no riabuild command can reach.
        for found in accounts::ids_on_disk(&ctx.paths.claude_dir()).await {
            if !ids.contains(&found) {
                return Ok(Status::needs(format!(
                    "the Claude Code account directory {found} is not registered"
                )));
            }
        }

        match accounts::status::read(ctx, primary).await {
            Identity::LoggedIn(_) => Ok(Status::Satisfied),
            Identity::LoggedOut => Ok(Status::needs("account 1 is not signed in")),
            Identity::Unknown(why) => Ok(Status::needs(format!(
                "riabuild could not tell whether account 1 is signed in: {why}"
            ))),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if install_needed(ctx).await? {
            install_claude(ctx).await?;
        }

        let claude_dir = ctx.paths.claude_dir();
        tokio::fs::create_dir_all(&claude_dir).await?;

        // Which registered accounts lost their directory. Both existence tests
        // go through `claude_profile_dir` so they cannot drift apart from
        // `check()`'s.
        let mut vanished = Vec::new();
        for id in ctx.config.claude_accounts.clone() {
            if !tokio::fs::try_exists(ctx.paths.claude_profile_dir(&id))
                .await
                .unwrap_or(false)
            {
                vanished.push(id);
            }
        }
        let on_disk = accounts::ids_on_disk(&claude_dir).await;

        // The id this machine gets if it turns out to have no account at all.
        // Minted out here because the mutation below runs inside the config
        // lock and may not touch the disk; it is thrown away unused on every
        // machine that already has one.
        let fresh = accounts::new_id();

        // Decided against the disk out here, and *applied* in there — by id,
        // never by assigning the whole `Vec` back. This used to hand
        // `update_config` a list built from `ctx.config`, the snapshot this
        // process read at start, which is the lost update `UserConfig::update`
        // exists to prevent: a `riabuild claude primary 2` in another terminal
        // was undone on the next run, and since **position is the number**,
        // `claude-2` then opened a different account than the developer had
        // just chosen. An account that terminal had registered but not yet
        // created the directory for — `claude new` does those two in that
        // order, deliberately — was dropped from the registry outright.
        //
        // Saved before the orphan is reported: dropping accounts whose
        // directories vanished is real progress, and losing it would make the
        // next run repeat the same work to reach the same error.
        let (created, blocked) = accounts::command::update_accounts(ctx, |config| {
            for id in &vanished {
                accounts::remove_id(config, id);
            }
            // At the cap there is no number left to give an orphan, and no
            // choice riabuild may make on the developer's behalf: one of these
            // directories is a login and a year of sessions. Adopting silently
            // is impossible and skipping silently wedges every future run —
            // `check()` would keep reporting the orphan, `apply()` would keep
            // changing nothing, and the engine would keep turning that into "it
            // did not take effect", which names nothing the developer can act
            // on. So say what is wrong.
            let mut blocked = None;
            for found in &on_disk {
                if config.claude_accounts.iter().any(|held| held == found) {
                    continue;
                }
                if config.claude_accounts.len() >= accounts::MAX {
                    blocked = Some(found.clone());
                    break;
                }
                config.claude_accounts.push(found.clone());
            }
            let mut created = None;
            if config.claude_accounts.is_empty() {
                config.claude_accounts.push(fresh.clone());
                created = Some(fresh);
            }
            Ok((created, blocked))
        })
        .await?;

        // After the registration rather than before it, which is the order
        // `accounts::command::new` takes and for its reason: a directory
        // created first and then not registered is an account nothing can
        // number. A registration whose directory did not land is the case
        // `check()` already names and the next `apply()` already drops.
        if let Some(id) = created {
            tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id)).await?;
        }

        if let Some(found) = blocked {
            let dir = ctx.paths.claude_profile_dir(&found);
            return Err(Failure::new(
                "numbering a Claude Code account directory riabuild found on disk",
                format!(
                    "Delete {} if you do not want it, or free a number with `riabuild claude delete <number>`, then run `riabuild` again.",
                    dir.display()
                ),
            )
            .detail(format!(
                "riabuild numbers at most {} accounts and you already have that many, so {found} cannot be one of them",
                accounts::MAX
            ))
            .into());
        }

        let Some(primary) = ctx.config.claude_accounts.first().cloned() else {
            return Ok(());
        };
        // `LoggedOut` and `Unknown` are not the same thing, and collapsing them
        // here would spend riabuild's ignorance as a browser sign-in on every
        // single run of a machine whose sign-in state simply cannot be read.
        // `accounts::status` goes to real lengths to keep them apart.
        match accounts::status::read(ctx, &primary).await {
            Identity::LoggedIn(_) => Ok(()),
            Identity::LoggedOut => sign_in(ctx, &primary).await,
            Identity::Unknown(why) => Err(Failure::new(
                "reading whether your Claude Code account is signed in",
                "Run `riabuild` again. If it keeps failing, run that command yourself and send its output to your team lead.",
            )
            .command("claude auth status --json")
            .detail(why)
            .into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::install::npm_env;
    use super::*;
    use crate::accounts;
    use crate::testing::{Bounds, ctx_with, write_file};
    use riabuild_runner::FakeRunner;
    use std::path::Path;
    use std::sync::Arc;

    const VERSION: &str = "claude --version";
    const STATUS: &str = "claude auth status --json";
    const NODE: &str = "22.23.1";

    fn installed() -> FakeRunner {
        FakeRunner::new().with(VERSION, 0, "2.1.223 (Claude Code)", "")
    }

    fn signed_in() -> FakeRunner {
        installed().with(
            STATUS,
            0,
            r#"{"loggedIn":true,"email":"clubria@proton.me"}"#,
            "",
        )
    }

    /// A ctx whose Claude Code binary is where `ctx.claude()` says it is.
    ///
    /// The file's contents are irrelevant — every invocation goes through
    /// `FakeRunner` — but it has to exist, because its existence is what tells a
    /// provisioned machine from a bare one. Tests that want the bare case use
    /// `ctx_with` and assert the task asks to install.
    async fn ctx_with_claude(runner: FakeRunner) -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = ctx_with(runner).await;
        // Written to disk, not just into `ctx.config`: `apply` updates the
        // config under the lock, which reloads, and a pin that was never on
        // disk would be discarded there — leaving `ctx.claude()` as the bare
        // name and every later assertion reading "Claude Code is not installed".
        ctx.update_config(|config| config.node_version = Some(NODE.into()))
            .await
            .unwrap();
        write_file(Path::new(&ctx.claude()), "#!/bin/sh\n").await;
        (ctx, home)
    }

    /// A ctx with one account on disk and Claude Code installed and signed in.
    async fn ready() -> (Ctx, tempfile::TempDir, String) {
        let (mut ctx, home) = ctx_with_claude(signed_in()).await;
        let id = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
            .await
            .unwrap();
        let registered = vec![id.clone()];
        ctx.update_config(|config| config.claude_accounts = registered)
            .await
            .unwrap();
        (ctx, home, id)
    }

    #[tokio::test]
    async fn a_missing_claude_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_claude_riabuild_has_not_installed_is_never_run() {
        // `RealRunner::run` answers a missing binary with `Err` — a spawn
        // failure, not an exit code — so a check that asked `--version` before
        // testing for the file would propagate an anyhow chain with no next
        // action, and `apply` would abort before reaching `install_claude`.
        // `FakeRunner` cannot reproduce a spawn error: it answers an unstubbed
        // command with exit 127 inside an `Ok`. So this pins the observable
        // half instead — a runner that would gladly answer is never asked.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();
        ctx.config.node_version = Some(NODE.into());

        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not installed"),
            "{status:?}"
        );
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    #[tokio::test]
    async fn applying_installs_claude_code_before_running_it() {
        // The other half of the same bug: the task whose job is installing
        // Claude Code must reach `install_claude`. There is no npm on this
        // machine, so that is as far as it gets — which is the point.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();
        ctx.config.node_version = Some(NODE.into());

        let error = ClaudeAccounts
            .apply(&mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("installing Claude Code"), "{error}");
        assert!(
            !runner.calls().iter().any(|call| call.contains(VERSION)),
            "{:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn claude_code_is_installed_where_riabuild_looks_for_it() {
        // `npm install -g` decides where a binary lands from the Node that
        // *interprets* npm and from any `prefix` in the developer's own
        // `~/.npmrc` — neither of which is riabuild's Node. Left to npm, Claude
        // Code installs beside whatever Node is on `PATH`, and `check()`, which
        // reads `Ctx::claude()` under riabuild's own Node, then reports Claude
        // Code as not installed on a machine that has just installed it. The
        // task can never satisfy, so every run installs it again.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.node_version = Some(NODE.into());
        let node_dir = ctx.paths.node_dir(NODE);
        write_file(&node_dir.join("bin").join("npm"), "#!/bin/sh\n").await;

        let runner = Arc::new(FakeRunner::new().with("npm install", 0, "", ""));
        ctx.runner = runner.clone();

        install_claude(&mut ctx).await.expect("the install runs");

        let call = runner
            .calls()
            .into_iter()
            .find(|call| call.contains("install"))
            .expect("npm was run");
        assert!(
            call.contains(&format!("--prefix {}", node_dir.display())),
            "{call}"
        );
    }

    /// And it is given long enough to finish. A package download over a link
    /// riabuild does not choose is not the hung call `RunOptions`' default
    /// ceiling is a bound against; pinned against the literal and against that
    /// default, so dropping the explicit bound fails here rather than on a
    /// developer's slow connection.
    #[tokio::test]
    async fn installing_claude_code_is_given_its_own_patience() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.node_version = Some(NODE.into());
        write_file(
            &ctx.paths.node_dir(NODE).join("bin").join("npm"),
            "#!/bin/sh\n",
        )
        .await;
        let bounds = Bounds::default();
        ctx.runner = bounds.watching(Arc::new(FakeRunner::new().with("npm install", 0, "", "")));

        install_claude(&mut ctx).await.expect("the install runs");

        assert_eq!(
            bounds.of("install -g"),
            Some(std::time::Duration::from_secs(1800))
        );
        assert_ne!(
            bounds.of("install -g"),
            RunOptions::default().timeout,
            "tens of megabytes off the registry is not a ten-minute call"
        );
    }

    #[test]
    fn the_install_runs_npm_under_riabuilds_own_node() {
        // The other half: `bin/npm` is a symlink to a script whose shebang is
        // `#!/usr/bin/env node`, so without this the install needs a system Node
        // to run at all — and uses it to decide the prefix.
        let bin = std::path::Path::new("/Users/ada/.riabuild/node/22.23.1/bin");
        let env = npm_env(bin);
        let (key, value) = env.first().expect("npm gets an environment");
        assert_eq!(key, "PATH");
        assert!(
            value.starts_with("/Users/ada/.riabuild/node/22.23.1/bin:"),
            "{value}"
        );
    }

    #[tokio::test]
    async fn an_old_claude_is_detected() {
        let (ctx, _home) =
            ctx_with_claude(FakeRunner::new().with(VERSION, 0, "1.9.0 (Claude Code)", "")).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("older than"), "{status:?}");
    }

    #[tokio::test]
    async fn an_old_claude_is_upgraded_rather_than_left_alone() {
        // `install_claude` is the upgrade path as well as the install path, so
        // a copy below the floor has to route to it — reaching the npm check is
        // how that shows here.
        let (mut ctx, _home) =
            ctx_with_claude(FakeRunner::new().with(VERSION, 0, "2.0.5 (Claude Code)", "")).await;
        let error = ClaudeAccounts
            .apply(&mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("installing Claude Code"), "{error}");
    }

    #[tokio::test]
    async fn a_machine_with_no_account_is_detected() {
        let (ctx, _home) = ctx_with_claude(installed()).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("no Claude Code account"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_deleted_account_directory_is_noticed() {
        let (mut ctx, _home) = ctx_with_claude(installed()).await;
        tokio::fs::create_dir_all(ctx.paths.claude_dir())
            .await
            .unwrap();
        let id = accounts::new_id();
        ctx.config.claude_accounts = vec![id.clone()];
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("directory is missing"),
            "{status:?}"
        );
        // Named, because repairing this by hand means knowing which one.
        assert!(format!("{status:?}").contains(&id), "{status:?}");
    }

    #[tokio::test]
    async fn a_directory_nothing_recorded_is_noticed() {
        let (ctx, _home, _id) = ready().await;
        let orphan = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&orphan))
            .await
            .unwrap();
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("not registered"),
            "{status:?}"
        );
        assert!(format!("{status:?}").contains(&orphan), "{status:?}");
    }

    #[tokio::test]
    async fn a_sign_in_state_riabuild_cannot_read_is_not_called_signed_out() {
        // No stub for `auth status --json`, so the answer will not parse.
        // `Unknown` is a distinct reason on purpose: reporting it as signed out
        // would assert something about the account that nothing established.
        let (mut ctx, _home, _id) = ready().await;
        ctx.runner = Arc::new(installed());
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        let described = format!("{status:?}");
        assert!(described.contains("could not tell"), "{described}");
        assert!(!described.contains("is not signed in"), "{described}");
    }

    #[tokio::test]
    async fn a_signed_out_primary_is_drift() {
        let (mut ctx, _home, _id) = ready().await;
        ctx.runner = Arc::new(installed().with(STATUS, 1, r#"{"loggedIn":false}"#, ""));
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(
            format!("{status:?}").contains("account 1 is not signed in"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn a_provisioned_machine_is_satisfied() {
        let (ctx, _home, _id) = ready().await;
        assert_eq!(ClaudeAccounts.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_creates_the_first_account() {
        let (mut ctx, _home) = ctx_with_claude(signed_in()).await;
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts.len(), 1);
        assert_eq!(ClaudeAccounts.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_directory_nothing_recorded_is_adopted_rather_than_abandoned() {
        // The rescue this exists for: config.json lost, but the login and a
        // year of session history are still sitting in the directory.
        let (mut ctx, _home) = ctx_with_claude(signed_in()).await;
        let orphan = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&orphan))
            .await
            .unwrap();

        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, vec![orphan]);
    }

    #[tokio::test]
    async fn an_orphan_that_cannot_be_numbered_is_reported_rather_than_ignored() {
        // The cap deadlock: `check()` reports the orphan, `apply()` can do
        // nothing about it, and the engine turns the still-failing re-check into
        // "it did not take effect" — so every later run aborts here, at a task
        // that has stopped explaining itself. The developer has to choose, so
        // apply() says so and names the directory.
        let (mut ctx, _home) = ctx_with_claude(signed_in()).await;
        let mut registered = Vec::new();
        for _ in 0..accounts::MAX {
            let id = accounts::new_id();
            tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
                .await
                .unwrap();
            registered.push(id);
        }
        // Written down, not just held in `ctx.config`: the registry `apply`
        // reads is the one under the lock, and a list that only ever existed in
        // this process's snapshot is the fiction `UserConfig::update`'s own
        // documentation warns about — here it would leave all nine of these
        // unregistered too, and the orphan riabuild named would be whichever
        // directory happened to sort last.
        ctx.update_config(|config| config.claude_accounts = registered)
            .await
            .unwrap();
        let orphan = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&orphan))
            .await
            .unwrap();

        let error = ClaudeAccounts
            .apply(&mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&orphan), "{error}");
        assert!(error.contains("riabuild claude delete"), "{error}");
        assert_eq!(ctx.config.claude_accounts.len(), accounts::MAX);
    }

    #[tokio::test]
    async fn an_account_whose_directory_vanished_is_dropped() {
        let (mut ctx, _home, id) = ready().await;
        let gone = accounts::new_id();
        ctx.config.claude_accounts.push(gone.clone());

        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, vec![id]);
    }

    /// A second riabuild's edit to the account list survives this task.
    ///
    /// The lost update this task used to have: `apply` built the whole list
    /// from `ctx.config` — the snapshot this process read at start — and handed
    /// it to `update_config`, which reloads under the lock precisely so there
    /// is no stale snapshot to merge. A closure that ignores what it is handed
    /// throws that away, and **position is the number**: the developer who ran
    /// `riabuild claude primary 2` in the other window found `claude-1` opening
    /// their old account again after the next `riabuild`.
    #[tokio::test]
    async fn applying_keeps_the_order_another_terminal_chose() {
        let (mut ctx, _home) = ctx_with_claude(signed_in()).await;
        let first = accounts::new_id();
        let second = accounts::new_id();
        for id in [&first, &second] {
            tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(id))
                .await
                .unwrap();
        }
        let registered = vec![first.clone(), second.clone()];
        ctx.update_config(|config| config.claude_accounts = registered)
            .await
            .unwrap();

        // The other window: `riabuild claude primary 2`.
        let promoted = second.clone();
        riabuild_paths::config::UserConfig::update(ctx.paths.as_ref(), |config| {
            accounts::promote_id(config, &promoted);
        })
        .await
        .unwrap();
        assert_eq!(
            ctx.config.claude_accounts,
            vec![first.clone(), second.clone()],
            "this process must still be holding its stale snapshot"
        );

        ClaudeAccounts.apply(&mut ctx).await.unwrap();

        assert_eq!(ctx.config.claude_accounts, vec![second, first]);
    }

    /// And an account that terminal had registered but not yet made a directory
    /// for is not dropped.
    ///
    /// `accounts::command::new` registers first and creates the directory
    /// second, deliberately — so this window is the ordinary one, not an exotic
    /// one. Rebuilding the list from disk plus a stale snapshot deleted the
    /// registration outright, leaving `claude new` to report an account number
    /// nothing had.
    #[tokio::test]
    async fn applying_keeps_an_account_another_terminal_had_only_just_registered() {
        let (mut ctx, _home, id) = ready().await;
        let added = accounts::new_id();
        let registered = added.clone();
        riabuild_paths::config::UserConfig::update(ctx.paths.as_ref(), |config| {
            config.claude_accounts.push(registered);
        })
        .await
        .unwrap();

        ClaudeAccounts.apply(&mut ctx).await.unwrap();

        assert_eq!(ctx.config.claude_accounts, vec![id, added]);
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home) = ctx_with_claude(signed_in()).await;
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        let first = ctx.config.claude_accounts.clone();
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, first);
    }

    #[tokio::test]
    async fn a_sign_in_with_nobody_to_finish_it_refuses_rather_than_waiting() {
        // `claude auth login` opens a browser and waits for a round trip to
        // complete. With no terminal there is nobody to complete it, and the
        // command does not fail — it sits there. On CI that was a job killed by
        // its own 30-minute timeout with nothing on stdout to explain it.
        let (mut ctx, _home) = ctx_with_claude(FakeRunner::new()).await;
        let runner = Arc::new(installed().with(STATUS, 1, r#"{"loggedIn":false}"#, ""));
        ctx.runner = runner.clone();
        // What `ctx_with` builds, and what a CI job has.
        assert!(!ctx.ui.interactive());

        let error = ClaudeAccounts.apply(&mut ctx).await.unwrap_err();

        let failure = error
            .downcast_ref::<Failure>()
            .expect("a machine with no terminal is not a riabuild bug");
        assert!(failure.action.contains("from a terminal"), "{failure:?}");
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("auth login")),
            "{:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn an_abandoned_sign_in_is_not_treated_as_success() {
        // Claude Code exits non-zero when the browser is closed. A task that
        // ignored that would report a machine that is ready and is not.
        let (mut ctx, _home) = ctx_with_claude(
            installed()
                .with(STATUS, 1, r#"{"loggedIn":false}"#, "")
                .with("claude auth login", 1, "", ""),
        )
        .await;
        // A terminal, so this reaches the sign-in at all: `sign_in` refuses
        // outright without one, which is a different test above.
        ctx.ui = riabuild_ui::Ui::scripted([]);
        let error = ClaudeAccounts
            .apply(&mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("signing you in to Claude Code"), "{error}");
    }

    #[tokio::test]
    async fn a_sign_in_state_riabuild_cannot_read_does_not_open_a_browser() {
        // `installed()` alone leaves `auth status --json` unstubbed, so the
        // answer will not parse. Treating that as a sign-out would open a
        // browser on every single run of a machine riabuild cannot read.
        let (mut ctx, _home) = ctx_with_claude(FakeRunner::new()).await;
        let runner = Arc::new(installed());
        ctx.runner = runner.clone();

        let error = ClaudeAccounts
            .apply(&mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("reading whether your Claude Code account is signed in"),
            "{error}"
        );
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("auth login")),
            "{:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn a_signed_in_account_is_never_sent_through_a_browser() {
        let (mut ctx, _home) = ctx_with_claude(FakeRunner::new()).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("auth login")),
            "{:?}",
            runner.calls()
        );
    }
}

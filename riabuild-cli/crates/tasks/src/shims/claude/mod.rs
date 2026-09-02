//! Which Claude Code launchers `~/.riabuild/bin` holds.
//!
//! One per account and a `claude` for the first, reconciled against the
//! account list on every run — written where an account gained one, removed
//! where an account went away. What each of them *says* is `launcher`.

mod launcher;

pub(super) use launcher::handoff;
pub use launcher::{Checkouts, checkout_for, launcher_script};

use super::write_script;
use crate::Ctx;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub async fn write_all(ctx: &Ctx) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;

    // Resolved before the first write, for the reason
    // `provision::write_launchers_with` resolves it before the shims: a run
    // that cannot say where its own binary is must fail rather than write nine
    // good launchers and one that names nothing.
    let riabuild = super::running_binary()?;
    let claude = ctx.claude();
    let settings = ctx.paths.org_settings_file();
    let ids = &ctx.config.claude_accounts;
    // Every checkout this machine knows a path for, plus this run's own
    // default — what `--cwd` falls back to when a developer is standing in
    // neither. Read here rather than inside the loop because every account's
    // launcher gets the same list: which repositories this machine knows
    // about is a fact about the machine, not about a sign-in.
    let (checkouts, default) = known_checkouts(ctx);

    // Landed by rename, like every other file riabuild generates. Launcher
    // content is deterministic given the account list, so two concurrent
    // writers agree and no lock is needed — the hazard is only an interrupt
    // landing mid-write, which leaves a truncated `claude-2` that fails with a
    // shell syntax error. `write_script` is what guarantees that.
    for (index, id) in ids.iter().enumerate() {
        // Only an account the developer marked gets a spool path, and an
        // account that loses the mark gets a launcher without one on the next
        // run — which is drift `check()` already sees, because the value is on
        // the exec line like every other. See `UserConfig::tracked_accounts`.
        let spool = ctx
            .config
            .tracked_accounts
            .contains(id)
            .then(|| ctx.paths.usage_spool_file(id));
        let script = launcher_script(
            &riabuild,
            &ctx.paths.claude_profile_dir(id),
            &claude,
            &settings,
            &bin,
            Checkouts {
                all: &checkouts,
                default: default.as_deref(),
            },
            spool.as_deref(),
        );
        write_script(&bin, &format!("claude-{}", index + 1), &script).await?;
        if index == 0 {
            write_script(&bin, "claude", &script).await?;
        }
    }

    prune(&bin, ids.len()).await?;
    Ok(())
}

/// Every checkout `UserConfig::repos` knows a path for — the whole reason
/// `--cwd` can be scoped per checkout instead of to one repository for the
/// whole machine — plus this run's own default (`Ctx::project_dir`), added if
/// it is not already among them. The default is what a machine that has never
/// used the picker falls back to, through `project_dir`'s own reading of
/// `legacy_checkout`.
///
/// Sorted longest-path-first so a checkout nested inside another — unusual,
/// but not something the launcher may assume away — matches the more specific
/// one; deduplicated so a repository that is also the default is not tested
/// twice for no reason.
fn known_checkouts(ctx: &Ctx) -> (Vec<PathBuf>, Option<PathBuf>) {
    let default = ctx.project_dir();
    let home = ctx.paths.home();
    let mut checkouts: Vec<PathBuf> = ctx
        .config
        .repos
        .values()
        .map(|path| riabuild_paths::expand_tilde(path, &home))
        .collect();
    if let Some(default) = &default
        && !checkouts.contains(default)
    {
        checkouts.push(default.clone());
    }
    checkouts.sort_by(|a, b| {
        b.as_os_str()
            .len()
            .cmp(&a.as_os_str().len())
            .then_with(|| a.cmp(b))
    });
    checkouts.dedup();
    (checkouts, default)
}

/// Removes launchers that no longer name an account.
///
/// A file that was never there is the state being asked for, so
/// `NotFound` is swallowed. Anything else — `EPERM`, a read-only mount — is
/// a real failure: silently swallowing it would leave an orphan launcher
/// behind unreported.
async fn prune(bin: &Path, count: usize) -> Result<()> {
    // `c` is what riabuild called the launcher before accounts existed.
    remove_if_present(&bin.join("c")).await?;
    if count == 0 {
        remove_if_present(&bin.join("claude")).await?;
    }
    for number in count + 1..=crate::accounts::MAX {
        remove_if_present(&bin.join(format!("claude-{number}"))).await?;
    }
    Ok(())
}

async fn remove_if_present(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;
    use crate::testing::ctx_with;
    use riabuild_api::Repo;
    use riabuild_runner::FakeRunner;

    /// `known_checkouts` is what turns `UserConfig::repos` — a machine's whole
    /// map of checkouts — into the list `build_agents_view` matches `$PWD`
    /// against, so this is the seam where a second repository either does or
    /// does not get a `--cwd` of its own.
    #[tokio::test]
    async fn known_checkouts_carries_every_repo_this_machine_has_cloned() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.repos.insert(
            "Clubria/riabuild".into(),
            "/home/ada/Clubria/riabuild".into(),
        );
        ctx.config.repos.insert(
            "Clubria/clubria-tenants".into(),
            "/home/ada/Clubria/clubria-tenants".into(),
        );
        ctx.repo = Some(Repo::parse("Clubria/clubria-tenants").unwrap());

        let (checkouts, default) = known_checkouts(&ctx);

        assert_eq!(
            default.as_deref(),
            Some(std::path::Path::new("/home/ada/Clubria/clubria-tenants")),
            "the run's own default is the active repository, unchanged"
        );
        // Both checkouts present — the fix. Before it, only whichever
        // repository this run happened to be about reached the launcher.
        for path in [
            "/home/ada/Clubria/riabuild",
            "/home/ada/Clubria/clubria-tenants",
        ] {
            assert!(
                checkouts.contains(&std::path::PathBuf::from(path)),
                "{checkouts:?} is missing {path}"
            );
        }
        assert_eq!(
            checkouts.len(),
            2,
            "the default must not be duplicated: {checkouts:?}"
        );
    }

    /// A repository this machine knows about, but which is not the run's
    /// default, must not be dropped — the exact map entry the bug report's
    /// `clubria-tenants` checkout would have been.
    #[tokio::test]
    async fn known_checkouts_keeps_a_repository_that_is_not_the_default() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.repos.insert(
            "Clubria/riabuild".into(),
            "/home/ada/Clubria/riabuild".into(),
        );
        ctx.repo = Some(Repo::parse("Clubria/ai-builders-hub").unwrap());
        ctx.config.repos.insert(
            "Clubria/ai-builders-hub".into(),
            "/home/ada/Clubria/ai-builders-hub".into(),
        );

        let (checkouts, _default) = known_checkouts(&ctx);

        assert!(
            checkouts.contains(&std::path::PathBuf::from("/home/ada/Clubria/riabuild")),
            "{checkouts:?}"
        );
    }

    /// A second repository this machine knows must reach the generated
    /// launcher as its own `--checkout` — the end-to-end proof that `write_all`
    /// wires `known_checkouts` all the way through, not just that the helper
    /// itself computes the right list. Which of them a launch resolves to is
    /// `launcher::checkout_for`'s question and is tested there.
    #[tokio::test]
    async fn a_second_known_repository_reaches_the_written_launcher() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id()];
        ctx.config.repos.insert(
            "Clubria/riabuild".into(),
            "/home/ada/Clubria/riabuild".into(),
        );
        ctx.config.repos.insert(
            "Clubria/clubria-tenants".into(),
            "/home/ada/Clubria/clubria-tenants".into(),
        );
        ctx.repo = Some(Repo::parse("Clubria/clubria-tenants").unwrap());

        write_all(&ctx).await.unwrap();

        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join("claude"))
            .await
            .unwrap();
        assert!(
            script.contains("--checkout '/home/ada/Clubria/riabuild'"),
            "{script}"
        );
        assert!(
            script.contains("--checkout '/home/ada/Clubria/clubria-tenants'"),
            "{script}"
        );
        // And the run's own repository is the fallback for a developer standing
        // in neither.
        assert!(
            script.contains("--default-checkout '/home/ada/Clubria/clubria-tenants'"),
            "{script}"
        );
    }

    #[tokio::test]
    async fn every_account_gets_a_launcher_and_the_first_gets_two() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let ids = vec![accounts::new_id(), accounts::new_id(), accounts::new_id()];
        ctx.config.claude_accounts = ids.clone();

        write_all(&ctx).await.unwrap();
        // Safe to run twice, like every other apply().
        write_all(&ctx).await.unwrap();

        let bin = ctx.paths.bin_dir();
        for (index, id) in ids.iter().enumerate() {
            let script = tokio::fs::read_to_string(bin.join(format!("claude-{}", index + 1)))
                .await
                .unwrap();
            assert!(script.contains(id.as_str()), "claude-{}", index + 1);
        }
        let primary = tokio::fs::read_to_string(bin.join("claude")).await.unwrap();
        assert!(primary.contains(ids[0].as_str()), "{primary}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(bin.join("claude"))
                .await
                .unwrap()
                .permissions()
                .mode();
            // A dropped `make_executable` reads as "permission denied" on a
            // developer's laptop, not as a test failure in CI.
            assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
        }
    }

    #[tokio::test]
    async fn launchers_for_accounts_that_are_gone_are_removed() {
        // An orphan is worse than a missing shim: it points at a deleted
        // directory, so Claude Code makes it afresh, asks for a login, and
        // leaves an account no riabuild command can see.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id(), accounts::new_id()];
        write_all(&ctx).await.unwrap();

        let bin = ctx.paths.bin_dir();
        // An older riabuild's launcher, and a third account since deleted.
        tokio::fs::write(bin.join("c"), "#!/bin/sh\n")
            .await
            .unwrap();
        tokio::fs::write(bin.join("claude-3"), "#!/bin/sh\n")
            .await
            .unwrap();

        ctx.config.claude_accounts.truncate(1);
        write_all(&ctx).await.unwrap();

        assert!(tokio::fs::try_exists(bin.join("claude-1")).await.unwrap());
        assert!(!tokio::fs::try_exists(bin.join("claude-2")).await.unwrap());
        assert!(!tokio::fs::try_exists(bin.join("claude-3")).await.unwrap());
        assert!(!tokio::fs::try_exists(bin.join("c")).await.unwrap());

        // Deleting the last account must take the primary `claude` launcher
        // with it — the `count == 0` branch of `prune`, otherwise untested.
        ctx.config.claude_accounts.clear();
        write_all(&ctx).await.unwrap();
        assert!(!tokio::fs::try_exists(bin.join("claude")).await.unwrap());
        assert!(!tokio::fs::try_exists(bin.join("claude-1")).await.unwrap());
    }
}

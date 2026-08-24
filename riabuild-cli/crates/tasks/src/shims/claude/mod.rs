//! Which Claude Code launchers `~/.riabuild/bin` holds.
//!
//! One per account and a `claude` for the first, reconciled against the
//! account list on every run — written where an account gained one, removed
//! where an account went away. What each of them *says* is `launcher`.

mod launcher;

pub use launcher::launcher_script;

use super::write_script;
use crate::Ctx;
use anyhow::Result;
use std::path::Path;

pub async fn write_all(ctx: &Ctx) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;

    let claude = ctx.claude();
    let settings = ctx.paths.org_settings_file();
    let ids = &ctx.config.claude_accounts;
    // The checkout the agents view opens on. Read here rather than inside the
    // loop because every account's launcher gets the same one: which repository
    // this machine is set up for is a fact about the machine, not about a
    // sign-in. `None` on a machine with no checkout yet, and the launcher then
    // opens the view exactly as it did before the flag existed.
    let project = ctx.project_dir();

    // Landed by rename, like every other file riabuild generates. Launcher
    // content is deterministic given the account list, so two concurrent
    // writers agree and no lock is needed — the hazard is only an interrupt
    // landing mid-write, which leaves a truncated `claude-2` that fails with a
    // shell syntax error. `write_script` is what guarantees that.
    for (index, id) in ids.iter().enumerate() {
        let script = launcher_script(
            &ctx.paths.claude_profile_dir(id),
            &claude,
            &settings,
            &bin,
            project.as_deref(),
        );
        write_script(&bin, &format!("claude-{}", index + 1), &script).await?;
        if index == 0 {
            write_script(&bin, "claude", &script).await?;
        }
    }

    prune(&bin, ids.len()).await?;
    Ok(())
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
    use riabuild_runner::FakeRunner;

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

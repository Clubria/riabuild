//! `~/.riabuild/.provision.lock` — the lock every command that changes the
//! tree takes, so two riabuilds on one machine take turns.
//!
//! It lived in `provision.rs` and was held by the provisioning run alone,
//! which is only half of what it is for. The two commands that *destroy*
//! things — `riabuild reset` and `riabuild move-project` — took nothing at
//! all, so a reset in one window could remove the tree a run in another was
//! unpacking Node into, and a move could rename the checkout out from under a
//! task holding it open. Those are the runs the protocol was written for, so
//! the helper lives here rather than inside the one flow that had it first.
//!
//! Design: `../docs/superpowers/specs/2026-08-12-concurrent-runs-design.md`.

use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_paths::filelock::FileLock;
use riabuild_ui as ui;
use riabuild_ui::Ui;

/// The lock a run holds while it changes this machine, or `None` under
/// `--check`.
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
///
/// Takes `paths`, `ui` and the flag rather than a `Ctx`, because `riabuild
/// reset` is dispatched before a `Ctx` exists at all — deliberately, since the
/// state a `Ctx` would load may be the reason someone is resetting.
pub(crate) async fn provisioning_lock(
    paths: &dyn Paths,
    ui: &Ui,
    dry_run: bool,
) -> Result<Option<FileLock>> {
    if dry_run {
        return Ok(None);
    }
    let path = paths.provision_lock_file();
    // The callback borrows the `Ui` rather than owning it — `Ui` is not `Clone`,
    // and does not need to be for a line printed before the wait begins.
    let lock = FileLock::acquire(&path, || {
        ui.info("Waiting for the riabuild already setting up this machine…");
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

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;
    use tempfile::TempDir;

    /// `--check` changes nothing, so it must never make a second window wait.
    #[tokio::test]
    async fn a_dry_run_takes_no_provisioning_lock() {
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());

        let taken = provisioning_lock(&paths, &Ui::new(true), true)
            .await
            .expect("dry run");

        assert!(
            taken.is_none(),
            "a run that promises to change nothing must not hold the provisioning lock"
        );
    }

    /// Dropping the guard is what every holder does before it hands the
    /// terminal over, so a second window has to find the lock free immediately
    /// afterwards.
    #[tokio::test]
    async fn a_real_run_takes_the_lock_and_releases_it_when_dropped() {
        let home = TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());

        let taken = provisioning_lock(&paths, &Ui::new(true), false)
            .await
            .expect("acquire");
        assert!(taken.is_some(), "a real run holds the lock across its work");
        drop(taken);

        assert!(
            FileLock::try_acquire(&paths.provision_lock_file())
                .await
                .expect("try")
                .is_some(),
            "the next window must find it free"
        );
    }
}

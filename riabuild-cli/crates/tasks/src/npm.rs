//! The one piece of npm's internals riabuild has to know about.
//!
//! npm does not install a package over the top of the old one. It *retires* the
//! old directory first — renames it aside, extracts the new one, runs the
//! lifecycle scripts, and only then deletes what it moved. The name it renames
//! to is not a temporary one:
//!
//! ```text
//! retirePath = from => `${dirname(from)}/.${basename(from)}-${pathSafeHash(from)}`
//! ```
//!
//! where `pathSafeHash` is eight alphanumeric characters of a sha1 **of the
//! absolute path**. So every install of a given package into a given prefix
//! retires it to the same name, every time, for ever.
//!
//! That is fine until an install is interrupted — a closed laptop, a lost
//! network, `INSTALL_PATIENCE` expiring, a `^C` — at which point the retired
//! directory is left on disk with nobody to delete it. npm cannot clean it up
//! on the next run, because cleaning it up is not a step that comes before the
//! rename; the rename *is* the next thing npm does, and it fails:
//!
//! ```text
//! npm error ENOTEMPTY: directory not empty, rename
//!   '…/node_modules/@anthropic-ai/claude-code' -> '…/node_modules/@anthropic-ai/.claude-code-eEXEHRqA'
//! ```
//!
//! Every subsequent `npm install -g` of that package into that prefix fails the
//! same way. The developer's machine is then in a state riabuild can describe
//! perfectly and never repair: whatever `check()` reports, the `apply()` that
//! follows it cannot succeed, so the run ends on "it did not take effect" for
//! ever. `.agents/skills/writing-setup-tasks` calls that out by name — a check
//! that cannot be satisfied by the apply that follows it is worse than no
//! check.
//!
//! So riabuild sweeps the leftovers itself, before every install. What makes
//! that safe rather than reckless is that a retired directory is npm's own
//! scratch space and exists only *during* a reify: one on disk when riabuild is
//! about to start npm is, by construction, from a run that is already over. The
//! provisioning lock is per developer and held for the length of the run, so no
//! second riabuild is inside npm at the same time.

use std::path::{Path, PathBuf};

/// Where `npm install -g --prefix <node_dir>` puts packages.
fn global_modules(node_dir: &Path) -> PathBuf {
    node_dir.join("lib").join("node_modules")
}

/// Whether `name` is `.{package}-{eight alphanumerics}` — npm's retired
/// spelling of `package`, and nothing else.
///
/// Matched exactly rather than as a `.{package}-*` prefix, because the loose
/// version is wrong in a way a test would not notice: `typescript` and
/// `typescript-language-server` are both installed into the same directory, and
/// a prefix match on the first would sweep a live retirement of the second out
/// from under a concurrent npm.
fn is_retired(name: &str, package: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some(hash) = rest.strip_prefix(package) else {
        return false;
    };
    let Some(hash) = hash.strip_prefix('-') else {
        return false;
    };
    hash.len() == 8 && hash.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Removes the retired directories a previous interrupted install of `package`
/// left in `node_dir`, and answers with what it removed.
///
/// `package` is the name npm knows — `@anthropic-ai/claude-code`, `typescript`
/// — never a spec with a version on it.
///
/// Best effort by design. A leftover this cannot delete is one npm is about to
/// name in an `ENOTEMPTY` that every install site already puts in front of the
/// developer, and failing here would replace that precise message with a
/// vaguer one.
pub(crate) async fn clear_retired(node_dir: &Path, package: &str) -> Vec<PathBuf> {
    let installed = global_modules(node_dir).join(package);
    let (Some(parent), Some(basename)) = (
        installed.parent(),
        installed.file_name().and_then(|name| name.to_str()),
    ) else {
        return Vec::new();
    };

    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        // No such directory yet: a prefix nothing has ever been installed into
        // has nothing to sweep.
        return Vec::new();
    };

    let mut cleared = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_retired(name, basename) {
            continue;
        }
        // `remove_dir_all` rather than `remove_dir`: the whole point of a
        // leftover is that it is not empty. A retired *file* — npm retires bin
        // links too — never wedges anything, because a rename over an existing
        // file succeeds on POSIX; only a directory earns `ENOTEMPTY`.
        if !entry.path().is_dir() {
            continue;
        }
        if tokio::fs::remove_dir_all(entry.path()).await.is_ok() {
            cleared.push(entry.path());
        }
    }
    cleared
}

/// Removes an installed package outright, so the next `npm install -g` has to
/// extract it again.
///
/// The repair for an install that is *present and unusable* rather than absent
/// or out of date. npm reifies towards a tree it computes from disk, so a
/// package directory sitting at the version npm was going to install is a
/// package npm considers done — it re-extracts nothing and, decisively, re-runs
/// no `postinstall`. For `@anthropic-ai/claude-code` the postinstall is the
/// step that puts the native binary in place of the 500-byte placeholder the
/// tarball ships, so "reinstall it" without this is a no-op against exactly the
/// machine that needs it most.
///
/// Only ever called where riabuild has established that the installed copy
/// cannot be executed. It is a deletion, and `toolchain`'s
/// `a_node_that_will_not_start_is_not_evidence_that_it_is_missing` is the
/// standing reminder of what deleting a tool on a guess costs.
pub(crate) async fn remove_installed(node_dir: &Path, package: &str) -> std::io::Result<()> {
    let installed = global_modules(node_dir).join(package);
    match tokio::fs::remove_dir_all(&installed).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(path, contents).await.unwrap();
    }

    #[test]
    fn npms_retired_spelling_is_recognised() {
        assert!(is_retired(".claude-code-eEXEHRqA", "claude-code"));
        assert!(is_retired(".typescript-AbCd1234", "typescript"));
    }

    #[test]
    fn a_live_package_is_not_mistaken_for_a_leftover() {
        assert!(!is_retired("claude-code", "claude-code"));
        assert!(!is_retired(".package-lock.json", "claude-code"));
        assert!(!is_retired(".bin", "claude-code"));
    }

    #[test]
    fn a_neighbours_leftover_is_left_alone() {
        // The reason `is_retired` counts the hash rather than matching a
        // prefix. Both of these live in the same `node_modules`.
        assert!(!is_retired(
            ".typescript-language-server-AbCd1234",
            "typescript"
        ));
        assert!(!is_retired(".typescript-language-server", "typescript"));
    }

    #[tokio::test]
    async fn an_interrupted_installs_leftover_is_swept() {
        // The wedge itself: with this directory on disk, every `npm install -g
        // @anthropic-ai/claude-code` into this prefix fails `ENOTEMPTY`, so no
        // `apply()` riabuild could write would ever succeed.
        let node = tempfile::TempDir::new().unwrap();
        let scope = global_modules(node.path()).join("@anthropic-ai");
        let retired = scope.join(".claude-code-eEXEHRqA");
        write(&retired.join("bin").join("claude.exe"), "the good binary").await;
        write(&scope.join("claude-code").join("package.json"), "{}").await;

        let cleared = clear_retired(node.path(), "@anthropic-ai/claude-code").await;

        assert_eq!(cleared, vec![retired.clone()]);
        assert!(!tokio::fs::try_exists(&retired).await.unwrap());
        // The install itself is npm's to replace, not this function's to
        // delete.
        assert!(
            tokio::fs::try_exists(scope.join("claude-code"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_prefix_with_nothing_in_it_sweeps_nothing() {
        let node = tempfile::TempDir::new().unwrap();
        assert!(
            clear_retired(node.path(), "@anthropic-ai/claude-code")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn removing_an_install_leaves_the_prefix_usable() {
        let node = tempfile::TempDir::new().unwrap();
        let modules = global_modules(node.path());
        write(
            &modules.join("@anthropic-ai/claude-code/package.json"),
            "{}",
        )
        .await;
        write(&modules.join("@openai/codex/package.json"), "{}").await;

        remove_installed(node.path(), "@anthropic-ai/claude-code")
            .await
            .unwrap();

        assert!(
            !tokio::fs::try_exists(modules.join("@anthropic-ai/claude-code"))
                .await
                .unwrap()
        );
        // A sibling package shares the prefix and is none of this function's
        // business.
        assert!(
            tokio::fs::try_exists(modules.join("@openai/codex"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn removing_an_install_that_is_not_there_is_not_an_error() {
        // `apply()` has to be safe to run twice.
        let node = tempfile::TempDir::new().unwrap();
        remove_installed(node.path(), "@anthropic-ai/claude-code")
            .await
            .unwrap();
    }
}

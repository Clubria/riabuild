//! Which repository this session is in, as `owner/repo`.
//!
//! riabuild stopped being single-repository when the picker landed, and two
//! repositories mean two checkouts side by side — same tools, same brokered
//! `.env` files, same marker. `(riabuild)` alone answers *which environment is
//! this?* and no longer answers *which of my checkouts am I in?*, which is the
//! question a developer with `payments` in one window and `ai-builders-hub` in
//! another is actually asking.
//!
//! **Read from git, and never from riabuild's own config.** Not a preference:
//! git's answer names the repository the working directory is *in*, including a
//! checkout riabuild never cloned, rather than the one riabuild last recorded.
//!
//! **And read from `.git/config` as a file, not from the `git` binary.** Claude
//! Code re-renders continuously, so a subprocess here is a subprocess per
//! render — a real cost to hang on a marker. Nothing in this file writes, and
//! every failure is `None`: an undecorated label beats no status line.

use std::path::{Path, PathBuf};

/// `owner/repo` for the checkout `dir` sits in, or `None` for anywhere else.
pub(super) fn of(dir: &Path) -> Option<String> {
    let gitdir = common_dir(dir)?;
    // A checkout with no `origin` is a repository riabuild has nothing to say
    // about — better an undecorated marker than a guess from the directory name.
    slug_of(&origin_url(&gitdir)?)
}

/// Walks up from `dir` looking for `.git`, and returns the directory holding the
/// `config` file — which is not always the `.git` it found.
fn common_dir(dir: &Path) -> Option<PathBuf> {
    let mut at = dir.to_path_buf();
    loop {
        let dot = at.join(".git");
        match std::fs::metadata(&dot) {
            Ok(found) if found.is_dir() => return Some(dot),
            // A **linked worktree** has a `.git` *file* naming the real git
            // directory, and that directory holds no `config` of its own —
            // `commondir` points at the one it shares with the main checkout.
            // Worth handling rather than treating as "not a repository": every
            // branch of riabuild's own work happens in one, under
            // `.claude/worktrees/`, which is exactly where a developer most
            // needs telling which repository they are in.
            Ok(found) if found.is_file() => return linked_worktree(&at, &dot),
            // Not here, or not readable. Keep walking.
            _ => {}
        }
        if !at.pop() {
            return None; // reached the filesystem root
        }
    }
}

fn linked_worktree(at: &Path, dot: &Path) -> Option<PathBuf> {
    let named = std::fs::read_to_string(dot).ok()?;
    let gitdir = named
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))
        .map(str::trim)?;
    let gitdir = at.join(gitdir);
    match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(shared) => Some(gitdir.join(shared.trim())),
        // No `commondir`: a `.git` file that is not a linked worktree — a
        // submodule is the usual one — so the directory it names is its own.
        Err(_) => Some(gitdir),
    }
}

/// `url` from the `[remote "origin"]` section, by walking the INI rather than
/// parsing it: this needs one value out of one section, and every other key in
/// the file is somebody else's business.
fn origin_url(gitdir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(gitdir.join("config")).ok()?;
    let mut in_origin = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|rest| rest.split(']').next())
        {
            // `[remote "origin"]`, with git's own tolerance for whitespace
            // between the two halves and none for a section merely starting
            // with the word — `[remotes "origin"]` is not this one.
            in_origin = section
                .trim()
                .strip_prefix("remote")
                .is_some_and(|rest| rest.trim() == "\"origin\"");
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            let url = value.trim();
            return (!url.is_empty()).then(|| url.to_string());
        }
    }
    None
}

/// `owner/repo` out of any spelling git records a remote in —
/// `git@host:o/r.git`, `https://host/o/r.git`, `ssh://git@host/o/r`.
///
/// The last two segments are the answer in all of them, which is why this takes
/// the tail rather than trying to know the shape of a URL.
fn slug_of(url: &str) -> Option<String> {
    let trimmed = url
        .strip_suffix(".git")
        .unwrap_or(url)
        .trim_end_matches('/');
    let (head, repo) = trimmed.rsplit_once('/')?;
    // `git@host:owner` and `https://host/owner` both end in the owner; the
    // separator before it is `:` in one and `/` in the other.
    let owner = head.rsplit(['/', ':']).next()?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains(':'))
        .then(|| format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statusline::testing::{checkout, worktree, write};

    #[test]
    fn a_checkout_names_its_origin() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        assert_eq!(of(&dir).as_deref(), Some("Clubria/payments"));
    }

    /// A linked worktree's `.git` is a *file* with no `config` behind it, so a
    /// walk that only recognises a `.git` **directory** finds nothing there and
    /// reports "not a repository" — in the one place a developer most needs
    /// telling which repository they are in. Following the `gitdir:` line to
    /// `commondir` is what makes it resolve.
    ///
    /// The worktree here sits **beside** the checkout rather than under it, and
    /// that placement is the test. riabuild's own live in `.claude/worktrees/`,
    /// physically inside the checkout — where the walk upwards reaches the main
    /// `.git` on its own and returns the right answer for the wrong reason. A
    /// fixture in that shape passes with the `commondir` branch deleted, which
    /// makes it no coverage of the branch at all.
    #[test]
    fn a_linked_worktree_resolves_to_the_repository_it_belongs_to() {
        let home = tempfile::TempDir::new().unwrap();
        let main = home.path().join("riabuild");
        checkout(&main, "https://github.com/Clubria/riabuild.git");
        let tree = home.path().join("elsewhere").join("feature");
        worktree(&main, &tree, "feature");

        assert_eq!(of(&tree).as_deref(), Some("Clubria/riabuild"));
    }

    #[test]
    fn a_directory_below_the_checkout_is_still_in_the_repository() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");
        let deep = dir.join("crates").join("api").join("src");
        std::fs::create_dir_all(&deep).unwrap();

        assert_eq!(of(&deep).as_deref(), Some("Clubria/payments"));
    }

    /// Every spelling git records a remote in has to reach the same answer —
    /// this is the whole reason `slug_of` takes the tail instead of parsing a
    /// URL.
    #[test]
    fn every_spelling_of_a_remote_names_the_same_repository() {
        for url in [
            "git@github.com:Clubria/payments.git",
            "git@github.com:Clubria/payments",
            "https://github.com/Clubria/payments.git",
            "https://github.com/Clubria/payments",
            "https://github.com/Clubria/payments/",
            "ssh://git@github.com/Clubria/payments.git",
        ] {
            let home = tempfile::TempDir::new().unwrap();
            let dir = home.path().join("payments");
            checkout(&dir, url);

            assert_eq!(of(&dir).as_deref(), Some("Clubria/payments"), "{url}");
        }
    }

    /// The INI walk has to stay inside `[remote "origin"]`. A repository with a
    /// fork remote first would otherwise be named after the fork.
    #[test]
    fn a_second_remote_does_not_get_mistaken_for_origin() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        write(
            &dir.join(".git").join("config"),
            "[remote \"upstream\"]\n\turl = git@github.com:Someone/fork.git\n\
             [remote \"origin\"]\n\turl = git@github.com:Clubria/payments.git\n",
        );

        assert_eq!(of(&dir).as_deref(), Some("Clubria/payments"));
    }

    #[test]
    fn a_checkout_with_no_origin_has_no_name() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("scratch");
        write(&dir.join(".git").join("config"), "[core]\n\tbare = false\n");

        assert_eq!(of(&dir), None);
    }

    #[test]
    fn somewhere_that_is_not_a_checkout_has_no_name() {
        let home = tempfile::TempDir::new().unwrap();

        assert_eq!(of(home.path()), None);
    }

    /// A commented-out remote is not a remote.
    #[test]
    fn a_commented_url_is_not_read() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        write(
            &dir.join(".git").join("config"),
            "[remote \"origin\"]\n\t# url = git@github.com:Someone/wrong.git\n\
             \turl = git@github.com:Clubria/payments.git\n",
        );

        assert_eq!(of(&dir).as_deref(), Some("Clubria/payments"));
    }
}

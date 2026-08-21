//! The `.env.<environment>` file itself: what it is called, whether it
//! parses, whether git would commit it, and how a new one lands.
//!
//! Everything here is about the file rather than about the pull. It is
//! separate because each of these is a property `check()` asserts and
//! `apply()` has to establish, and because `ensure_ignored` is the mechanism
//! for anything riabuild leaves in a developer's checkout rather than for
//! secrets alone.

use crate::Ctx;
use anyhow::Result;
use riabuild_runner::RunOptions;
use std::path::{Path, PathBuf};

/// True if the text is a readable dotenv file with at least one assignment.
pub fn parses_as_dotenv(text: &str) -> bool {
    let mut assignments = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim().trim_start_matches("export ").trim();
        if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return false;
        }
        assignments += 1;
    }
    assignments > 0
}

/// The filename an environment lands in — `dev` becomes `.env.dev`.
///
/// Deriving it here rather than reading it from the reply is deliberate: the
/// server names environments, and the CLI decides what a file is called. A
/// filename chosen on the server would be riabuild-web picking a path on a
/// laptop, which is the same channel "the server ships data, never logic"
/// exists to close.
pub(super) fn env_file_name(environment: &str) -> String {
    format!(".env.{environment}")
}

pub(super) fn env_file(project: &Path, environment: &str) -> PathBuf {
    project.join(env_file_name(environment))
}

pub(super) async fn is_ignored(ctx: &Ctx, project: &Path, file: &str) -> Result<bool> {
    let output = ctx
        .runner
        .run(
            "git",
            &["-C", &project.to_string_lossy(), "check-ignore", "-q", file],
            &RunOptions::default(),
        )
        .await?;
    Ok(output.ok())
}

/// Adds one riabuild-written file to `.git/info/exclude` rather than `.gitignore`.
///
/// `.gitignore` is a tracked file: editing it would dirty every developer's
/// checkout and show up in their next diff — and it belongs to the repository
/// being cloned, which is not riabuild's to edit. `info/exclude` is local,
/// private and does exactly the same job.
///
/// One line per file, never a `.env.*` glob — the glob would also hide a
/// tracked `.env.example`, which is a file a repository is entitled to have.
///
/// `pub(crate)` because this is the mechanism for *anything* riabuild leaves in
/// a developer's checkout, not only the secrets files: `project` uses it for
/// the `.riabuild-owner` marker. Named here rather than moved because this is
/// where the reasoning above was worked out, and the `.env.<environment>` files
/// are still its main use.
pub(crate) async fn ensure_ignored(ctx: &mut Ctx, project: &Path, file: &str) -> Result<()> {
    if is_ignored(ctx, project, file).await? {
        return Ok(());
    }
    let exclude = project.join(".git").join("info").join("exclude");
    if let Some(parent) = exclude.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut contents = tokio::fs::read_to_string(&exclude)
        .await
        .unwrap_or_default();
    if !contents.lines().any(|line| line.trim() == file) {
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(file);
        contents.push('\n');
        tokio::fs::write(&exclude, contents).await?;
    }
    Ok(())
}

/// Lands one `.env.<environment>` full of brokered secrets.
///
/// `config::write_atomic` and not an `OpenOptions` here, because the two things
/// this has to be — private, and whole — are both things opening the target
/// itself gets wrong, and each got wrong in its own way:
///
/// - **Private on every write, not only the first.** `OpenOptions::mode` is the
///   `mode` argument to `open(2)`, which the kernel consults only when `O_CREAT`
///   actually creates. A `.env.dev` the developer had already `touch`ed, or that
///   an older riabuild wrote at the umask, kept its `0644` while being refilled
///   with brokered Infisical secrets — for ever, since nothing ever created it
///   again. `write_atomic` writes a fresh `0600` temporary and renames it over
///   the old inode, so the mode is not inherited from whatever was there.
/// - **Whole, not truncate-then-write.** `truncate(true)` opens a window in
///   which the file on disk is empty and then partial, and this is a path other
///   programs read on their own schedule: a `pnpm dev` or a `direnv` that looks
///   during it gets a short env and starts against half the secrets, and an
///   interrupted run leaves that file behind. The old comment here reasoned
///   about tokio's deferred write and answered it with `flush()`, which is a
///   real hazard and the smaller one — a reader can lose the race without
///   riabuild being interrupted at all. A rename has no window: readers see the
///   whole previous file or the whole new one.
///
/// `check()` asserts the mode as well, so a file left loose by a riabuild older
/// than this is repaired rather than merely not made worse.
pub(super) async fn write_private(path: &Path, contents: &str) -> Result<()> {
    riabuild_paths::config::write_atomic(path, contents.as_bytes()).await
}

/// Whether a `.env.<environment>` is readable by anyone but its owner.
///
/// `None` where the question cannot be asked — a filesystem that reports no
/// mode, and every non-unix target. Answering "loose" there would put `check()`
/// into a loop it can never satisfy, which is worse than not checking.
#[cfg(unix)]
pub(super) async fn is_world_or_group_readable(path: &Path) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some(meta.permissions().mode() & 0o077 != 0)
}

#[cfg(not(unix))]
pub(super) async fn is_world_or_group_readable(_path: &Path) -> Option<bool> {
    None
}

//! Assembling the envelope a run executes inside, before its `Ctx` exists.
//!
//! Two things belong here and they are one subject: whether this invocation
//! may claim the GitHub-session marker, and — where it is a remote scope — the
//! session directory and the scoped runner that carry `GH_CONFIG_DIR` and
//! `GIT_CONFIG_GLOBAL` into every child. `main::run` is left with the wiring
//! it is named for.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use riabuild_gh_session as gh_session;
use riabuild_paths::Paths;
use riabuild_runner::{self as runner, CommandRunner};
use riabuild_tasks::scope;

use crate::cli::{Cli, Command};

/// Whether this invocation goes on to hold the interactive environment shell
/// open.
///
/// This is the single source of truth `holds_gh_session_marker` defers to,
/// and it is also, deliberately, the exact condition `provision`'s own tail
/// uses to decide whether to call `open_shell` — see the table in
/// `task-19-brief.md:24-30`: `internal gh-sweep`, the seeding run, and the
/// *setup* run (`riabuild --no-shell` on the server, which is what Task 21's
/// `remote::flow::run` sends over its first SSH hop) all answer `false`; only
/// the interactive shell run answers `true`. Getting this wrong the other
/// way — granting a marker to a `--no-shell` run — is what would make the
/// setup run's own exit sweep away a credential `seed_github` had just
/// written on an earlier hop, before the shell ever sees it.
pub(crate) fn opens_shell(cli: &Cli) -> bool {
    match &cli.command {
        Some(Command::Shell) => true,
        // `Channel` and `Reset` return from `run` before a `Ctx` exists, so
        // they never reach here — named anyway rather than swept into a
        // wildcard, so that adding a subcommand that *should* open a shell is
        // a compile error rather than a silently wrong `false`.
        Some(
            Command::Internal { .. }
            | Command::Login
            | Command::Logout
            | Command::Env
            | Command::Paths
            | Command::Remote { .. }
            | Command::MoveProject { .. }
            | Command::Channel { .. }
            | Command::Reset { .. }
            | Command::Claude { .. }
            // `agents` holds the terminal for as long as a shell would, and is
            // deliberately not one: it draws frames rather than handing the
            // terminal to a child, so nothing about the GitHub-session marker
            // an environment shell claims applies to it.
            | Command::Agents { .. }
            | Command::Status,
        ) => false,
        None => !cli.check && !cli.no_shell,
    }
}

/// Whether this invocation is allowed to claim (and later release) the
/// GitHub-session marker `gh_session::open`/`close` guard.
///
/// Only the invocation that goes on to hold the interactive environment
/// shell open should ever do that — see `gh_session`'s module doc. Every
/// other invocation — the hidden `internal` subcommands (`gh-sweep`,
/// `seed-github`), and just as importantly the *setup* run, which is an
/// ordinary default-flow invocation with `--no-shell` set — calls `attach`
/// instead, which never claims or releases anything.
fn holds_gh_session_marker(cli: &Cli) -> bool {
    opens_shell(cli)
}

/// The GitHub-session envelope a remote-scoped run executes inside.
///
/// `dir` is what every child's `GH_CONFIG_DIR` points at; `marker` is the
/// claim only the invocation that goes on to open the interactive shell takes,
/// and the one thing `run` must close on every return.
pub(crate) struct GhScope {
    pub(crate) dir: Option<PathBuf>,
    pub(crate) marker: Option<gh_session::GhSession>,
}

/// Only a remote scope claims a GitHub session — see `gh_session`. This is
/// deliberately unconditional over every subcommand a remote-scoped
/// invocation might run, not just the shell — with one exception.
/// `internal gh-sweep`/`internal seed-github` are short plumbing
/// invocations the laptop runs *before* the interactive shell exists (see
/// [`holds_gh_session_marker`]): if either claimed a marker the same way
/// the shell does, its own exit would find no other marker yet and wipe
/// the GitHub credential moments after `internal seed-github` wrote it —
/// the exact "earlier draft got it backwards" bug `gh_session`'s module
/// doc warns about. Those two only `attach`, which never claims or
/// releases anything.
pub(crate) async fn open_gh_session(
    scope: &scope::Scope,
    cli: &Cli,
    paths: &dyn Paths,
) -> Result<GhScope> {
    if !scope.is_remote() {
        return Ok(GhScope {
            dir: None,
            marker: None,
        });
    }
    let runtime = gh_session::choose_runtime_dir(
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("TMPDIR").ok().as_deref(),
    )
    .await?;
    let member_id = scope::member_id_from_root(paths)?;
    if holds_gh_session_marker(cli) {
        let session = gh_session::GhSession::open(&runtime, &member_id, std::process::id()).await?;
        Ok(GhScope {
            dir: Some(session.config_dir()),
            marker: Some(session),
        })
    } else {
        Ok(GhScope {
            dir: Some(gh_session::GhSession::attach(&runtime, &member_id).await?),
            marker: None,
        })
    }
}

/// The runner every child of a remote-scoped run goes through.
///
/// With no session directory this is the runner it was handed, unchanged: a
/// laptop has no envelope to put a child inside.
pub(crate) fn scoped_runner(
    inner: Arc<dyn CommandRunner>,
    gh_dir: Option<&Path>,
    paths: &dyn Paths,
) -> Arc<dyn CommandRunner> {
    match gh_dir {
        Some(dir) => Arc::new(runner::ScopedRunner::new(
            inner,
            vec![
                ("GH_CONFIG_DIR".into(), dir.to_string_lossy().into_owned()),
                (
                    "GIT_CONFIG_GLOBAL".into(),
                    paths
                        .root()
                        .join("gitconfig")
                        .to_string_lossy()
                        .into_owned(),
                ),
            ],
        )),
        None => inner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Cli` with every field but `command`/`no_shell`/`check` at its
    /// ordinary default, for the marker-predicate tests below — those three
    /// are the only fields `opens_shell` reads.
    fn cli_for(command: Option<Command>, no_shell: bool, check: bool) -> Cli {
        Cli {
            command,
            project: None,
            repo: None,
            check,
            quiet: false,
            no_shell,
        }
    }

    #[test]
    fn internal_plumbing_never_claims_the_gh_session_marker() {
        // This is the fix for the bug described in `gh_session`'s module
        // doc: if `internal seed-github` claimed a marker the same way the
        // interactive shell does, its own exit would wipe the credential it
        // had just written. Reverting `holds_gh_session_marker` to always
        // return `true` reproduces that bug and fails this test.
        assert!(!holds_gh_session_marker(&cli_for(
            Some(Command::Internal {
                action: crate::cli::InternalAction::SeedGithub,
            }),
            false,
            false,
        )));
        assert!(!holds_gh_session_marker(&cli_for(
            Some(Command::Internal {
                action: crate::cli::InternalAction::GhSweep,
            }),
            false,
            false,
        )));
    }

    #[test]
    fn the_setup_run_never_claims_the_gh_session_marker_either() {
        // The critical fix from Task 21's review: the *setup* run — an
        // ordinary default-flow invocation with `--no-shell` set, exactly
        // what `remote::flow::run` sends over its first SSH hop — used to be
        // granted a marker by the old `Command`-only predicate. If it were,
        // its own exit would sweep away the credential `seed_github` had
        // just written on an earlier SSH hop, before the interactive shell
        // (a third, later hop) ever saw it.
        assert!(!holds_gh_session_marker(&cli_for(None, true, false)));
        // `--check` never opens a shell either, for the same reason.
        assert!(!holds_gh_session_marker(&cli_for(None, false, true)));
        assert!(!holds_gh_session_marker(&cli_for(
            Some(Command::Status),
            false,
            false
        )));
    }

    #[test]
    fn every_other_command_still_claims_the_gh_session_marker() {
        assert!(holds_gh_session_marker(&cli_for(None, false, false)));
        assert!(holds_gh_session_marker(&cli_for(
            Some(Command::Shell),
            false,
            false
        )));
    }
}

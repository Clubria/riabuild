//! What a caller may say about a child, and the two rules read off it.
//!
//! `should_subdue` and `directory_for_riabuild` sit beside `RunOptions` rather
//! than inside the runner that consults them, so each is testable without a
//! terminal and without spawning anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use riabuild_theme::Theme;

/// Where a command riabuild runs for itself lands when the call names no
/// directory.
///
/// Not riabuild's own working directory, which is wherever the developer
/// happened to be standing when they typed `riabuild` — and which is the one
/// input riabuild never chose. Tools read that directory. pnpm 11 walks up from
/// it for a `package.json` and, on a `packageManager` field naming another pnpm,
/// downloads that version and hands the command over to it, so `pnpm -v` answers
/// for the *directory* rather than for the binary that was asked; `infisical`
/// looks for `.infisical.json` the same way, and Claude Code reads `.claude/`
/// and `CLAUDE.md`. A version probe that inherits is asking the wrong question,
/// and a `check()` built on it reports drift the `apply()` after it cannot
/// repair — which is a hard error on a machine with nothing wrong with it, on
/// every run, until the developer thinks to stand somewhere else.
///
/// The root is the only directory that can promise no manifest above it. Nothing
/// under `$HOME` — `~/.riabuild` included — is more than one stray
/// `package.json` away from the same bug.
const FILESYSTEM_ROOT: &str = "/";

/// The bound a call gets when it names none.
///
/// A ceiling rather than a deadline: nothing riabuild captures the output of
/// takes ten minutes, so a call that reaches this has hung rather than run
/// long. Tighten it where a developer is waiting on the answer — the
/// repository listing behind the picker gives GitHub eight seconds — and reach
/// for `None` only where a child genuinely has no bound.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Where the child runs.
    ///
    /// `None` does **not** mean "inherit". For every method but
    /// `run_interactive` it means riabuild chose no directory, and the child
    /// gets [`FILESYSTEM_ROOT`] rather than whatever directory riabuild itself
    /// was started in. Naming one is still how a command that genuinely belongs
    /// to a directory — `infisical export` in the checkout — gets there.
    ///
    /// `run_interactive` is the exception, and for the reason `CLAUDE.md` gives
    /// for it being the exception to the async-IO rule: it is a handoff. There
    /// the developer's own directory is the right answer, so `None` inherits.
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Fed to the child's stdin. Used to pipe brokered secrets without them ever
    /// appearing in a process argument list, where `ps` would show them — and on
    /// a shared server `ps` shows other developers' processes — and to hand a
    /// clipboard write to `xclip -i`.
    ///
    /// Bytes rather than a `String`, for the same reason `run_bytes` exists:
    /// nothing piped through here (a tarball, a token, a PNG) is guaranteed to
    /// be UTF-8, and a `String` cannot represent a PNG at all — so an image
    /// write would not be merely lossy, it would be unconstructible.
    pub stdin: Option<Vec<u8>>,
    /// Run this child under a pty riabuild owns, discard everything it draws
    /// *with*, and print what is left one dimmed line at a time.
    ///
    /// Honoured by `run_interactive` only. The capturing methods never reach a
    /// terminal, so there is nothing there to subdue.
    ///
    /// A `Theme` rather than a `bool` for the reason `CLAUDE.md` gives for the
    /// text a generated rcfile prints: the palette is resolved on the side that
    /// has a `Ui` and passed to the side that does not. `runner/` has no `Ui`
    /// and must not grow one. `Theme::plain()` is a legitimate value — line
    /// discipline with no dim — which is what a `NO_COLOR` run produces without
    /// a special case anywhere.
    pub subdued: Option<Theme>,
    /// How long riabuild waits for the child before killing it.
    ///
    /// `gh`, `git`, `node`, `ssh` and `apt` can all wait for ever — on a
    /// prompt nobody will answer, a lock nobody will release, a socket nobody
    /// will close — and riabuild runs on a current-thread runtime, so one of
    /// them waiting is the whole provisioner waiting, with no output and no
    /// error to send anyone. That is the failure this codebase is written
    /// against, and the bound belongs at the layer every subprocess already
    /// goes through: an ad-hoc `tokio::time::timeout` at one call site is a
    /// rule with no enforcement.
    ///
    /// Honoured by `run`, `run_bytes` and `run_forking` — the calls riabuild
    /// waits out. Not by `spawn` or `spawn_piped`, which hand back a child
    /// meant to outlive the call, and not by `run_interactive`, whose child
    /// runs for as long as the developer keeps typing into it: a bound on
    /// either would be riabuild ending a session it had given away.
    ///
    /// Defaults to [`DEFAULT_TIMEOUT`].
    pub timeout: Option<Duration>,
}

impl Default for RunOptions {
    /// Written out rather than derived, because the one field whose default is
    /// not "nothing" is the point: `RunOptions::default()` is how almost every
    /// call site in the workspace is spelled, and a derived `None` would leave
    /// them all unbounded.
    fn default() -> Self {
        Self {
            cwd: None,
            env: Vec::new(),
            stdin: None,
            subdued: None,
            timeout: Some(DEFAULT_TIMEOUT),
        }
    }
}

/// Whether this call actually gets a pty.
///
/// Split out from `run_interactive` so the rule is testable without a terminal,
/// the same reason `theme::depth_for` is split out from `Theme::detect`.
///
/// With no terminal the flag is ignored outright. That is not a convenience: an
/// unattended run must not take a different code path from an attended one for
/// a *cosmetic* reason, and a pty allocated where no terminal exists would be
/// riabuild inventing a tty for a child that correctly concluded there wasn't
/// one.
pub fn should_subdue(is_terminal: bool, subdued: Option<Theme>) -> Option<Theme> {
    is_terminal.then_some(subdued).flatten()
}

/// Which directory a command riabuild runs for itself actually gets.
///
/// Split out from `RealRunner::for_riabuild` so the rule is testable without
/// spawning anything, the same reason `should_subdue` above is split out from
/// `run_interactive` — and so that a test double can resolve a probe's directory
/// the way the real runner does instead of guessing. A double that guesses is
/// how this went unnoticed in the first place: `FakeRunner` keys its stubs on
/// the invocation, and the invocation is identical whichever directory the child
/// ran in.
///
/// Note what is *not* a parameter: riabuild's own working directory. There is no
/// argument that could reintroduce it.
pub fn directory_for_riabuild(cwd: Option<&Path>) -> &Path {
    cwd.unwrap_or(Path::new(FILESYSTEM_ROOT))
}

#[cfg(test)]
mod subdued_tests {
    use super::*;
    use crate::{CommandRunner, FakeRunner, ScopedRunner};
    use std::sync::Arc;

    #[test]
    fn no_terminal_means_no_subduing_whatever_the_caller_asked_for() {
        // CI, `cargo test`, a pipe. An unattended run must not take a
        // different code path from an attended one for a cosmetic reason.
        assert_eq!(should_subdue(false, Some(Theme::plain())), None);
    }

    #[test]
    fn a_terminal_and_a_theme_is_the_only_combination_that_subdues() {
        assert_eq!(
            should_subdue(true, Some(Theme::plain())),
            Some(Theme::plain())
        );
        assert_eq!(should_subdue(true, None), None);
        assert_eq!(should_subdue(false, None), None);
    }

    #[test]
    fn the_default_run_is_not_subdued() {
        // Every existing call site constructs this way, and none of them
        // changes behaviour because this field was added.
        assert_eq!(RunOptions::default().subdued, None);
    }

    #[tokio::test]
    async fn the_stub_records_which_commands_were_subdued() {
        let runner = FakeRunner::new();
        runner
            .run_interactive(
                "sudo",
                &["apt-get", "update"],
                &RunOptions {
                    subdued: Some(Theme::plain()),
                    ..Default::default()
                },
            )
            .await
            .expect("interactive run");
        runner
            .run_interactive("bash", &["-l"], &RunOptions::default())
            .await
            .expect("interactive run");

        assert_eq!(runner.calls().len(), 2);
        assert_eq!(runner.subdued_calls(), vec!["sudo apt-get update"]);
    }

    #[tokio::test]
    async fn a_scope_carries_the_subdued_flag_through() {
        // `ScopedRunner::merge` clones the options; a field it forgot would be
        // silently dropped for every task that runs under a scope, which is
        // every task that touches `gh`.
        let inner = Arc::new(FakeRunner::new());
        let scoped = ScopedRunner::new(inner.clone(), vec![("K".into(), "V".into())]);
        scoped
            .run_interactive(
                "gh",
                &["auth", "login"],
                &RunOptions {
                    subdued: Some(Theme::plain()),
                    ..Default::default()
                },
            )
            .await
            .expect("interactive run");

        assert_eq!(inner.subdued_calls(), vec!["gh auth login"]);
    }
}

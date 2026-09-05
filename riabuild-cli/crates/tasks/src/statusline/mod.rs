//! The line Claude Code draws under the prompt, and the usage sample it spools.
//!
//! `~/.riabuild/claude-statusline` is a one-line `exec` into `riabuild internal
//! statusline`, so this is what actually runs on every render. It was five
//! hundred lines of JavaScript until 2026-09-05, run on the Node riabuild
//! installs; the reasoning for moving it is the reasoning in
//! [`2026-08-28-launchers-in-rust-design.md`] applied to the one generated file
//! that still held logic — and it is the same three sentences. There is no type
//! checker in that language either. Every test of it needed a subprocess, an
//! interpreter on `PATH` and a temporary copy of the shipped bytes. And its
//! mistakes produced a *different working status line* rather than an error: a
//! renamed key in Claude Code's own state file takes the email off the line, an
//! `?.` on the wrong side of a `??` draws `undefined%`, and a status line whose
//! command fails renders as **no status line at all** — the failure this file
//! has already shipped twice, and the one nothing on the render path announces.
//!
//! What is kept from the JavaScript, because it is what made it correct:
//!
//! **Everything is derived from `CLAUDE_CONFIG_DIR`, never from `Paths`.** This
//! process is started by Claude Code, not by a provisioning run, and on a server
//! one Unix account holds several developers. The launcher sets
//! `CLAUDE_CONFIG_DIR = <root>/claude/<uuid>` on the session's environment, so
//! `basename` is the account and the grandparent is that developer's root — the
//! same derivation is right on a laptop and on a box two colleagues share, and
//! neither needs this to know which of them it is serving. A `Paths` here would
//! answer for whoever the process happens to belong to.
//!
//! **Files are read as files.** `git` and `claude auth status --json` both
//! answer these questions authoritatively and both cost a subprocess. Claude
//! Code re-renders continuously and debounces at 300ms, cancelling the
//! in-flight render when a newer one supersedes it, so everything here reads
//! `.git/config`, `config.json` and `.claude.json` directly. That is a weaker
//! source, deliberately, and the weakness is bounded: a key that moves costs
//! one clause on one line and leaves everything else drawn.
//!
//! **Nothing may cost the developer their status line.** Every fallible part is
//! isolated: an unparseable payload still leaves a marker, a directory that is
//! not a checkout still leaves a marker, and a spool that cannot be written
//! still leaves the whole line. This is the render path of an interactive
//! session that did not ask riabuild for anything.
//!
//! **No network, and no credential.** The line is printed and *then* a sample is
//! appended to a spool; `riabuild internal usage-flush`, started detached at
//! most once a minute, is what talks to riabuild-web. A round trip on this path
//! would stall the bar whenever the network was slow, and a token in this
//! process's reach would be a token in reach of a session the model can run
//! `env` in.
//!
//! Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md` and
//! `docs/superpowers/specs/2026-09-05-statusline-in-rust-design.md`.
//!
//! [`2026-08-28-launchers-in-rust-design.md`]: https://github.com/Clubria/riabuild/blob/main/docs/superpowers/specs/2026-08-28-launchers-in-rust-design.md

mod account;
mod bar;
mod repo;
pub mod usage;

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Marks the status line the way the environment shell marks `PS1` — same word,
/// same bold blue. The prompt and the status line are two renderers answering
/// one question, so a developer learns the marker once.
///
/// It is `shell::PROMPT_LABEL` itself rather than a copy of the string: the two
/// used to be a Rust constant and a JavaScript literal with a test holding them
/// together, and the test was all there was.
const LABEL: &str = crate::shell::PROMPT_LABEL;

/// What this render knows before it reads the payload.
///
/// Taken as a value rather than read from the environment inside the drawing,
/// for the reason `shims::launch::World` is: every branch of the status line is
/// then a unit test with no environment, no interpreter and no subprocess
/// behind it — which is exactly what the JavaScript could not have.
#[derive(Debug, Clone, Default)]
pub struct Session {
    /// `CLAUDE_CONFIG_DIR` — `<root>/claude/<uuid>`, set by the launcher.
    ///
    /// Absent for a `claude` that riabuild's launcher did not start. That is a
    /// real case rather than a broken one — a developer's own install, or a
    /// `claude` straight off `PATH` — and it has no account and no spool, so it
    /// gets neither.
    pub config_dir: Option<PathBuf>,
    /// `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, when the session sets one.
    pub auto_compact_window: Option<u64>,
    /// Where this process was started, which Claude Code makes the session's
    /// own directory. The fallback for a payload naming no directory.
    pub cwd: PathBuf,
}

impl Session {
    pub fn from_env() -> Self {
        Self {
            config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            auto_compact_window: std::env::var("CLAUDE_CODE_AUTO_COMPACT_WINDOW")
                .ok()
                .and_then(|value| value.trim().parse().ok()),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

/// The whole line: `(riabuild · Clubria/payments) claude-2 · ada@clubria.com █████░░░░░ 47%`.
///
/// The account is computed **outside** the payload's own fallible half, because
/// it is not computed *from* the payload: it comes from the environment and two
/// files on disk. Folded in, a Claude Code that sent something unparseable would
/// take the signed-in account off the line along with the bar, and *which
/// account is this window* would go missing exactly when something is already
/// wrong.
pub fn render(input: &str, session: &Session) -> String {
    let who = account::line(session.config_dir.as_deref());

    let Some(payload) = parse(input) else {
        // A marker with nothing after it is still the answer to "which
        // environment is this?", which is the question the label exists for.
        return format!("{}{who}", marker(None));
    };

    format!(
        "{}{who}{}",
        marker(repo::of(&cwd_of(&payload, session)).as_deref()),
        bar::draw(&payload, session.auto_compact_window),
    )
}

/// Appends this render's usage sample, and says whether a flush is due.
///
/// Separated from [`render`] rather than folded into it, so the caller can put
/// the line in front of the developer before anything touches a file — and so
/// that a failure here has no way to reach the string that was already printed.
///
/// Returns `false` for everything that is not "a flush is due now", including
/// every failure: no account directory, a payload with no session, a spool that
/// cannot be written, a flush attempted less than a minute ago.
pub async fn collect(input: &str, session: &Session) -> bool {
    let Some(payload) = parse(input) else {
        return false;
    };
    usage::collect(&payload, session.config_dir.as_deref()).await
}

fn parse(input: &str) -> Option<Value> {
    serde_json::from_str(input).ok()
}

/// The repository goes *inside* the parentheses — `(riabuild · Clubria/payments)`
/// — so there is still one marker to learn rather than a marker with a second
/// thing sitting next to it.
///
/// Spliced into [`LABEL`] instead of spelled out again, so the word the prompt
/// and the status line share stays in one place.
fn marker(repo: Option<&str>) -> String {
    let inner = match repo {
        Some(repo) => format!("{} · {repo})", LABEL.trim_end_matches(')')),
        None => LABEL.to_string(),
    };
    format!("\x1b[1;34m{inner}\x1b[0m")
}

/// Where the session is *now*, not where it started: a developer who has `cd`'d
/// into a second checkout is in that repository, and `project_dir` would still
/// name the first.
fn cwd_of(payload: &Value, session: &Session) -> PathBuf {
    let named = payload
        .get("workspace")
        .and_then(|workspace| workspace.get("current_dir"))
        .and_then(Value::as_str)
        .or_else(|| payload.get("cwd").and_then(Value::as_str));
    match named {
        // Relative only in a payload nobody has sent, but resolving it against
        // this process's own directory is what the JavaScript's `path.resolve`
        // did and costs one line.
        Some(dir) => session.cwd.join(Path::new(dir)),
        None => session.cwd.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statusline::testing::{checkout, namespace, payload_at};

    /// The marker and the prompt answer the same question, and now they answer
    /// it with the same constant rather than with two strings a test compares.
    #[test]
    fn the_status_line_carries_the_same_label_as_the_prompt() {
        assert!(
            render("{}", &Session::default()).contains(crate::shell::PROMPT_LABEL),
            "the status line has to say `{}`, like the prompt does",
            crate::shell::PROMPT_LABEL
        );
    }

    #[test]
    fn the_marker_names_the_repository_the_session_is_in() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(&payload_at(&dir), &Session::default());

        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
    }

    /// The repository goes *inside* the parentheses. There is one marker to
    /// learn, the same one the prompt draws — not a marker with a second thing
    /// beside it that reads as two environments.
    #[test]
    fn the_repository_is_part_of_the_marker_rather_than_beside_it() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(&payload_at(&dir), &Session::default());

        assert!(
            !drawn.contains("(riabuild)"),
            "the bare marker must not be drawn beside the named one: {drawn:?}"
        );
    }

    /// The bar and the marker are drawn from the same payload and must not be
    /// an either/or.
    #[test]
    fn the_context_bar_still_draws_beside_the_repository() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");
        let payload = format!(
            r#"{{"workspace":{{"current_dir":{:?}}},"context_window":{{"remaining_percentage":100}}}}"#,
            dir.to_string_lossy()
        );

        let drawn = render(&payload, &Session::default());

        assert!(drawn.contains("Clubria/payments"), "{drawn:?}");
        assert!(drawn.contains('%'), "{drawn:?}");
    }

    /// Claude Code sends this on every render and riabuild controls neither end
    /// of it, so an unparseable one is a thing to survive rather than a bug.
    #[test]
    fn a_payload_that_is_not_json_still_leaves_a_marker() {
        let drawn = render("not json at all", &Session::default());

        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
    }

    #[test]
    fn a_directory_that_does_not_exist_still_leaves_a_marker() {
        let home = tempfile::TempDir::new().unwrap();

        let drawn = render(&payload_at(&home.path().join("gone")), &Session::default());

        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
    }

    /// A payload naming no directory is drawn against the directory Claude Code
    /// started this process in, which is the session's own.
    #[test]
    fn a_payload_with_no_directory_falls_back_to_where_the_render_runs() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(
            "{}",
            &Session {
                cwd: dir,
                ..Session::default()
            },
        );

        assert!(drawn.contains("Clubria/payments"), "{drawn:?}");
    }

    /// The account is not computed from the payload, so a payload that will not
    /// parse must not take it off the line: something is already wrong, and
    /// *which account is this* is what a developer needs most at that moment.
    #[test]
    fn the_account_survives_a_payload_that_will_not_parse() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[("one-uuid", Some("ada@clubria.com"))],
        );

        let drawn = render(
            "not json at all",
            &Session {
                config_dir: Some(dirs[0].clone()),
                ..Session::default()
            },
        );

        assert!(drawn.contains("(riabuild)"), "{drawn:?}");
        assert!(drawn.contains("claude-1"), "{drawn:?}");
        assert!(drawn.contains("ada@clubria.com"), "{drawn:?}");
    }

    /// The account sits *beside* the marker rather than inside it — the
    /// opposite of the repository one function up, and for the opposite reason.
    /// The repository says which environment this is, the question the prompt
    /// also answers; the account says who this window is signed in as, which the
    /// prompt does not carry.
    #[test]
    fn the_account_sits_beside_the_marker_rather_than_inside_it() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[("one-uuid", Some("ada@clubria.com"))],
        );
        let dir = home.path().join("payments");
        checkout(&dir, "git@github.com:Clubria/payments.git");

        let drawn = render(
            &payload_at(&dir),
            &Session {
                config_dir: Some(dirs[0].clone()),
                ..Session::default()
            },
        );

        assert!(drawn.contains("(riabuild · Clubria/payments)"), "{drawn:?}");
        assert!(
            !drawn.contains("ada@clubria.com)"),
            "the account must not be folded into the marker: {drawn:?}"
        );
    }
}

/// Fixtures the four suites below this module share.
///
/// A checkout is written by hand rather than by `git`, on purpose: nothing here
/// shells out to `git`, so a fixture needs no `git` binary either and these
/// tests pin the on-disk layout the status line actually depends on.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    pub(crate) fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("a fixture path has a parent"))
            .expect("creating the fixture directory");
        std::fs::write(path, contents).expect("writing the fixture");
    }

    pub(crate) fn checkout(at: &Path, url: &str) {
        write(
            &at.join(".git").join("config"),
            &format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n"),
        );
    }

    /// A linked worktree of `checkout`, laid out the way `git worktree add`
    /// leaves one: a `.git` *file* naming a directory under the checkout's own
    /// `.git`, and a `commondir` in that directory pointing back at the config
    /// the two share.
    pub(crate) fn worktree(checkout: &Path, at: &Path, name: &str) {
        let gitdir = checkout.join(".git").join("worktrees").join(name);
        write(&gitdir.join("commondir"), "../..\n");
        write(&at.join(".git"), &format!("gitdir: {}\n", gitdir.display()));
    }

    pub(crate) fn payload_at(dir: &Path) -> String {
        format!(
            r#"{{"workspace":{{"current_dir":{:?}}}}}"#,
            dir.to_string_lossy()
        )
    }

    /// One developer's riabuild namespace: `config.json` at the root and the
    /// account directories under `claude/` beside it, which is the layout
    /// `Paths::config_file` and `Paths::claude_profile_dir` produce on a laptop
    /// and on a server alike.
    ///
    /// Returns the directory `CLAUDE_CONFIG_DIR` would name for each account, in
    /// the order they were given — so a test can hand the second one to
    /// `Session` and assert it is called `claude-2`.
    pub(crate) fn namespace(root: &Path, accounts: &[(&str, Option<&str>)]) -> Vec<PathBuf> {
        let uuids: Vec<String> = accounts
            .iter()
            .map(|(uuid, _)| format!("{uuid:?}"))
            .collect();
        write(
            &root.join("config.json"),
            &format!(r#"{{"claude_accounts":[{}]}}"#, uuids.join(",")),
        );
        accounts
            .iter()
            .map(|(uuid, email)| {
                let dir = root.join("claude").join(uuid);
                std::fs::create_dir_all(&dir).expect("creating the account directory");
                // A directory with no `.claude.json` is an account nothing has
                // signed in yet — a real state, not a broken one.
                if let Some(email) = email {
                    write(
                        &dir.join(".claude.json"),
                        &format!(r#"{{"oauthAccount":{{"emailAddress":{email:?}}}}}"#),
                    );
                }
                dir
            })
            .collect()
    }
}

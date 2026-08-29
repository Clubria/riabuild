//! What `~/.riabuild/bin/claude`, `codex` and `grok` do, in Rust.
//!
//! Every launcher in `bin/` is now one `exec` line naming this process and the
//! parameters that launcher was generated with. What it used to be was ninety
//! lines of `sh` per harness, and the decisions in them are the ones this
//! module makes instead:
//!
//! | what the script did | what does it now |
//! |---|---|
//! | `case "$binary" in /*) ;; *) binary="" ;; esac` then `[ ! -x ]` | [`resolve_binary`] |
//! | `printf '%s' "$PATH" \| tr ':' '\n' \| grep -vxF "$bin" \| paste -sd: -` | [`path_without`] |
//! | `case "$PWD" in "$c"\|"$c"/*) …` | [`checkout_for`](super::claude::checkout_for) |
//! | `for arg do case "$arg" in --yolo\|-a*) …` | [`codex::handoff`](super::codex::handoff) |
//! | `if [ $# -eq 0 ] && [ -t 0 ] && [ -t 1 ]` | [`World::stdin_is_tty`] and friends |
//!
//! **Why this is not a matter of taste.** A shell script is a program with no
//! type checker, no test that runs in CI without a subprocess, and a parser
//! that turns a mistake into a different working program rather than into an
//! error. Three of the bugs the old launchers actually shipped are of a kind
//! that cannot occur here: a `PATH` strip whose `grep -vxF` matched the wrong
//! entry, a path with a space in it splitting back into two arguments after
//! `${x:+--cwd "$x"}`, and a `set --` whose branch left the wrong flag on the
//! line. The generator's own comments record each of them as a thing not to do
//! again — which is a rule enforced by whoever reads them next, and now is not.
//!
//! What replaced them is one exec line per launcher, and the properties that
//! made the shell version tolerable are kept rather than dropped:
//!
//! - **The values are still in the file.** `check()` compares a launcher
//!   against what riabuild would write now, so a launcher that names last
//!   week's Node or a deleted account is still drift a re-run repairs. Moving
//!   the parameters into riabuild's own state instead would have made every
//!   launcher on the machine byte-identical and that comparison worthless.
//! - **The path is still absolute.** See the header of `shims`: riabuild is the
//!   one tool riabuild does not put on `PATH`.
//! - **It is still an `exec`.** [`CommandRunner::exec_replacing`] is
//!   `execvp(2)`, so the developer's shell goes on waiting for one process and
//!   `Ctrl+C` still reaches the harness.
//!
//! [`CommandRunner::exec_replacing`]: riabuild_runner::CommandRunner::exec_replacing

use std::path::{Path, PathBuf};

use anyhow::Result;
use riabuild_runner::{CommandRunner, RunOptions};

use crate::shell::shell_quote;

/// Which launcher is running. One per harness riabuild writes launchers for.
///
/// Spelled as data rather than as three subcommands because everything around
/// it — the script, the parameters, the hand-over — is the same three steps in
/// the same order, and only [`handoff`] differs. `riabuild-harness::Kind` is
/// the same three names and is deliberately not reused: that one is about
/// driving a harness headless for the agents window, and a launcher is the
/// developer's own interactive session. Sharing the type would tie the two
/// together at exactly the point they have nothing to say to each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Grok,
}

impl Harness {
    /// What the launcher writes on the exec line, and what the CLI parses back.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Grok => "grok",
        }
    }

    /// The bare command name to fall back to when the recorded binary is gone.
    ///
    /// The same word as [`as_str`](Self::as_str) today, and separate from it
    /// because the two answer different questions: one names a subcommand of
    /// riabuild, the other names somebody else's program on the developer's
    /// `PATH`.
    pub fn command(self) -> &'static str {
        self.as_str()
    }

    /// The variable that points the harness at one profile's config directory.
    pub fn home_var(self) -> &'static str {
        match self {
            Harness::Claude => "CLAUDE_CONFIG_DIR",
            Harness::Codex => "CODEX_HOME",
            Harness::Grok => "GROK_HOME",
        }
    }
}

/// Everything one launcher was generated knowing — the whole of what used to be
/// interpolated into its shell script.
///
/// Resolved when the launcher is *written*, not when it runs, which is the
/// property the shell version had and this keeps. The alternative — a launcher
/// naming only its profile number and this process working the rest out per
/// launch — would make `claude` depend on reading `config.json` before it could
/// start, and would put `Ctx::project_dir`'s answer (which wants the org's
/// default repository, and therefore the network) on the path of a command
/// documented to work with no session and no network at all.
#[derive(Debug, Clone)]
pub struct Plan {
    pub harness: Harness,
    /// `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `GROK_HOME` for this profile.
    pub home: PathBuf,
    /// The harness binary riabuild installed, as recorded at write time. A bare
    /// name on a machine with no Node pinned yet — see [`resolve_binary`].
    pub binary: String,
    /// `~/.riabuild/bin`, which is what comes *off* `PATH` in the fallback.
    pub bin_dir: PathBuf,
    /// `~/.riabuild/org-settings.json`. Claude Code only.
    pub settings: Option<PathBuf>,
    /// Every checkout this machine knows a path for, longest first. Claude Code
    /// only, and only for the agents view.
    pub checkouts: Vec<PathBuf>,
    /// What the agents view opens on when the developer is standing in none of
    /// `checkouts`. Claude Code only.
    pub default_checkout: Option<PathBuf>,
    /// Where this account's status line writes usage samples, when the
    /// developer has marked the account as one to track. Claude Code only.
    ///
    /// `None` is the whole of "do not collect from this account": no path
    /// reaches the status line's environment, so it returns before it writes
    /// anything rather than filling a spool nothing will ever send. Which
    /// accounts are tracked is `UserConfig::tracked_accounts`, and it is
    /// resolved *here*, at write time, for the reason every other value on this
    /// line is — so that `claude` never has to read `config.json` to start.
    ///
    /// Being on the launcher line is also what makes it drift `check()` can
    /// see: `riabuild claude track 2` changes what riabuild would write, and
    /// the next run rewrites the launcher.
    pub usage_spool: Option<PathBuf>,
    /// The developer's own arguments, verbatim.
    pub args: Vec<String>,
}

impl Plan {
    /// A plan with nothing but a harness and the three things every launcher
    /// has, for the two harnesses that need no more than that.
    pub fn new(harness: Harness, home: PathBuf, binary: String, bin_dir: PathBuf) -> Self {
        Self {
            harness,
            home,
            binary,
            bin_dir,
            settings: None,
            checkouts: Vec::new(),
            default_checkout: None,
            usage_spool: None,
            args: Vec::new(),
        }
    }
}

/// The machine, as the shell script used to ask about it — one field per test
/// it ran.
///
/// Taken as a value rather than probed inside the decision so that every branch
/// of every launcher is a unit test with no filesystem, no terminal and no
/// environment behind it. The old scripts could only be tested by *running*
/// them, which is why the two harnesses with a smoke test for this have it
/// marked `#[ignore]` and needing a real install.
#[derive(Debug, Clone, Default)]
pub struct World {
    /// `[ -x "$binary" ]` — the recorded harness binary is still there.
    pub binary_is_executable: bool,
    /// `[ -x "$bin_dir/wl-copy" ]` — riabuild's own clipboard shim is written.
    pub wl_copy_present: bool,
    /// `$WAYLAND_DISPLAY`, `$DISPLAY` — whether this machine has a display of
    /// its own.
    pub wayland_display: Option<String>,
    pub x11_display: Option<String>,
    /// `[ -f "$settings" ]` — the org settings have been fetched at least once.
    pub settings_present: bool,
    /// `[ -t 0 ]`, `[ -t 1 ]`.
    pub stdin_is_tty: bool,
    pub stdout_is_tty: bool,
    /// `$CLAUDE_CODE_DISABLE_AGENT_VIEW` — Claude Code's documented off switch.
    pub agents_view_disabled: bool,
    /// `$PWD`, which decides which checkout the agents view opens on.
    pub cwd: PathBuf,
    /// `[ -d "$project" ]` for whichever checkout `$PWD` selected. A view
    /// pinned to a directory nobody has is worse than no `--cwd` at all.
    pub selected_checkout_exists: bool,
    /// `$PATH`, for the fallback that has to take `bin_dir` back off it.
    pub path: String,
    /// This process's own binary, which is riabuild's — the launcher `exec`ed
    /// it, so `current_exe()` is the absolute path the launcher named.
    ///
    /// Passed to the status line as `RIABUILD_SELF` so it can start a flush.
    /// Not `RIABUILD_BIN`, which e2e and CI already use to name the binary
    /// under test — a collision there would point a flush at a build fixture.
    /// `None` where the platform will not say, and the cost of that is only the
    /// one-a-minute cadence: the sample is still spooled and the next `riabuild`
    /// run still sends it.
    pub riabuild: Option<PathBuf>,
}

/// What the launcher hands over to, once every question has been answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    /// Absolute wherever riabuild could make it so — see [`resolve_binary`].
    pub program: String,
    pub args: Vec<String>,
    /// Applied to this process immediately before the `exec`, because after it
    /// there is no other process to apply it to.
    pub env: Vec<(String, String)>,
    /// Taken off it, which is a different thing: the Claude Code launcher's
    /// `unset SSH_CONNECTION SSH_CLIENT SSH_TTY` cannot be spelled as a value,
    /// and setting those three to `""` would leave Claude Code still reading
    /// itself as an SSH session.
    pub env_remove: Vec<String>,
}

impl Handoff {
    fn new(program: String) -> Self {
        Self {
            program,
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    pub(super) fn unset(mut self, keys: impl IntoIterator<Item = &'static str>) -> Self {
        self.env_remove.extend(keys.into_iter().map(str::to_string));
        self
    }

    pub(super) fn with_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args.extend(args);
        self
    }

    pub(super) fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.push((key.to_string(), value.into()));
        self
    }
}

/// Which harness binary to start, and what `PATH` it is started with.
///
/// The three-way decision every launcher opened with, and the only one of them
/// with a comment in all three scripts explaining a case that cannot happen
/// yet. In order:
///
/// - **A recorded path that is not absolute is treated as no path at all.**
///   `Ctx::claude()` and `Ctx::codex()` fall back to the bare name before a Node
///   is pinned, and a bare name reaching the executable test below would be
///   resolved against the process's *working directory* — so a same-named
///   executable in whatever directory the developer happened to be standing in
///   would pass it, skip the strip, and be started in place of the harness.
/// - **A recorded path that is there is what riabuild installed**, and is used.
/// - **Otherwise the recorded binary is gone** — a `claude update` that migrated
///   to a native install, a Node version bump since the last run, a
///   half-removed install — and the launcher falls back to whatever the
///   developer's own `PATH` offers, with [`path_without`] taking
///   `~/.riabuild/bin` off it first. Without that strip the fallback finds *this
///   launcher*, because `bin/` leads `PATH` inside the environment shell, and
///   the developer gets a fork bomb rather than an error.
pub fn resolve_binary(plan: &Plan, world: &World) -> Resolved {
    let recorded = Path::new(&plan.binary);
    if recorded.is_absolute() && world.binary_is_executable {
        return Resolved::Recorded(plan.binary.clone());
    }
    Resolved::OnPath {
        name: plan.harness.command(),
        path: path_without(&world.path, &plan.bin_dir),
    }
}

/// The answer [`resolve_binary`] gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The binary riabuild installed, by the absolute path it was recorded at.
    Recorded(String),
    /// The developer's own, found on a `PATH` that no longer leads with
    /// `~/.riabuild/bin`.
    OnPath { name: &'static str, path: String },
}

/// `PATH` with one directory removed, entry for entry.
///
/// The Rust spelling of `printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$dir" |
/// paste -sd: -`, and worth naming for what that pipeline was doing rather than
/// for what it looked like: it removes entries **equal** to `dir`, not entries
/// containing it, which is why `grep` needed both `-x` and `-F` and why getting
/// either wrong would have silently emptied a developer's `PATH` of everything
/// under their home directory.
///
/// Empty entries are kept, exactly as the pipeline kept them. An empty `PATH`
/// entry means the current directory to `execvp(3)`, so dropping them would be
/// a change to how the developer's own machine resolves commands, made by a
/// launcher, for tidiness.
pub fn path_without(path: &str, dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    path.split(':')
        .filter(|entry| *entry != dir)
        .collect::<Vec<_>>()
        .join(":")
}

/// The first executable called `name` on `path`.
///
/// The shell left this to `execvp(3)` by handing it a bare name. Doing it here
/// instead removes a question that has no good answer in Rust: which `PATH`
/// `Command::new("claude").exec()` searches when the command also carries a
/// `PATH` of its own in its environment. Resolving against the stripped `PATH`
/// explicitly means the launcher starts the binary it decided on, and the
/// `PATH` it exports is what the harness's own children inherit rather than
/// also being a lookup rule this code depends on.
///
/// `None` where nothing matches, and the caller then execs the bare name so the
/// failure is the operating system's own `ENOENT` for a name the developer
/// recognises.
pub async fn find_on_path(path: &str, name: &str) -> Option<PathBuf> {
    for entry in path.split(':') {
        // An empty entry is the current directory, which is what `execvp(3)`
        // makes of it too.
        let dir = if entry.is_empty() {
            Path::new(".")
        } else {
            Path::new(entry)
        };
        let candidate = dir.join(name);
        if is_executable(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

/// `[ -x <path> ]`, near enough.
///
/// `access(2)` with `X_OK` is what the shell test really is, and the difference
/// — a file whose execute bit is set for a user this process is not — is a
/// machine where the launcher would have failed at the `exec` either way, one
/// line later and with a clearer message.
#[cfg(unix)]
pub async fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::metadata(path)
        .await
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub async fn is_executable(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// What one launcher's whole hand-over comes to, given the machine.
///
/// Pure: every filesystem and terminal question is already answered in `world`,
/// and nothing here starts anything. That is what makes each harness's rules —
/// which are undocumented behaviour read out of three third-party binaries —
/// assertable in an ordinary `cargo test` rather than only in the `#[ignore]`d
/// smoke tests that need all three installed.
pub fn handoff(plan: &Plan, world: &World) -> Handoff {
    let resolved = resolve_binary(plan, world);
    let (program, path) = match &resolved {
        Resolved::Recorded(binary) => (binary.clone(), None),
        Resolved::OnPath { name, path } => ((*name).to_string(), Some(path.clone())),
    };

    let mut handoff = Handoff::new(program).env(plan.harness.home_var(), home_value(plan));
    if let Some(path) = path {
        handoff = handoff.env("PATH", path);
    }

    match plan.harness {
        Harness::Claude => super::claude::handoff(handoff, plan, world),
        Harness::Codex => super::codex::handoff(handoff, plan, world, &resolved),
        Harness::Grok => super::grok::handoff(handoff, plan),
    }
}

fn home_value(plan: &Plan) -> String {
    plan.home.to_string_lossy().into_owned()
}

/// Runs one launcher: gather the machine, decide, then become the harness.
///
/// The order is the script's own and matters in one place. `CODEX_HOME` is
/// created *before* the hand-over because Codex refuses to start against a
/// directory that is not there, and the gap between two riabuild runs is
/// exactly where a `rm -rf` lands — the setup task would go on reporting a
/// satisfied machine while every `codex` refused to start. Grok Build would
/// make its own, and gets the same treatment so that "nine profiles" is a state
/// riabuild can assert rather than one that comes true the first time each
/// launcher is run.
pub async fn run(runner: &dyn CommandRunner, plan: &Plan) -> Result<i32> {
    if plan.harness != Harness::Claude {
        tokio::fs::create_dir_all(&plan.home).await?;
    }

    let world = observe(plan).await;
    let handoff = handoff(plan, &world);

    let program = match Path::new(&handoff.program).is_absolute() {
        true => handoff.program.clone(),
        // `find_on_path` rather than a bare name for `execvp` to resolve — see
        // its own note. Falling through with the bare name where nothing
        // matched keeps the failure the operating system's.
        false => {
            let path = handoff
                .env
                .iter()
                .find(|(key, _)| key == "PATH")
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            match find_on_path(&path, &handoff.program).await {
                Some(found) => found.to_string_lossy().into_owned(),
                None => handoff.program.clone(),
            }
        }
    };

    let args: Vec<&str> = handoff.args.iter().map(String::as_str).collect();
    runner
        .exec_replacing(
            &program,
            &args,
            &RunOptions {
                env: handoff.env.clone(),
                env_remove: handoff.env_remove.clone(),
                ..Default::default()
            },
        )
        .await
}

/// Asks the machine every question the shell script used to ask it.
///
/// One place, so that a launcher's decisions have exactly one impure input and
/// the rest of this module can be tested without a machine.
async fn observe(plan: &Plan) -> World {
    use std::io::IsTerminal;

    let cwd = std::env::current_dir().unwrap_or_default();
    // Asked of the *selected* checkout rather than of all of them: the script
    // ran one `[ -d ]`, on the arm the `case` had already chosen, and probing
    // every entry would make a launcher's cost grow with the number of
    // repositories this machine has cloned.
    let selected_checkout_exists = match super::claude::checkout_for(
        &cwd,
        &plan.checkouts,
        plan.default_checkout.as_deref(),
    ) {
        Some(checkout) => tokio::fs::metadata(checkout)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false),
        None => false,
    };

    World {
        binary_is_executable: is_executable(Path::new(&plan.binary)).await,
        wl_copy_present: is_executable(&plan.bin_dir.join("wl-copy")).await,
        wayland_display: non_empty("WAYLAND_DISPLAY"),
        x11_display: non_empty("DISPLAY"),
        settings_present: match &plan.settings {
            Some(settings) => tokio::fs::try_exists(settings).await.unwrap_or(false),
            None => false,
        },
        stdin_is_tty: std::io::stdin().is_terminal(),
        stdout_is_tty: std::io::stdout().is_terminal(),
        agents_view_disabled: non_empty("CLAUDE_CODE_DISABLE_AGENT_VIEW").is_some(),
        cwd,
        selected_checkout_exists,
        path: std::env::var("PATH").unwrap_or_default(),
        riabuild: std::env::current_exe().ok(),
    }
}

/// `[ -n "$VAR" ]`: a variable set to the empty string is not set.
///
/// The distinction is load-bearing for `WAYLAND_DISPLAY` and `DISPLAY`, which
/// the launcher tests with `-z` — and for `CLAUDE_CODE_DISABLE_AGENT_VIEW`,
/// where an empty value must not read as "the developer turned the view off".
fn non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// The whole of one launcher: a shebang, a comment, and one `exec`.
///
/// Everything the launcher knows is on that line, so `check()` still detects a
/// launcher naming last week's Node or an account that has been deleted, and a
/// developer reading the file can still see what their `claude` is about to do.
/// What is *not* on it is a decision — there is no branch, no substitution and
/// no pipeline for a shell to get wrong.
///
/// Quoted with the workspace's one POSIX quoter, which is a change from the
/// scripts this replaces: those interpolated every path into `"…"`, where a
/// `$`, a backtick or a `"` in a home directory would have been expanded or
/// would have ended the string. No developer has hit it and none should have to.
pub fn script(riabuild: &Path, plan: &Plan) -> String {
    let mut line = vec![
        shell_quote(&riabuild.to_string_lossy()),
        "internal".to_string(),
        "launch".to_string(),
        plan.harness.as_str().to_string(),
    ];
    let mut flag = |name: &str, value: &str| {
        line.push(name.to_string());
        line.push(shell_quote(value));
    };
    flag("--home", &plan.home.to_string_lossy());
    flag("--binary", &plan.binary);
    flag("--bin-dir", &plan.bin_dir.to_string_lossy());
    if let Some(settings) = &plan.settings {
        flag("--settings", &settings.to_string_lossy());
    }
    for checkout in &plan.checkouts {
        flag("--checkout", &checkout.to_string_lossy());
    }
    if let Some(default) = &plan.default_checkout {
        flag("--default-checkout", &default.to_string_lossy());
    }
    if let Some(spool) = &plan.usage_spool {
        flag("--usage-spool", &spool.to_string_lossy());
    }
    // `--` so that a developer's own first argument can be anything at all:
    // `claude --resume`, `codex -a on-request`, `grok --permission-mode plan`.
    // Without it the launcher would be the one program on the machine that
    // cannot be passed a flag riabuild has not heard of.
    line.push("--".to_string());

    let harness = plan.harness.as_str();
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Every decision this launcher used to make in shell — which binary, which
# PATH, which profile, which flags, which checkout the agents view opens on —
# is made by `riabuild internal launch {harness}` instead, which then `exec`s
# the harness itself. This file carries the answers riabuild resolved when it
# was written and nothing else.
#
# riabuild is named in full because it is not on PATH: it lives in its own
# versioned directory, and a bare name would find another machine's copy or
# none at all.
exec {line} "$@"
"#,
        line = line.join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan::new(
            Harness::Claude,
            PathBuf::from("/home/ada/.riabuild/claude/abc"),
            "/home/ada/.riabuild/node/22.23.1/bin/claude".to_string(),
            PathBuf::from("/home/ada/.riabuild/bin"),
        )
    }

    #[test]
    fn the_recorded_binary_is_used_where_it_is_still_there() {
        let world = World {
            binary_is_executable: true,
            ..Default::default()
        };
        assert_eq!(
            resolve_binary(&plan(), &world),
            Resolved::Recorded("/home/ada/.riabuild/node/22.23.1/bin/claude".to_string())
        );
    }

    /// A `claude update` that migrated to a native install, or a Node bump
    /// since the last run. The fallback must not find this launcher, which it
    /// would: `~/.riabuild/bin` leads `PATH` inside the environment shell.
    #[test]
    fn a_binary_that_is_gone_falls_back_to_a_path_without_riabuilds_own_bin() {
        let world = World {
            binary_is_executable: false,
            path: "/home/ada/.riabuild/bin:/usr/local/bin:/usr/bin".to_string(),
            ..Default::default()
        };
        assert_eq!(
            resolve_binary(&plan(), &world),
            Resolved::OnPath {
                name: "claude",
                path: "/usr/local/bin:/usr/bin".to_string(),
            }
        );
    }

    /// The case all three scripts carried a comment about. Before a Node is
    /// pinned `Ctx::claude()` answers with the bare name, and a bare name is
    /// resolved against the *working directory* by an executable test — so a
    /// `./claude` in whatever directory the developer was standing in would
    /// have been started in place of the harness.
    #[test]
    fn a_recorded_binary_that_is_not_absolute_is_treated_as_no_binary_at_all() {
        let mut plan = plan();
        plan.binary = "claude".to_string();
        let world = World {
            // Even with the executable test passing, which is the whole trap.
            binary_is_executable: true,
            path: "/home/ada/.riabuild/bin:/usr/bin".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            resolve_binary(&plan, &world),
            Resolved::OnPath { .. }
        ));
    }

    #[test]
    fn the_path_strip_removes_whole_entries_and_never_prefixes() {
        // `grep -vxF` and not `grep -v`: the second would have taken every
        // directory *under* `~/.riabuild` off a developer's PATH, including
        // Node's own bin.
        let stripped = path_without(
            "/home/ada/.riabuild/bin:/home/ada/.riabuild/node/22.23.1/bin:/usr/bin",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert_eq!(
            stripped, "/home/ada/.riabuild/node/22.23.1/bin:/usr/bin",
            "only the exact entry comes off"
        );
    }

    #[test]
    fn the_path_strip_keeps_an_empty_entry_because_execvp_reads_it_as_the_cwd() {
        assert_eq!(
            path_without("/usr/bin::/bin", Path::new("/nowhere")),
            "/usr/bin::/bin"
        );
    }

    #[test]
    fn a_path_that_is_only_the_stripped_directory_comes_back_empty() {
        // Not a case any machine reaches, and the one that would crash a
        // `join` written the other way round.
        assert_eq!(path_without("/opt/bin", Path::new("/opt/bin")), "");
    }

    /// The launcher is one `exec` and nothing else. This is the property the
    /// whole change exists for, so it is asserted directly rather than inferred
    /// from the absence of any particular construct.
    #[test]
    fn a_generated_launcher_has_no_logic_in_it_at_all() {
        let script = script(Path::new("/opt/riabuild/2026.08.27/riabuild"), &plan());
        let code: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        assert_eq!(code.len(), 1, "{script}");
        assert!(code[0].starts_with("exec "), "{script}");
        for forbidden in ["if ", "case ", "for ", "set --", "$(", "|", "&&"] {
            assert!(!script.contains(forbidden), "{forbidden} in {script}");
        }
    }

    /// A `$`, a backtick or a `"` in a home directory used to reach the shell
    /// unquoted. Nobody has hit it; nobody should have to.
    #[test]
    fn every_path_on_the_exec_line_is_quoted_against_the_shell() {
        let mut plan = plan();
        plan.home = PathBuf::from("/home/o'brien/$HOME/`whoami`/claude");
        let script = script(Path::new("/opt/riabuild/riabuild"), &plan);
        assert!(
            script.contains(r#"'/home/o'\''brien/$HOME/`whoami`/claude'"#),
            "{script}"
        );
    }
}

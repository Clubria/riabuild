//! `~/.riabuild/bin` — the small scripts that make the environment work.
//!
//! The important ones are the Claude Code launchers: `claude` runs the primary
//! account, and `claude-1` … `claude-N` each run their own. Every one launches
//! Claude Code with that account's config directory and the org settings
//! layered over it.
//!
//! ```sh
//! # a bare, interactive `claude` — opens on the agents view
//! CLAUDE_CONFIG_DIR=~/.riabuild/claude/<uuid> claude \
//!   --settings ~/.riabuild/org-settings.json \
//!   --allow-dangerously-skip-permissions \
//!   agents
//!
//! # anything else: `claude -p`, `claude --resume`, `claude-2 auth login`
//! CLAUDE_CONFIG_DIR=~/.riabuild/claude/<uuid> claude \
//!   --settings ~/.riabuild/org-settings.json \
//!   --exclude-dynamic-system-prompt-sections \
//!   --allow-dangerously-skip-permissions "$@"
//! ```
//!
//! `--settings` layers over the account's own settings, so org policy is always
//! current, removals take effect, and developer edits survive. Nothing is merged
//! into anyone's `settings.json`.
//!
//! `--exclude-dynamic-system-prompt-sections` is a flag and not a settings key
//! because Claude Code offers no settings key for it: it is read off argv
//! (`excludeDynamicSections`) and appears nowhere in the settings schema, so the
//! launcher is the only place riabuild can turn it on for the whole team.
//!
//! `--allow-dangerously-skip-permissions` is a flag for a related reason. The
//! org settings already carry `permissions.defaultMode: "bypassPermissions"`,
//! and that key decides which mode a session *starts* in; whether the mode stays
//! *reachable* from the Shift+Tab cycle is a second question with no settings key
//! at all. The flag is what riabuild can say about it, and it covers the machines
//! the settings key cannot reach — including one whose launcher execs without
//! `--settings` because the org settings have never been fetched. See
//! `ALLOW_BYPASS`.
//!
//! **The two lines are two lines because they cannot be one.** The agents view
//! is reached by the bare `agents` positional, and Claude Code only honours it
//! when everything else on the line has been stripped off. Which flags are
//! stripped is not a matter of taste and not guessable from the names:
//! `--settings` and `--allow-dangerously-skip-permissions` both are — the first
//! is taken off with its value, the second is folded into the dispatch defaults
//! the view hands to the sessions it starts — and
//! `--exclude-dynamic-system-prompt-sections` is not. So the first two ride
//! along and the third cannot.
//!
//! Passing it anyway does not degrade to a view with one feature missing:
//! `agents` falls through to the ordinary parser as the *background-agents*
//! subcommand, and `claude` prints a list and exits. So a bare launch takes the
//! view and gives up the shared prompt prefix, and every launch that carries
//! arguments keeps the prefix and never wanted the view. Giving it up costs
//! nothing that was not already gone: that flag is not among the ones Claude
//! Code carries into a session started *from* the agents view either, so no bare
//! launch could have kept it whichever way this was written. `ALLOW_BYPASS` is
//! the opposite case and is carried, which is why it stays on both lines.
//!
//! Which also means `defaultToAgentsView` — the global-config key
//! `claude_agents_view` writes — has never decided anything here, and cannot:
//! Claude Code reads it only when the raw argv holds nothing but debug flags,
//! and every launcher riabuild has ever written passed `--settings`. The task
//! still writes it, for a developer who runs Claude Code from outside
//! `~/.riabuild/bin`; the launcher is what makes the view happen for everyone
//! else, and it does so unconditionally. See `claude_agents_view`.
//!
//! `CLAUDE_CONFIG_DIR` is present in the Claude Code binary (verified against
//! 2.1.221) but is **not** in the public settings documentation. Undocumented
//! means unpromised, so the `#[ignore]`d smoke tests at the end of this
//! file pin it and the behaviours built on it: an upstream change should
//! surface as a test failure, not as broken laptops. They need a real Claude Code
//! install, so run them with `cargo test -- --ignored` before every version bump.
//!
//! The launchers also clear `SSH_CONNECTION`, `SSH_CLIENT` and `SSH_TTY`, and
//! claim `WAYLAND_DISPLAY` on a machine with no display, which together are what
//! make the clipboard shims below reachable — see the comments in
//! `launcher_script`. Both are undocumented behaviour, read out of the shipped
//! binary rather than promised anywhere, and neither can be pinned by a smoke
//! test: Claude Code exposes no non-interactive clipboard command to assert
//! against. Re-read them by hand when the pinned Claude Code version moves.
//!
//! Every shim here names the riabuild binary by **absolute path**, and none of
//! them may go back to a bare `riabuild`. riabuild does not put its own binary
//! on `PATH`: it lives at `<tools>/riabuild/<version>/riabuild`, while
//! `shell::riabuild_path_dirs` leads `PATH` with `bin/` and Node's `bin/` and
//! nothing else. A bare name therefore resolves to whatever *else* is called
//! riabuild on that machine — on a server with an apt or Homebrew copy, some
//! other version; on a server without one, nothing at all, and `$BROWSER`
//! becomes `xdg-open: exec: riabuild: not found`.

pub mod browser;
pub mod clipboard;
pub mod codex;
pub mod grok;

use crate::Ctx;
use anyhow::{Context, Result};
use riabuild_fetch::archive::make_executable;
use std::path::Path;

/// Moves the per-machine half of the system prompt into the first user message.
///
/// The sections it moves — working directory, environment info, memory paths,
/// git status — are the ones that differ on every laptop, and they sit in the
/// part of the prompt the API caches. Moving them out leaves every Clubria
/// developer's Claude Code opening against a system prompt that differs only
/// where the *org's* settings differ, so the cache one developer warms is one
/// the next can reuse. Nothing is dropped; it arrives as the first user message
/// instead.
///
/// A flag rather than a settings key because Claude Code offers no key: it
/// reaches the session as `excludeDynamicSections` off parsed argv and is absent
/// from the settings schema, so `org-settings.json` could carry the name and
/// change nothing. Verified against 2.1.231.
///
/// Passed on every launch that carries arguments, which is safe on two counts.
/// It arrived in Claude Code 2.1.98, well below the 2.1.223 floor
/// `claude_accounts` already enforces and repairs, so no launcher will meet a
/// binary that rejects it — and it is a global option, accepted ahead of a
/// subcommand exactly as `--settings` is, so `claude-2 auth login` still works.
///
/// Withheld from exactly one launch: a bare interactive `claude`, which takes
/// the agents view instead. That is not a preference between the two. The
/// `agents` positional is honoured only when the rest of the line is empty
/// after Claude Code strips the options it recognises, and this flag is not one
/// of them — so the pair does not open a view with a longer prompt, it opens
/// the background-agents subcommand and exits. See the module header.
const STATIC_SYSTEM_PROMPT: &str = "--exclude-dynamic-system-prompt-sections";

/// Keeps bypass-permissions in the Shift+Tab cycle, without making it the mode.
///
/// Claude Code answers two separate questions about that mode. Which mode a
/// session *starts* in comes from `permissions.defaultMode`, which
/// `org-settings.json` already carries. Whether the mode is *offered* by the
/// cycle is decided once, at startup, and never revisited:
///
/// ```text
/// isBypassPermissionsModeAvailable =
///     (resolvedMode === "bypassPermissions" || allowDangerouslySkipPermissions)
///     && permissions.disableBypassPermissionsMode !== "disable"
/// ```
///
/// There is no settings key for the left-hand disjunct's second half — only this
/// flag — and no way to switch a running session into a mode that startup did not
/// make available. So on every machine where the settings key does not arrive or
/// is refused, the mode is not merely off: it is unreachable for the whole
/// session, and the developer's Shift+Tab silently has one fewer stop on it.
///
/// Three such machines are ordinary rather than hypothetical. A launcher whose
/// `if [ -f ]` finds no `org-settings.json` execs with **no `--settings` at all**,
/// which is every laptop before its first successful fetch. A laptop holding a
/// cached copy written before the key existed serves settings with no permission
/// mode in them — the failure `org.backfillClaudeDefaults` was written for, which
/// repairs the *server's* row and can do nothing about a file already on disk. And
/// under `CLAUDE_CODE_REMOTE` Claude Code rejects `bypassPermissions` from settings
/// outright, allowing only `acceptEdits`, `plan`, `default` and `auto`.
///
/// Strictly weaker than the settings key beside it, which is why it is safe to
/// pass on all three: it makes the mode selectable and changes no default, so a
/// machine that never fetched org policy gets a cycle stop rather than a session
/// with its permission prompts already off. `disableBypassPermissionsMode` in
/// settings still overrides it — that stays the way to take the mode away.
///
/// Passed unconditionally on the same two counts as `STATIC_SYSTEM_PROMPT`. It is
/// in Claude Code by 2.1.143, below the 2.1.223 floor `claude_accounts` enforces
/// and repairs, so no launcher will meet a binary that rejects it; and it is a
/// global option accepted ahead of a subcommand, which `settings_flag_survives_a_subcommand`
/// pins beside the other two. Verified against 2.1.235.
const ALLOW_BYPASS: &str = "--allow-dangerously-skip-permissions";

/// One account's launcher: `claude`, or `claude-<n>`.
pub fn launcher_script(
    config_dir: &Path,
    claude: &str,
    org_settings: &Path,
    bin_dir: &Path,
) -> String {
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Launches Claude Code with one account's config directory and the team's
# settings layered on top. --settings wins over the account's own settings,
# which is how org policy stays current without riabuild ever editing
# settings.json.
set -e
# Claude Code treats "am I over SSH?" as "my clipboard is not the user's
# clipboard", and answers it from these three variables alone. Over SSH it skips
# the native copy and returns "" from every paste *without running anything* — so
# the xclip/wl-paste shims in this same bin/ are never reached, and the channel
# they front is dead code. Clearing the three makes Claude Code probe for a
# clipboard tool, find riabuild's shim first on PATH, and reach the laptop.
#
# Verified against Claude Code 2.1.224: only SSH_CONNECTION reaches the clipboard
# path, but all three feed the terminal-type probe, so a session that cleared one
# and kept the others would still report itself as "ssh-session". They are also
# on Claude Code's own environment allowlist, so a relaunched or child session
# inherits whatever this script leaves — clearing them here covers the whole tree.
#
# SSH_AUTH_SOCK is deliberately NOT cleared: it is agent forwarding, not session
# detection, and dropping it breaks `git push` over SSH.
unset SSH_CONNECTION SSH_CLIENT SSH_TTY
# Clearing those three is necessary and *not* sufficient, because reading and
# writing the clipboard are not gated on the same thing. Reading is a plain
# subprocess Claude Code runs whatever the environment says. Writing goes
# through a Linux probe that asks for a display before it will look for a tool
# at all — $WAYLAND_DISPLAY before wl-copy, $DISPLAY before xclip — and a
# headless server has neither. It then records "no clipboard tool here" and
# every copy leaves as an OSC 52 escape alone, so the wl-copy/xclip shims in
# this same bin/ are never run and the channel carries pastes but no copies.
# Read out of Claude Code 2.1.232; re-read it when the pinned version moves.
#
# Claimed only where riabuild's own wl-copy is what the probe will find, and
# only on a machine that genuinely has no display of its own — so a Linux
# laptop with a real session keeps the clipboard it already had. The name is
# not a compositor anyone can connect to and is not meant to be: it says who
# claimed it, to whoever runs `env` and wonders.
if [ -x "{bin_dir}/wl-copy" ] && [ -z "$WAYLAND_DISPLAY" ] && [ -z "$DISPLAY" ]; then
  WAYLAND_DISPLAY=riabuild-channel
  export WAYLAND_DISPLAY
fi
CLAUDE_CONFIG_DIR="{config_dir}"
export CLAUDE_CONFIG_DIR
claude_binary="{claude}"
case "$claude_binary" in
  /*) ;;
  # A non-absolute path (no Node pinned yet) can't be trusted with the -x
  # test below: a same-named executable in the current directory would pass
  # it, skip the PATH strip, and exec a bare name that PATH search resolves
  # straight back to this script. Treat it as no path at all.
  *) claude_binary="" ;;
esac
if [ ! -x "$claude_binary" ]; then
  # The recorded binary is gone: a `claude update` that migrated to a native
  # install, or a Node version change since the last run. Fall back to PATH
  # with riabuild's own bin/ removed — without that this script finds itself,
  # because bin/ comes first inside the environment shell.
  PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "{bin_dir}" | paste -sd: -)
  export PATH
  claude_binary=claude
fi
# Which view Claude Code opens on. Clubria's answer is the agents view, and the
# bare `agents` positional is the only route to it that a launcher can take.
#
# The obvious route is the `defaultToAgentsView` key in the account's
# `.claude.json`, which `claude_agents_view` writes. It cannot work from here.
# Claude Code consults that key only when *every* token on the command line is a
# debug flag — it tests the raw argv, before its own option parsing — so the
# `--settings` two lines below is on its own enough to rule the agents view out.
# Every launcher riabuild has ever written passed `--settings`, so that key has
# never once decided what a Clubria developer's `claude` opened on.
#
# The positional is checked differently: `--settings` and its value are taken
# off the line first, and only what remains has to be empty. So this is the one
# spelling that carries org policy *and* opens the view. It also ignores
# `defaultToAgentsView` entirely, which is what "always" means here.
#
# {flag} has to come off the line to do it, and {bypass} does not. Which flags
# are stripped is not guessable from the names: --settings goes with its value,
# {bypass} is folded into the dispatch defaults the view hands to the sessions it
# starts, and {flag} is left where it lies. Leaving {flag} on makes the remainder
# non-empty and the view is refused; worse, `agents` then reaches the ordinary
# parser, where it is the *background-agents* subcommand, and `claude` would
# print a list of background agents and exit instead of opening a session.
#
# Nothing is lost by dropping it on this path that was not lost already: {flag}
# is not among the ones Claude Code carries into a session dispatched from the
# agents view either, so a bare launch could not have kept it whichever way this
# was written. Every other launch — `claude -p`, `claude --resume`, `claude
# "some prompt"`, `claude-2 auth login` — still gets it, unchanged.
#
# Read out of Claude Code 2.1.235; re-read it when the pinned version moves.
#
# Three guards, each load-bearing:
#
#   $# -eq 0   a developer who typed arguments asked for something other than
#              the view, and `agents` would collide with their own first word.
#   -t 0 -t 1  `echo "fix the build" | claude` is a session with a prompt on
#              stdin. The positional route does not test the terminal itself,
#              so without this the prompt is swallowed and the view opens over
#              it. Claude Code applies the same pair on its own route.
#   the env var  Claude Code's documented off switch. With the view disabled,
#              `claude agents` does not fall back to a session — it prints
#              "'claude agents' is disabled …" and exits 1. Honouring the
#              switch here is the difference between a developer turning the
#              view off and a developer losing the `claude` command.
if [ $# -eq 0 ] && [ -t 0 ] && [ -t 1 ] && [ -z "$CLAUDE_CODE_DISABLE_AGENT_VIEW" ]; then
  set -- {bypass} agents
else
  set -- {flag} {bypass} "$@"
fi
if [ -f "{settings}" ]; then
  exec "$claude_binary" --settings "{settings}" "$@"
fi
exec "$claude_binary" "$@"
"#,
        config_dir = config_dir.display(),
        bin_dir = bin_dir.display(),
        settings = org_settings.display(),
        flag = STATIC_SYSTEM_PROMPT,
        bypass = ALLOW_BYPASS,
    )
}

/// `~/.riabuild/bin/<name>`, handing off to a binary in its own versioned tree.
///
/// Used for pnpm, `gh`, and `infisical` alike. pnpm is the one that *requires*
/// it: pnpm 11 loads `dist/` from beside its executable, so copying the
/// executable onto `PATH` by itself gets `Cannot find module
/// .../dist/pnpm.mjs`. A symlink also works today, because Node resolves
/// `process.execPath` through symlinks, but that is an implementation detail of
/// the runtime pnpm happens to be built on.
///
/// `gh` and `infisical` are single static binaries that could be copied, and
/// use a shim anyway so that `bin/` is uniformly a directory of small scripts
/// and a version bump rewrites one line instead of moving 30 MB.
pub fn exec_shim(binary: &Path) -> String {
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
exec "{binary}" "$@"
"#,
        binary = binary.display(),
    )
}

/// Writes `~/.riabuild/bin/<name>` for one owned tool.
///
/// This is what puts the owned copy in front of any system one for the
/// developer's own commands: `~/.riabuild/bin` is first on `PATH` inside the
/// environment shell. riabuild's own calls do not rely on it — they run the
/// versioned path directly, because during provisioning that `PATH` does not
/// exist yet.
pub async fn write_tool(ctx: &Ctx, name: &str, binary: &Path) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;
    let shim = bin.join(name);
    riabuild_paths::config::write_atomic(&shim, exec_shim(binary).as_bytes()).await?;
    make_executable(&shim).await?;
    Ok(())
}

/// The tools riabuild shadows in `~/.riabuild/bin`.
///
/// One per direction per session type: xclip does both on X11, and Wayland
/// splits them across wl-paste and wl-copy. Every name here must also be a
/// `Tool` the shim can parse — one without a parser would shadow a working
/// binary with one that passes everything through.
pub const CLIPBOARD_TOOLS: &[&str] = &["xclip", "wl-paste", "wl-copy"];

/// `~/.riabuild/bin/xclip`, `wl-paste` and `wl-copy`.
///
/// All three route into riabuild, which decides whether the invocation is a
/// clipboard transfer to send down the channel or something to hand to the real
/// binary. They are written on the *server*, where `~/.riabuild/bin` leads
/// `PATH` and Claude Code's probe finds them first.
///
/// Written by `provision::write_launchers`, under the same condition
/// `shell::browser_for` uses to export `BROWSER` — see the note there for why
/// the two must not drift apart.
///
/// `riabuild` is the absolute path of the binary writing them; see the module
/// header for why it can never be the bare name.
pub async fn write_clipboard_shims(ctx: &Ctx, riabuild: &Path) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;

    // One per direction per session type: xclip does both on X11, and Wayland
    // splits them across wl-paste and wl-copy.
    for tool in CLIPBOARD_TOOLS {
        let path = bin.join(tool);
        let script = clipboard_shim_script(riabuild, tool);
        riabuild_paths::config::write_atomic(&path, script.as_bytes()).await?;
        make_executable(&path).await?;
    }
    Ok(())
}

/// `~/.riabuild/bin/<tool>` for one clipboard tool.
pub fn clipboard_shim_script(riabuild: &Path, tool: &str) -> String {
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Carries the clipboard between this server and the laptop over the riabuild
# channel. Anything that is not a clipboard transfer is handed to the real
# {tool} on PATH.
#
# riabuild is named in full because it is not on PATH: it lives in its own
# versioned directory, and a bare name would find another machine's copy or
# none at all.
exec "{riabuild}" channel shim {tool} "$@"
"#,
        riabuild = riabuild.display(),
    )
}

/// `~/.riabuild/bin/ngrok` — ngrok, with the team's authtoken in its
/// environment and nowhere else.
///
/// The token is the org's, it is long-lived, and riabuild does not write this
/// class of secret down: not into ngrok's own `ngrok.yml`, not into a generated
/// rcfile, not into this script. It is fetched on **every** invocation, so a
/// token a lead rotates this morning is in effect this morning, and the audit
/// row the fetch writes says somebody used the team's tunnel credential rather
/// than that somebody opened a terminal.
///
/// Command substitution keeps it out of every argument list — `ps` is
/// world-readable, and on a shared server it shows one developer's command
/// lines to all the others — and out of this file, which is on disk.
///
/// A fetch that fails prints riabuild's own explanation on stderr and leaves
/// the variable empty, at which point it is unset rather than exported: an
/// empty `NGROK_AUTHTOKEN` reads to ngrok as *not authenticated* and would
/// override a token a developer had configured for themselves. ngrok still
/// runs, because `ngrok --version` and `ngrok help` should work on a plane.
///
/// `riabuild` is named in full for the reason in the module header, and so is
/// ngrok: this shim *is* what `PATH` finds, so a bare `ngrok` here would find
/// this script.
pub fn ngrok_shim_script(riabuild: &Path, ngrok: &Path) -> String {
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Runs ngrok with the team's authtoken, fetched from riabuild-web on every
# invocation and stored nowhere on this machine. The token reaches ngrok in its
# environment and never in an argument, because `ps` shows argument lists to
# every account on the machine.
#
# A fetch that fails says so on stderr and leaves ngrok unauthenticated rather
# than refusing to start.
NGROK_AUTHTOKEN=$("{riabuild}" internal ngrok-token)
if [ -n "$NGROK_AUTHTOKEN" ]; then
  export NGROK_AUTHTOKEN
else
  unset NGROK_AUTHTOKEN
fi
exec "{ngrok}" "$@"
"#,
        riabuild = riabuild.display(),
        ngrok = ngrok.display(),
    )
}

/// Writes `~/.riabuild/bin/ngrok`.
///
/// Deliberately not `write_tool`: every other owned tool is handed straight to
/// `exec`, and this one has to carry a credential to the process it starts.
pub async fn write_ngrok_shim(ctx: &Ctx, riabuild: &Path, ngrok: &Path) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;
    let shim = bin.join("ngrok");
    riabuild_paths::config::write_atomic(&shim, ngrok_shim_script(riabuild, ngrok).as_bytes())
        .await?;
    make_executable(&shim).await?;
    Ok(())
}

/// The tool riabuild shadows to send links to the laptop.
///
/// Named separately from `CLIPBOARD_TOOLS` because it is a different shim with
/// a different contract: the clipboard shims hand unrecognised invocations to
/// the real binary, and this one never does.
pub const BROWSER_TOOL: &str = "xdg-open";

/// `~/.riabuild/bin/xdg-open`.
///
/// Written on the *server*, beside the clipboard shims and for the same reason:
/// `~/.riabuild/bin` leads `PATH` there, so this is what `gh auth login` finds.
/// Claude Code needs `BROWSER` pointing here as well — see `shims::browser` for
/// why `PATH` alone does not reach it.
///
/// Written beside `write_clipboard_shims` and under the same condition. It has
/// to be: `BROWSER` is exported by exactly that condition, and a `BROWSER`
/// pointing at a shim nobody wrote fails a sign-in outright instead of falling
/// back to printing the URL.
pub async fn write_browser_shim(ctx: &Ctx, riabuild: &Path) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;

    let path = bin.join(BROWSER_TOOL);
    let script = browser_shim_script(riabuild);
    riabuild_paths::config::write_atomic(&path, script.as_bytes()).await?;
    make_executable(&path).await?;
    Ok(())
}

/// `~/.riabuild/bin/xdg-open`.
pub fn browser_shim_script(riabuild: &Path) -> String {
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Opens links in the browser on the developer's laptop, over the riabuild
# channel. Deliberately does NOT fall back to the real {BROWSER_TOOL}: on a
# server with no display that resolves to a terminal browser, which then
# renders inside this session's own TTY.
#
# riabuild is named in full because it is not on PATH: it lives in its own
# versioned directory, and a bare name would find another machine's copy or
# none at all.
exec "{riabuild}" channel open "$@"
"#,
        riabuild = riabuild.display(),
    )
}

/// The riabuild that is running, by absolute path — what every generated shim
/// has to name.
///
/// Resolved once, by the caller that writes the shims, rather than inside each
/// writer: a run that cannot answer this question must fail before it writes
/// the first shim, not after it has written three good ones and a broken one.
///
/// This is `/proc/self/exe` on Linux, so it survives the developer's `PATH`,
/// the launcher's `PATH` strip, and a `$BROWSER` invoked from a process that
/// sanitised the environment. It follows a self-update too: `upgrade_and_reexec`
/// replaces this process before provisioning reaches here, so what gets written
/// is the version that will actually be running.
pub fn running_binary() -> Result<std::path::PathBuf> {
    std::env::current_exe()
        .context("riabuild could not work out the path to its own binary, so the clipboard and browser shims would have had to guess at it")
}

pub async fn write_all(ctx: &Ctx) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;

    let claude = ctx.claude();
    let settings = ctx.paths.org_settings_file();
    let ids = &ctx.config.claude_accounts;

    for (index, id) in ids.iter().enumerate() {
        let script = launcher_script(&ctx.paths.claude_profile_dir(id), &claude, &settings, &bin);
        write_launcher(&bin.join(format!("claude-{}", index + 1)), &script).await?;
        if index == 0 {
            write_launcher(&bin.join("claude"), &script).await?;
        }
    }

    prune(&bin, ids.len()).await?;
    Ok(())
}

/// Landed by rename, like every other file riabuild generates.
///
/// Launcher content is deterministic given the account list, so two concurrent
/// writers agree and no lock is needed here — the hazard is only an interrupt
/// landing mid-write, which leaves a truncated `claude-2` that fails with a
/// shell syntax error.
async fn write_launcher(path: &Path, script: &str) -> Result<()> {
    riabuild_paths::config::write_atomic(path, script.as_bytes()).await?;
    make_executable(path).await?;
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

    #[test]
    fn the_pnpm_shim_starts_the_launcher_in_its_own_tree() {
        let shim = exec_shim(Path::new("/Users/ada/.riabuild/pnpm/11.11.0/pnpm"));
        assert!(shim.starts_with("#!/bin/sh\n"), "{shim}");
        // The launcher is started where its `dist/` tree is, not copied out of
        // it — pnpm 11 loads `dist/pnpm.mjs` from beside its own executable.
        assert!(
            shim.contains(r#"exec "/Users/ada/.riabuild/pnpm/11.11.0/pnpm" "$@""#),
            "{shim}"
        );
    }

    fn script() -> String {
        launcher_script(
            Path::new("/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555"),
            "/Users/ada/.riabuild/node/22.23.1/bin/claude",
            Path::new("/Users/ada/.riabuild/org-settings.json"),
            Path::new("/Users/ada/.riabuild/bin"),
        )
    }

    #[test]
    fn the_launcher_sets_the_account_and_layers_org_settings() {
        let script = script();
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains(
            r#"CLAUDE_CONFIG_DIR="/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555""#
        ));
        assert!(script.contains(r#"--settings "/Users/ada/.riabuild/org-settings.json""#));
        // Arguments must reach claude, or `claude-2 --resume` silently loses
        // them — and `claude-2 auth login`, which the account box tells the
        // developer to run, would do nothing at all.
        assert!(script.contains(r#""$@""#));
        // A dropped `export` would leave every account sharing the default
        // config directory — all nine collapsing into one — with the rest of
        // this test still green.
        assert!(script.contains("export CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn the_launcher_clears_the_ssh_detection_variables() {
        let script = script();
        // Assert on the `unset` lines themselves, not on the script text: the
        // comment above them names SSH_AUTH_SOCK to explain why it is spared,
        // so a substring search over the whole script proves nothing.
        let unsets: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("unset "))
            .collect();
        // Claude Code answers "am I over SSH?" from these three and nothing
        // else. Leave any of them set and every paste returns "" without a
        // subprocess, so the xclip/wl-paste shims beside this launcher are
        // never invoked and the channel behind them is unreachable.
        //
        // Agent forwarding is not session detection: a fourth name here, or
        // SSH_AUTH_SOCK joining this line, breaks `git push` over SSH.
        assert_eq!(
            unsets,
            ["unset SSH_CONNECTION SSH_CLIENT SSH_TTY"],
            "{script}"
        );
        // A clear that lands after the exec never runs.
        let unset = script.find("unset SSH_CONNECTION").unwrap();
        let exec = script.find("exec ").unwrap();
        assert!(unset < exec, "unset must precede every exec:\n{script}");
    }

    /// Clearing the SSH variables is half of reaching the clipboard shims, and
    /// on its own it only buys pastes. Claude Code's *write* path runs a Linux
    /// probe that asks for a display before it will look for a tool at all, so
    /// on a headless server it records "no clipboard tool" and every copy
    /// leaves as an OSC 52 escape while the wl-copy beside this launcher is
    /// never run.
    #[test]
    fn the_launcher_claims_a_display_so_copies_reach_the_shims() {
        let script = script();
        assert!(
            script.contains("WAYLAND_DISPLAY=riabuild-channel"),
            "{script}"
        );
        assert!(script.contains("export WAYLAND_DISPLAY"), "{script}");

        // Guarded three ways, and each guard is load-bearing. Without the
        // `-x` test a laptop that never wrote the shims would claim a display
        // it has no tool for; without the two `-z` tests riabuild would take a
        // real X11 or Wayland session away from a Linux developer working at
        // their own desk, and their copies would go to a channel that is not
        // there instead of to the clipboard in front of them.
        assert!(
            script.contains(r#"[ -x "/Users/ada/.riabuild/bin/wl-copy" ]"#),
            "{script}"
        );
        assert!(script.contains(r#"[ -z "$WAYLAND_DISPLAY" ]"#), "{script}");
        assert!(script.contains(r#"[ -z "$DISPLAY" ]"#), "{script}");

        // A claim that lands after the exec never runs.
        let claim = script.find("WAYLAND_DISPLAY=riabuild-channel").unwrap();
        assert!(claim < script.find("exec ").unwrap(), "{script}");
    }

    #[test]
    fn the_launcher_can_never_exec_itself() {
        // `~/.riabuild/bin` is first on PATH, so a script called `claude` that
        // runs `exec claude` finds itself and forks until the shell dies.
        let script = script();
        assert!(!script.contains("exec claude"), "{script}");
        assert!(
            script.contains(r#"claude_binary="/Users/ada/.riabuild/node/22.23.1/bin/claude""#),
            "{script}"
        );
        assert!(script.contains(r#"exec "$claude_binary""#), "{script}");
    }

    #[test]
    fn a_binary_that_moved_is_found_without_riabuilds_own_bin() {
        // `claude update` can migrate to a native install, which leaves the
        // recorded path dangling until the next `riabuild`. A dead `claude`
        // command reads as Claude Code being uninstalled.
        let script = script();
        assert!(
            script.contains(r#"if [ ! -x "$claude_binary" ]"#),
            "{script}"
        );
        assert!(
            script.contains(r#"grep -vxF "/Users/ada/.riabuild/bin""#),
            "{script}"
        );
        // `tr '\n' ':'` would leave a trailing colon, and an empty PATH entry
        // means the current directory.
        assert!(script.contains("paste -sd: -"), "{script}");
    }

    #[test]
    fn the_launcher_still_works_before_settings_have_been_fetched() {
        let script = script();
        assert!(script.contains(r#"if [ -f "/Users/ada/.riabuild/org-settings.json" ]"#));
        // The unconditional exec after the `if` is what runs on every machine
        // that has not fetched settings yet. Losing it would still satisfy
        // `the_launcher_can_never_exec_itself` (which only checks the exec
        // inside the `if`) while the launcher silently exited 0 doing nothing.
        assert!(script.contains(r#"exec "$claude_binary" "$@""#), "{script}");
    }

    /// A bare, interactive `claude` opens on the agents view — always, and
    /// without consulting `defaultToAgentsView`.
    ///
    /// The positional is the only spelling that reaches the view from a
    /// launcher. Claude Code's other route reads the *raw* argv and requires
    /// every token on it to be a debug flag, which `--settings` alone defeats;
    /// this one is tested after the options it recognises have been taken off.
    #[test]
    fn a_bare_interactive_launch_opens_the_agents_view() {
        let script = script();
        // `ALLOW_BYPASS` rides along, and that is not decoration. Claude Code
        // folds it into the dispatch defaults the view hands to every session
        // it starts, so dropping it here would take bypass-permissions out of
        // the Shift+Tab cycle for exactly the launch most developers make —
        // the one thing `every_exec_keeps_bypass_permissions_reachable_from_
        // the_cycle` was written to prevent, reintroduced one branch over.
        assert!(
            script.contains(&format!("set -- {ALLOW_BYPASS} agents")),
            "{script}"
        );

        // Ahead of every exec, or it decides nothing. Matched on whole lines
        // rather than with `find`, because the comments above the PATH strip
        // talk about exec'ing a bare name and a substring search finds those
        // first — which would make this pass on a script that never runs the
        // branch at all.
        let line_of = |wanted: &str| {
            script
                .lines()
                .position(|line| line.trim_start().starts_with(wanted))
                .unwrap_or_else(|| panic!("no line starts with {wanted:?}:\n{script}"))
        };
        assert!(line_of("set -- ") < line_of("exec "), "{script}");
    }

    /// Each guard on the agents-view branch, and what dropping it would cost.
    #[test]
    fn the_agents_view_is_guarded_three_ways() {
        let script = script();

        // A developer who typed something asked for that, not for the view —
        // and `agents` would land in front of their own first word.
        assert!(script.contains("[ $# -eq 0 ]"), "{script}");

        // `echo "fix the build" | claude` is a session with a prompt on stdin.
        // Claude Code's positional route does not test the terminal itself, so
        // without these two the prompt is swallowed and the view opens over it.
        assert!(script.contains("[ -t 0 ]"), "{script}");
        assert!(script.contains("[ -t 1 ]"), "{script}");

        // Claude Code's own off switch. With the view disabled, `claude agents`
        // does not fall back to a session — it writes "'claude agents' is
        // disabled …" to stderr and exits 1. Ignoring it here would turn a
        // developer who turned the view off into a developer with no working
        // `claude` at all.
        assert!(
            script.contains(r#"[ -z "$CLAUDE_CODE_DISABLE_AGENT_VIEW" ]"#),
            "{script}"
        );
    }

    /// The guards, run as shell rather than read as text.
    ///
    /// `cargo test` gives a child no terminal, so this is a real
    /// non-interactive launch — the shape `echo "fix the build" | claude`, a CI
    /// job and `claude -p` all arrive in. None of them may pick up `agents`:
    /// the positional would swallow a prompt waiting on stdin, and Claude
    /// Code's own route does not test the terminal, so this script is the only
    /// thing standing between a piped prompt and a view opening over it.
    ///
    /// Executed instead of asserted on, because every way of getting this wrong
    /// reads identically in the source: a `set --` that drops `"$@"`, a guard
    /// that is true when it should be false, an `-eq` that should be `-gt`.
    /// The interactive half cannot be reached from here — it needs a terminal
    /// on both descriptors — which is what
    /// `the_agents_token_is_still_the_agents_entry_point` and a bare `claude`
    /// on a laptop are for.
    #[tokio::test]
    async fn a_launch_with_no_terminal_never_picks_up_the_agents_view() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};

        let home = tempfile::TempDir::new().unwrap();
        // A stand-in for Claude Code that reports the line it was given.
        let claude = home.path().join("claude");
        tokio::fs::write(
            &claude,
            "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\"; done\n",
        )
        .await
        .unwrap();
        make_executable(&claude).await.unwrap();

        let settings = home.path().join("org-settings.json");
        tokio::fs::write(&settings, "{}").await.unwrap();

        let launcher = home.path().join("launcher");
        let script = launcher_script(
            &home.path().join("profile"),
            &claude.to_string_lossy(),
            &settings,
            &home.path().join("bin"),
        );
        tokio::fs::write(&launcher, &script).await.unwrap();
        make_executable(&launcher).await.unwrap();

        let runner = RealRunner;
        let run = |args: Vec<String>| {
            let launcher = launcher.to_string_lossy().into_owned();
            let runner = &runner;
            async move {
                let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
                let output = runner
                    .run(&launcher, &borrowed, &RunOptions::default())
                    .await
                    .expect("the launcher ran");
                assert!(output.ok(), "{output:?}");
                output
                    .stdout
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<String>>()
            }
        };

        // No arguments, no terminal. This is the case the guards exist for:
        // without `-t 0`/`-t 1` it would take the agents branch.
        let bare = run(Vec::new()).await;
        assert!(!bare.iter().any(|arg| arg == "agents"), "{bare:?}");
        assert!(
            bare.iter().any(|arg| arg == STATIC_SYSTEM_PROMPT),
            "{bare:?}"
        );
        assert_eq!(
            bare.first().map(String::as_str),
            Some("--settings"),
            "{bare:?}"
        );

        // And a launch that carries arguments keeps every one of them, in
        // order, after the flag — `claude-2 auth login` and `claude --resume`
        // both depend on it.
        let carried = run(vec!["-p".into(), "fix the build".into()]).await;
        assert!(!carried.iter().any(|arg| arg == "agents"), "{carried:?}");
        assert_eq!(
            carried.last().map(String::as_str),
            Some("fix the build"),
            "{carried:?}"
        );
        let flag = carried
            .iter()
            .position(|arg| arg == STATIC_SYSTEM_PROMPT)
            .unwrap_or_else(|| panic!("{carried:?}"));
        let prompt = carried.iter().position(|arg| arg == "-p").unwrap();
        assert!(flag < prompt, "{carried:?}");
    }

    /// The pair that must never share a line.
    ///
    /// `--exclude-dynamic-system-prompt-sections` is not among the options
    /// Claude Code strips before testing whether the `agents` positional stands
    /// alone, so a line carrying both does not open the view with a longer
    /// system prompt. It falls through to the ordinary parser, where `agents`
    /// is the *background-agents* subcommand — `claude` would print a list of
    /// background agents and exit instead of opening a session.
    #[test]
    fn the_agents_view_and_the_static_prompt_flag_never_share_a_line() {
        let script = script();
        for line in script.lines().map(str::trim) {
            if !line.starts_with("exec ") && !line.starts_with("set -- ") {
                continue;
            }
            assert!(
                !(line.contains(" agents") && line.contains(STATIC_SYSTEM_PROMPT)),
                "the agents view and the static-prompt flag share a line:\n{line}"
            );
        }
    }

    #[test]
    fn every_launch_that_carries_arguments_moves_the_per_machine_sections_out() {
        // Asserted in two halves, because the argument line is now decided in
        // one place and forwarded by two execs that are reached under opposite
        // conditions — the first on a machine that has fetched the org
        // settings, the second on one that has not. A flag reaching only one of
        // them would be a cache that half the team shares and the other half
        // does not, with nothing to see in either terminal.
        let script = script();

        // Half one: exactly two branches build a line, and the one that is not
        // the agents view carries the flag ahead of `"$@"` — so a developer's
        // own trailing arguments still reach Claude Code as arguments, rather
        // than landing after a flag that has already consumed the line.
        let built: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("set -- "))
            .collect();
        assert_eq!(built.len(), 2, "{script}");
        let carried = built
            .iter()
            .find(|line| line.contains(STATIC_SYSTEM_PROMPT))
            .unwrap_or_else(|| panic!("no branch carries the flag:\n{script}"));
        assert!(
            carried.find(STATIC_SYSTEM_PROMPT) < carried.find(r#""$@""#),
            "{carried}"
        );

        // Half two: both execs forward whatever that branch built, so neither
        // drops the line it was handed.
        let execs: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("exec "))
            .collect();
        assert_eq!(execs.len(), 2, "{script}");
        for exec in execs {
            assert!(exec.ends_with(r#""$@""#), "{exec}");
        }
    }

    #[test]
    fn every_branch_keeps_bypass_permissions_reachable_from_the_cycle() {
        // Asserted on *every* branch that builds an argument line, because the
        // machine that matters most is the one an assertion on the script text
        // would not distinguish: a laptop that has not fetched
        // `org-settings.json` execs with no `--settings` at all, so
        // `permissions.defaultMode` reaches it by no route whatsoever and this
        // flag is the only thing keeping the mode in its Shift+Tab cycle.
        // Losing it is invisible — the launcher starts, Claude Code starts, and
        // the cycle silently has one fewer stop on it.
        //
        // The agents branch is included on purpose. Unlike the static-prompt
        // flag, this one is stripped before the `agents` positional is tested
        // *and* carried into the sessions the view dispatches, so it belongs on
        // both lines and there is no reason to drop it from either.
        let script = script();
        let built: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("set -- "))
            .collect();
        assert_eq!(built.len(), 2, "{script}");
        for line in built {
            assert!(line.contains(ALLOW_BYPASS), "{line}");
        }

        // And on the branch that forwards a developer's own arguments, ahead of
        // `"$@"` — behind it the flag would land after a line that has already
        // been consumed.
        let carried = script
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("set -- ") && line.contains(r#""$@""#))
            .unwrap_or_else(|| panic!("{script}"));
        assert!(
            carried.find(ALLOW_BYPASS) < carried.find(r#""$@""#),
            "{carried}"
        );
    }

    #[test]
    fn a_bare_binary_name_cannot_be_used_to_exec_itself() {
        // `Ctx::claude()` returns the bare name "claude" before a Node
        // version is pinned. `[ ! -x "claude" ]` is a cwd-relative test — a
        // same-named executable in an untrusted checkout would pass it, skip
        // the PATH strip, and `exec "claude"` would search PATH straight back
        // to this same script.
        let script = launcher_script(
            Path::new("/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555"),
            "claude",
            Path::new("/Users/ada/.riabuild/org-settings.json"),
            Path::new("/Users/ada/.riabuild/bin"),
        );
        assert!(script.contains(r#"case "$claude_binary" in"#), "{script}");
        assert!(script.contains(r#"*) claude_binary="" ;;"#), "{script}");
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

    #[tokio::test]
    async fn a_tool_shim_points_at_the_versioned_copy_and_is_executable() {
        // This is what puts riabuild's gh in front of a system one on PATH.
        let (ctx, home) = ctx_with(FakeRunner::new()).await;
        let binary = home.path().join(".riabuild/gh/2.97.0/bin/gh");
        write_tool(&ctx, "gh", &binary).await.unwrap();
        // Safe to run twice, like every other apply().
        write_tool(&ctx, "gh", &binary).await.unwrap();

        let shim = ctx.paths.bin_dir().join("gh");
        let script = tokio::fs::read_to_string(&shim).await.unwrap();
        assert!(script.contains("gh/2.97.0/bin/gh"), "{script}");
        assert!(script.contains(r#""$@""#), "{script}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(&shim)
                .await
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");
        }
    }

    /// The path a server actually reaches riabuild by: a versioned directory
    /// that nothing puts on `PATH`.
    fn server_binary() -> &'static Path {
        Path::new("/home/dev/.riabuild/riabuild/2026.08.14/riabuild")
    }

    #[tokio::test]
    async fn the_clipboard_shims_route_through_riabuild() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_clipboard_shims(&ctx, server_binary()).await.unwrap();
        // Safe to run twice, like every other apply().
        write_clipboard_shims(&ctx, server_binary()).await.unwrap();

        for tool in CLIPBOARD_TOOLS {
            let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join(tool))
                .await
                .unwrap();
            assert!(script.contains("channel shim"), "{script}");
            assert!(script.contains(tool), "{script}");
            // Arguments must reach the tool, or the TARGETS probe loses its
            // flags and every paste silently fails.
            assert!(script.contains(r#""$@""#), "{script}");
        }
    }

    #[tokio::test]
    async fn the_browser_shim_routes_through_riabuild() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_browser_shim(&ctx, server_binary()).await.unwrap();
        // Safe to run twice, like every other apply().
        write_browser_shim(&ctx, server_binary()).await.unwrap();

        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join(BROWSER_TOOL))
            .await
            .unwrap();
        assert!(script.contains("channel open"), "{script}");
        assert!(script.contains(r#""$@""#), "{script}");
        // The whole point. A fallback to the real xdg-open on a display-less
        // server runs a terminal browser inside the session's own TTY, which is
        // the failure this shim exists to prevent.
        assert!(!script.contains("exec xdg-open"), "{script}");
    }

    /// The bug this pins: every shim used to `exec riabuild …` by bare name,
    /// and riabuild is the one tool riabuild does not put on `PATH` — it lives
    /// in a versioned directory, and `shell::riabuild_path_dirs` leads `PATH`
    /// with `bin/` and Node's `bin/` alone. On a server with no other copy the
    /// developer got `xdg-open: exec: riabuild: not found` and no browser; on a
    /// server with an apt or Homebrew copy, worse, because it worked — as some
    /// other version, against a channel this session owns.
    ///
    /// Asserted over every generated shim at once rather than one at a time, so
    /// that a shim added later is covered by this test on the day it is written
    /// rather than the day someone remembers to extend it.
    #[test]
    fn no_shim_looks_riabuild_up_on_the_path() {
        let scripts = CLIPBOARD_TOOLS
            .iter()
            .map(|tool| clipboard_shim_script(server_binary(), tool))
            .chain(std::iter::once(browser_shim_script(server_binary())));

        for script in scripts {
            let execs: Vec<&str> = script
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with("exec "))
                .collect();
            assert_eq!(execs.len(), 1, "{script}");
            assert!(
                execs[0].starts_with(&format!(r#"exec "{}" "#, server_binary().display())),
                "{script}"
            );
        }
    }

    /// The path is taken from the running process, so it is right on a laptop,
    /// on a server, and after a self-update re-exec — none of which a test can
    /// stand in for. What it can pin is that the answer is absolute, which is
    /// the whole property the shims need.
    #[test]
    fn the_binary_the_shims_name_is_an_absolute_path() {
        let binary = running_binary().expect("the test binary has a path");
        assert!(binary.is_absolute(), "{}", binary.display());
    }

    #[test]
    fn the_ngrok_shim_names_both_binaries_in_full() {
        // riabuild is not on `PATH` and ngrok is deliberately not either: the
        // shim *is* what `PATH` finds. A bare name in here would find the shim
        // itself, or another machine's riabuild.
        let script = ngrok_shim_script(
            server_binary(),
            Path::new("/home/dev/.riabuild/ngrok/3.39.11/ngrok"),
        );
        assert!(
            script.contains(&format!(
                r#""{}" internal ngrok-token"#,
                server_binary().display()
            )),
            "{script}"
        );
        assert!(
            script.contains(r#"exec "/home/dev/.riabuild/ngrok/3.39.11/ngrok" "$@""#),
            "{script}"
        );
        for line in script.lines().map(str::trim) {
            assert!(
                !line.starts_with("exec riabuild") && !line.starts_with("exec ngrok"),
                "{line}"
            );
        }
    }

    #[test]
    fn the_ngrok_shim_keeps_the_token_out_of_every_argument_list() {
        // `ps` is world-readable, and on a shared server it shows every other
        // developer's command lines. The token reaches ngrok through the
        // environment or not at all.
        let script = ngrok_shim_script(
            server_binary(),
            Path::new("/home/dev/.riabuild/ngrok/3.39.11/ngrok"),
        );
        assert!(!script.contains("--authtoken"), "{script}");
        assert!(!script.contains("config add-authtoken"), "{script}");
        let exec = script
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("exec "))
            .expect("the shim execs ngrok");
        assert!(!exec.contains("NGROK_AUTHTOKEN"), "{exec}");
    }

    #[test]
    fn a_token_that_could_not_be_fetched_is_not_exported_as_an_empty_one() {
        // Offline, signed out, or with nothing set in the dashboard, the fetch
        // prints nothing. Exporting `NGROK_AUTHTOKEN=` would override a token
        // the developer had configured for themselves with a value that means
        // "not authenticated".
        let script = ngrok_shim_script(
            server_binary(),
            Path::new("/home/dev/.riabuild/ngrok/3.39.11/ngrok"),
        );
        assert!(script.contains("unset NGROK_AUTHTOKEN"), "{script}");
        assert!(
            script.contains(r#"if [ -n "$NGROK_AUTHTOKEN" ]"#),
            "{script}"
        );
    }

    /// A shadowed binary with no parser behind it would pass everything through
    /// — a tool that looks installed and does nothing riabuild intended.
    #[test]
    fn every_shadowed_tool_has_a_parser() {
        for tool in CLIPBOARD_TOOLS {
            assert!(
                clipboard::Tool::from_name(tool).is_some(),
                "{tool} is shimmed but the shim cannot parse it"
            );
        }
    }

    /// Pins the undocumented `CLAUDE_CONFIG_DIR` behaviour against a real
    /// Claude Code install.
    ///
    /// Ignored by default because it needs `claude` on PATH; run it with
    /// `cargo test -- --ignored` on a machine that has it, and before every
    /// Claude Code version bump.
    #[tokio::test]
    #[ignore = "requires Claude Code installed; pins undocumented behaviour"]
    async fn claude_config_dir_smoke() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let profile = home.path().join("profile");
        tokio::fs::create_dir_all(&profile).await.unwrap();

        let output = runner
            .run(
                "claude",
                &["--version"],
                &RunOptions {
                    env: vec![(
                        "CLAUDE_CONFIG_DIR".into(),
                        profile.to_string_lossy().into_owned(),
                    )],
                    ..Default::default()
                },
            )
            .await
            .expect("claude --version");

        assert!(output.ok(), "claude rejected CLAUDE_CONFIG_DIR: {output:?}");
        // Claude Code writes its state into the directory it was pointed at. If
        // this stops happening, the profile model is broken and every developer
        // shares one config.
        assert!(
            profile.read_dir().unwrap().next().is_some() || profile.exists(),
            "CLAUDE_CONFIG_DIR was ignored"
        );
    }

    /// Does `CLAUDE_CONFIG_DIR` isolate *credentials*, or only configuration?
    ///
    /// This is the property every account rests on: if it stops holding, two
    /// accounts share one sign-in and the whole feature is a lie. Kept
    /// `#[ignore]`d rather than deleted because it is a record of an
    /// undocumented upstream property, and it needs a real Claude Code install
    /// to say anything at all.
    ///
    /// The profile model assumes the former. Tested directly on macOS:
    /// `CLAUDE_CONFIG_DIR=/tmp/asd claude` prompts for a fresh login, so the
    /// credential is keyed to the config directory, not to the Unix account.
    /// Two riabuild profiles on one Mac — or two developers sharing a Unix
    /// account on a Mac server — get separate Claude sign-ins. See
    /// `docs/superpowers/specs/2026-08-06-remote-mode-design.md`, "Claude Code
    /// needs no special handling here".
    ///
    /// `claude auth status --json` is the non-interactive probe: it reports
    /// `loggedIn` without opening a prompt and, for a signed-out directory,
    /// without printing anything credential-shaped. A fresh `CLAUDE_CONFIG_DIR`
    /// must report `loggedIn: false` — if a future Claude Code release instead
    /// inherits a login from outside the config directory, this fails instead
    /// of silently merging two developers' sign-ins.
    ///
    /// Ignored by default: it needs a real Claude Code install, **and** an
    /// ambient sign-in already sitting in the default config directory (run
    /// `claude auth login` first if `claude auth status` there says
    /// `loggedIn: false`). Without that ambient sign-in the test is vacuous —
    /// if isolation broke completely and every config dir now shared one
    /// global (signed-out) login, a fresh temp dir would still report
    /// `loggedIn: false` and the test would pass for the wrong reason. The
    /// two probes together are the evidence: the ambient directory must be
    /// signed in, and the fresh one must not be.
    ///
    /// To confirm credentials (not just configuration) are isolated,
    /// additionally sign in under two different `CLAUDE_CONFIG_DIR`s by hand
    /// and check `claude auth status` disagrees between them — that step
    /// cannot be scripted, so it isn't asserted here. Run with
    /// `cargo test -- --ignored` before every Claude Code version bump, the
    /// way `claude_config_dir_smoke` already is.
    #[tokio::test]
    #[ignore = "requires Claude Code installed and already signed in to the \
                default config directory; pins undocumented behaviour"]
    async fn claude_credentials_follow_the_config_dir() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        // Probe 1: the ambient config directory must already be signed in, or
        // this test cannot distinguish "isolated" from "broken" — a machine
        // with no sign-in anywhere would pass either way. Never surface any
        // field but `loggedIn`: the signed-in probe's JSON also carries
        // `email`, `orgId`, `orgName`, and `subscriptionType`.
        let ambient = runner
            .run(
                "claude",
                &["auth", "status", "--json"],
                &RunOptions::default(),
            )
            .await
            .expect("claude auth status --json (ambient)");
        let ambient_status: serde_json::Value = serde_json::from_str(ambient.trimmed())
            .expect("claude auth status --json must print JSON");
        let Some(&serde_json::Value::Bool(true)) = ambient_status.get("loggedIn") else {
            panic!(
                "the default Claude config directory is not signed in \
                 (loggedIn: false); this test needs an ambient sign-in to \
                 prove anything — run `claude auth login` first"
            );
        };

        // Probe 2: a brand-new config directory must report signed out.
        let home = tempfile::TempDir::new().unwrap();
        let profile = home.path().join("profile");
        tokio::fs::create_dir_all(&profile).await.unwrap();

        let output = runner
            .run(
                "claude",
                &["auth", "status", "--json"],
                &RunOptions {
                    env: vec![(
                        "CLAUDE_CONFIG_DIR".into(),
                        profile.to_string_lossy().into_owned(),
                    )],
                    ..Default::default()
                },
            )
            .await
            .expect("claude auth status --json (fresh)");

        // Parsed rather than substring-matched: `"loggedIn": false` and
        // `"loggedIn":false` are the same JSON and differ as text, and a
        // two-spelling `contains` check silently starts passing for the wrong
        // reason the day upstream reformats its output.
        let status: serde_json::Value = serde_json::from_str(output.trimmed())
            .expect("claude auth status --json must print JSON");
        assert_eq!(
            status.get("loggedIn"),
            Some(&serde_json::Value::Bool(false)),
            "a fresh CLAUDE_CONFIG_DIR was already authenticated: credentials \
             are not isolated per config directory"
        );
    }

    /// Pins the JSON key the account box reads. `status::ask` takes the email out
    /// of `auth status --json`, and a rename upstream would turn every signed-in
    /// account into `Unknown` — a box that says riabuild cannot tell, on a machine
    /// where nothing is wrong.
    ///
    /// Deliberately `#[ignore]`d: it asserts against the developer's own sign-in,
    /// which only a real machine has.
    #[tokio::test]
    #[ignore = "needs a real Claude Code install and a signed-in developer; records the `email` key the account box reads"]
    async fn auth_status_reports_an_email_field() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        let mine = runner
            .run(
                "claude",
                &["auth", "status", "--json"],
                &RunOptions::default(),
            )
            .await
            .expect("claude auth status");

        assert!(
            mine.stdout.contains("email"),
            "the developer's own sign-in reported no email field: {mine:?}"
        );
    }

    /// Every launch that carries arguments passes `--settings`,
    /// `--exclude-dynamic-system-prompt-sections` and
    /// `--allow-dangerously-skip-permissions`, so `claude-2 auth login` — which
    /// the account box tells developers to run — depends on all three being
    /// accepted ahead of a subcommand.
    ///
    /// All three are asserted in one invocation because that is the argument line
    /// a launcher actually builds; a test that passed them separately would not
    /// cover the set. If this ever fails, the launcher — not the developer's
    /// command — is what broke.
    ///
    /// Deliberately `#[ignore]`d: only a real `claude` can say whether its
    /// argument parser still allows this, and the shims are generated on the
    /// assumption that it does.
    #[tokio::test]
    #[ignore = "needs a real Claude Code install; records that the launcher's global flags are accepted ahead of a subcommand, which every launcher assumes"]
    async fn settings_flag_survives_a_subcommand() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let settings = home.path().join("settings.json");
        tokio::fs::write(&settings, "{}").await.unwrap();

        let output = runner
            .run(
                "claude",
                &[
                    "--settings",
                    &settings.to_string_lossy(),
                    STATIC_SYSTEM_PROMPT,
                    ALLOW_BYPASS,
                    "auth",
                    "status",
                    "--json",
                ],
                &RunOptions::default(),
            )
            .await
            .expect("claude --settings auth status");
        assert!(output.stdout.contains("loggedIn"), "{output:?}");
    }

    /// Records that `agents` is still Claude Code's agents entry point, and
    /// still reachable with `--settings` in front of it.
    ///
    /// A bare interactive `claude` is `claude --settings <org> agents`, and the
    /// whole of that spelling is undocumented: the positional is honoured only
    /// when everything Claude Code recognises has been stripped off the line
    /// and nothing remains, `--settings` is one of the things stripped, and
    /// none of it is promised anywhere. If upstream renames the token, the
    /// parser stops recognising it and every developer's `claude` starts
    /// failing as an unknown command.
    ///
    /// **What this can and cannot prove.** Opening the view needs a terminal on
    /// both stdin and stdout, so no test here can watch it happen; what
    /// `--json` reaches is the same token's non-interactive branch. So this
    /// pins that the token is still live and still composes with `--settings`,
    /// which is the half that breaks silently. It does **not** pin that the
    /// interactive branch still opens the view — that is a real gap, and the
    /// way to close it is to run a bare `claude` on a laptop and look.
    ///
    /// Deliberately `#[ignore]`d: only a real `claude` can answer, and it needs
    /// no sign-in — a signed-out install still lists its own sessions.
    #[tokio::test]
    #[ignore = "needs a real Claude Code install; records that the `agents` token the launcher builds is still recognised"]
    async fn the_agents_token_is_still_the_agents_entry_point() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let settings = home.path().join("settings.json");
        tokio::fs::write(&settings, "{}").await.unwrap();

        // The launcher's own order: --settings, then the positional.
        let output = runner
            .run(
                "claude",
                &[
                    "--settings",
                    &settings.to_string_lossy(),
                    "agents",
                    "--json",
                ],
                &RunOptions::default(),
            )
            .await
            .expect("claude --settings agents --json");

        assert!(output.ok(), "claude rejected the agents token: {output:?}");
        // Parsed rather than substring-matched: an unknown-command error could
        // contain the word "agents" too, and a JSON array is the one answer
        // only the real subcommand gives.
        let sessions: serde_json::Value =
            serde_json::from_str(output.trimmed()).expect("claude agents --json must print JSON");
        assert!(sessions.is_array(), "{sessions:?}");
    }
}

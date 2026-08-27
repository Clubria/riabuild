//! What one Claude Code launcher says.
//!
//! ```sh
//! # a bare, interactive `claude` — opens on the agents view, scoped to
//! # whichever checkout the developer is standing in
//! CLAUDE_CONFIG_DIR=~/.riabuild/claude/<uuid> claude \
//!   --settings ~/.riabuild/org-settings.json \
//!   --allow-dangerously-skip-permissions \
//!   agents --cwd ~/Clubria/ai-builders-hub
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
//! `--cwd` is a third case again, and it is why "stripped or not" is the wrong
//! question to ask of it. It belongs to the `agents` subcommand rather than to
//! `claude` — `claude --cwd <path> mcp list` is "unknown option" — so it goes
//! *after* the positional instead of ahead of it, and an option in that position
//! costs the view nothing: the launch still opens on it, still with
//! `ALLOW_BYPASS` in force. Which makes it the one thing here that only the bare
//! line can have, where `STATIC_SYSTEM_PROMPT` is the one thing only the other
//! line can have. See `VIEW_CWD`.
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

use std::path::{Path, PathBuf};

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

/// Scopes the agents view to the checkout the developer is actually standing
/// in — never to one fixed repository for the whole machine.
///
/// A **subcommand** option of `agents`, and only of `agents`: `claude --cwd
/// <path> mcp list` is "error: unknown option '--cwd'". So it cannot join
/// `STATIC_SYSTEM_PROMPT` and `ALLOW_BYPASS` ahead of the line — it goes after
/// the positional, on the one launch that reaches the view, and every other
/// launch is left exactly as it was.
///
/// What it does is two things at once, and the second is the one worth having.
/// It filters the session list to the sessions started under that path — which
/// is what the `--help` line says and all it says — *and* it becomes the
/// working directory the view reports and dispatches from. A developer who runs
/// `claude` from their home directory used to get a view listing every session
/// on the machine, from every checkout; now they get whichever repository is
/// under them, or the one riabuild set this machine up for by default.
///
/// It does **not** override a developer who is already somewhere more specific.
/// Claude Code keeps the process's own working directory when that directory is
/// inside the path passed here, and takes this one when it is not — so `claude`
/// from `<checkout>/riabuild-cli`, or from a `.claude/worktrees/` worktree under
/// it, still opens on where they are. "Always" is therefore always *at least*
/// the repository, which is the useful reading of it: the flag pulls a
/// developer who is nowhere near their work back to it, and leaves one who is
/// standing in it alone.
///
/// **Which repository "it" is has to be resolved per launch, not baked in
/// once.** A machine that knows two checkouts — `riabuild` and a second
/// repository picked later — used to get one `--cwd` for both, chosen by
/// whichever repository `riabuild` was last run against: a developer typing
/// `claude` from inside `riabuild` was moved to the *other* checkout, because
/// the floor above only keeps you where you stand when you are already inside
/// the one path the launcher knows. That is not "leaves one who is standing in
/// it alone" — it is "alone" for exactly one checkout, and a wall for every
/// other one riabuild has ever cloned. So the launcher script itself picks:
/// `case "$PWD"` walks every checkout `riabuild` knows about (`build_agents_view`
/// generates one arm per entry in `UserConfig::repos`) and takes whichever one
/// contains the working directory, falling back to the run's default repository
/// only when none of them do. Every developer with one repository sees exactly
/// today's behaviour — one arm, one fallback that agrees with it.
///
/// Passed only where the resolved checkout is on disk. A path that is not there
/// does not fail — the view opens on an empty list naming a directory nobody
/// has — and that is precisely the failure worth not shipping: a `claude`
/// whose view is pinned to a ghost is worse than one that behaves like it did
/// last week.
///
/// Verified against Claude Code 2.1.235, including the thing that would have
/// sunk it: an option after the `agents` positional does not push the launch
/// off the view and into the background-agents listing, the way
/// `STATIC_SYSTEM_PROMPT` ahead of it does. `ALLOW_BYPASS` still lands, too —
/// the view comes up with bypass on its footer. Re-read it when the pinned
/// version moves.
const VIEW_CWD: &str = "--cwd";

/// The branch of the launcher that opens the agents view, scoped to whichever
/// checkout `$PWD` turns out to be under when the script actually runs.
///
/// One `case` arm per entry in `checkouts`, most specific (longest path)
/// first, so a checkout nested inside another — unusual, but not something the
/// generator may assume away — matches the inner one rather than the outer.
/// The pattern for each is `"{path}"|"{path}"/*`: the first alternative is an
/// exact match (`$PWD` is the checkout root itself), the second is quoted
/// literal followed by an *unquoted* `/*`, which is what makes it a prefix
/// match on every subdirectory and worktree beneath it rather than a glob over
/// the checkout's own contents. Determined entirely at shell runtime — nothing
/// here can know where a script's caller will `cd` from before that caller
/// exists.
///
/// The `*` arm is `default`: what a developer standing nowhere riabuild has a
/// checkout for still gets pulled back to, exactly as the single-repository
/// launcher always did. Two spellings of the branch rather than one with an
/// interpolated argument, for the same reason `launcher_script` used to give
/// for its own single-path version: `${{x:+--cwd "$x"}}` would split a path
/// containing a space back into two arguments, and `/Users/Ada Smith/Clubria`
/// is an ordinary macOS home.
///
/// `checkouts` empty and `default` `None` together is the one case with
/// nothing to match against at all — every machine before its first clone —
/// and it is spelled as the plain `agents` line the launcher always wrote
/// rather than a `case` with only a `*` arm, so a machine that has never
/// chosen a repository sees exactly the script it always has.
fn build_agents_view(checkouts: &[PathBuf], default: Option<&Path>) -> String {
    if checkouts.is_empty() && default.is_none() {
        return format!("  set -- {ALLOW_BYPASS} agents");
    }
    let mut arms = String::new();
    for checkout in checkouts {
        let path = checkout.display();
        arms.push_str(&format!(
            "      \"{path}\"|\"{path}\"/*) project=\"{path}\" ;;\n"
        ));
    }
    arms.push_str(&match default {
        Some(default) => format!("      *) project=\"{}\" ;;\n", default.display()),
        None => "      *) project=\"\" ;;\n".to_string(),
    });
    format!(
        r#"  project=""
  case "$PWD" in
{arms}  esac
  if [ -n "$project" ] && [ -d "$project" ]; then
    set -- {bypass} agents {cwd} "$project"
  else
    set -- {bypass} agents
  fi"#,
        bypass = ALLOW_BYPASS,
        cwd = VIEW_CWD,
    )
}

/// One account's launcher: `claude`, or `claude-<n>`.
///
/// `checkouts` is every repository this machine knows a path for —
/// `UserConfig::repos`, in the order the case statement should try them — and
/// `default` is the checkout of the repository the *current* run is about,
/// used only when the developer's `$PWD` matches none of `checkouts`. Both are
/// commonly the same single path, which is what a machine with one repository
/// looks like; `default` is `None` only where there is no checkout at all yet
/// to fall back to — every machine before its first clone.
pub fn launcher_script(
    config_dir: &Path,
    claude: &str,
    org_settings: &Path,
    bin_dir: &Path,
    checkouts: &[PathBuf],
    default: Option<&Path>,
) -> String {
    let agents_view = build_agents_view(checkouts, default);
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
#
# The view opens on whichever checkout the developer is standing in, which is
# what `--cwd` is doing below. It is an option of the `agents` subcommand and
# of nothing else — `claude --cwd <path> mcp list` is "unknown option" — so it
# sits after the positional, and the other branch cannot have it. Unlike
# {flag}, an option in that position does not cost the view: the launch still
# opens on it, still with {bypass} in force. Verified against Claude Code
# 2.1.235.
#
# It is a floor rather than a move. Claude Code keeps the working directory the
# process already has when that directory is *inside* the one named here, so
# `claude` from a subdirectory or from a `.claude/worktrees/` worktree still
# opens where the developer stands; it is the `claude` typed in a home
# directory, or in some unrelated tree, that lands on a repository instead of
# on a list of every session on the machine.
#
# Which repository is decided just above, not baked into this script once:
# `$PWD` is matched against every checkout riabuild knows a path for, and only
# a developer standing nowhere any of them falls back to this run's default —
# so working in a second checkout no longer pulls `claude` away from the first
# one riabuild set this machine up for. Only where the resolved checkout is on
# disk. A path that is gone opens a view onto an empty list naming a directory
# nobody has, which is worse than the view this launcher opened before the
# flag existed.
if [ $# -eq 0 ] && [ -t 0 ] && [ -t 1 ] && [ -z "$CLAUDE_CODE_DISABLE_AGENT_VIEW" ]; then
{agents_view}
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

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_fetch::archive::make_executable;

    /// The checkout the fixture launcher opens its agents view on.
    const PROJECT: &str = "/Users/ada/Clubria/ai-builders-hub";

    /// A second checkout, for the tests about a machine that knows more than
    /// one — the shape that used to lose `--cwd` for every repository but the
    /// last one `riabuild` was run against.
    const OTHER_PROJECT: &str = "/Users/ada/Clubria/payments";

    /// The fixture most tests want: one known checkout, which is also the
    /// run's default — what a machine with a single repository looks like,
    /// and indistinguishable from the pre-multi-repository launcher.
    fn script() -> String {
        script_for(&[PathBuf::from(PROJECT)], Some(Path::new(PROJECT)))
    }

    fn script_for(checkouts: &[PathBuf], default: Option<&Path>) -> String {
        launcher_script(
            Path::new("/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555"),
            "/Users/ada/.riabuild/node/22.23.1/bin/claude",
            Path::new("/Users/ada/.riabuild/org-settings.json"),
            Path::new("/Users/ada/.riabuild/bin"),
            checkouts,
            default,
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

    #[test]
    fn the_agents_view_opens_on_the_checkout() {
        // The feature. Without it a `claude` typed anywhere but the checkout
        // opens a view listing every session on the machine, from every
        // directory the developer has ever worked in.
        let script = script();
        assert!(
            script.contains(&format!(
                r#"set -- {ALLOW_BYPASS} agents {VIEW_CWD} "$project""#
            )),
            "{script}"
        );
        // The path itself lives in the `case` arm that resolves `$project`,
        // not on the exec line — `$PWD` decides which checkout that variable
        // names, so the literal path can only be asserted there.
        assert!(
            script.contains(&format!(
                r#""{PROJECT}"|"{PROJECT}"/*) project="{PROJECT}" ;;"#
            )),
            "{script}"
        );
    }

    /// A machine that knows a second repository still opens `--cwd` on the
    /// first one — the regression this module exists to close. Before
    /// `build_agents_view`, `--cwd` named whichever repository `riabuild` was
    /// *last run against*, machine-wide, so a developer standing in `PROJECT`
    /// while `OTHER_PROJECT` was the more recent run was moved off their own
    /// checkout the moment they typed `claude`.
    #[test]
    fn a_second_known_checkout_gets_its_own_case_arm_rather_than_replacing_the_first() {
        let script = script_for(
            &[PathBuf::from(PROJECT), PathBuf::from(OTHER_PROJECT)],
            Some(Path::new(OTHER_PROJECT)),
        );
        assert!(
            script.contains(&format!(
                r#""{PROJECT}"|"{PROJECT}"/*) project="{PROJECT}" ;;"#
            )),
            "{script}"
        );
        assert!(
            script.contains(&format!(
                r#""{OTHER_PROJECT}"|"{OTHER_PROJECT}"/*) project="{OTHER_PROJECT}" ;;"#
            )),
            "{script}"
        );
        // The default — the repository this run is about — is what a
        // developer standing in neither checkout still falls back to.
        assert!(
            script.contains(&format!(r#"*) project="{OTHER_PROJECT}" ;;"#)),
            "{script}"
        );
        // Exactly one exec line reaches Claude Code with `--cwd`; which path
        // fills `$project` is a runtime question this script can no longer
        // answer by reading its own text — `checkout_matching_pwd_wins_over_
        // the_run_default` proves that half by actually running it.
        assert!(
            script.contains(&format!(r#"agents {VIEW_CWD} "$project""#)),
            "{script}"
        );
    }

    /// Runs the generated `case "$PWD" in …` block for real, from three
    /// different working directories, and reads back which checkout it
    /// resolved to. The text assertions above prove the right literals are in
    /// the right place; this is what proves the shell actually picks the one
    /// `$PWD` is under rather than always falling through to the run's
    /// default — the one property no substring match can stand in for, and
    /// the exact bug report this module exists to close: standing in a known
    /// checkout that happens not to be the most recently active repository
    /// must still resolve to itself.
    ///
    /// Run directly rather than through the full launcher, because the case
    /// block sits inside the interactive branch and `cargo test` gives a
    /// child no terminal — see `a_launch_with_no_terminal_never_picks_up_the_
    /// agents_view`'s own doc comment for why that half is untestable here.
    #[tokio::test]
    async fn checkout_matching_pwd_wins_over_the_run_default() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};

        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join("riabuild");
        let other = home.path().join("payments");
        let elsewhere = home.path().join("elsewhere");
        for dir in [&project, &other, &elsewhere] {
            tokio::fs::create_dir_all(dir).await.unwrap();
        }
        // A `.claude/worktrees/` worktree, or any ordinary subdirectory — the
        // floor `VIEW_CWD`'s own doc comment describes must survive matching
        // by `$PWD`, not just by "is this exactly the checkout root".
        let worktree = project.join(".claude/worktrees/wt");
        tokio::fs::create_dir_all(&worktree).await.unwrap();

        // `other` is the run's default — the case a developer standing in
        // `project` must NOT fall back to.
        let case_block = build_agents_view(&[project.clone(), other.clone()], Some(&other));
        let resolver = home.path().join("resolve.sh");
        tokio::fs::write(
            &resolver,
            format!("#!/bin/sh\n{case_block}\nprintf '%s' \"$project\"\n"),
        )
        .await
        .unwrap();
        make_executable(&resolver).await.unwrap();

        let runner = RealRunner;
        let resolved_from = |dir: std::path::PathBuf| {
            let resolver = resolver.to_string_lossy().into_owned();
            let runner = &runner;
            async move {
                runner
                    .run(
                        &resolver,
                        &[],
                        &RunOptions {
                            cwd: Some(dir),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("resolve.sh ran")
                    .stdout
            }
        };

        assert_eq!(
            resolved_from(project.clone()).await,
            project.to_string_lossy(),
            "standing in a known checkout must resolve to itself, not to the run's default"
        );
        assert_eq!(
            resolved_from(worktree).await,
            project.to_string_lossy(),
            "a worktree beneath a known checkout must still resolve to it"
        );
        assert_eq!(
            resolved_from(elsewhere).await,
            other.to_string_lossy(),
            "standing in neither known checkout must fall back to the run's default"
        );
    }

    #[test]
    fn the_view_cwd_never_reaches_a_launch_that_carries_arguments() {
        // `--cwd` belongs to the `agents` subcommand and to nothing else:
        // `claude --cwd <path> mcp list` is "error: unknown option '--cwd'".
        // So a copy of it on the other branch would not scope anything — it
        // would break every `claude -p`, `claude --resume` and `claude auth
        // login` on every laptop at once, in Claude Code's own parser.
        let script = script();
        for line in script.lines().map(str::trim) {
            if !line.starts_with("set -- ") && !line.starts_with("exec ") {
                continue;
            }
            let Some(cwd) = line.find(VIEW_CWD) else {
                continue;
            };
            let agents = line
                .find(" agents")
                .unwrap_or_else(|| panic!("{VIEW_CWD} on a line with no agents view:\n{line}"));
            assert!(
                agents < cwd,
                "{VIEW_CWD} is ahead of the positional:\n{line}"
            );
        }
    }

    #[test]
    fn a_machine_with_no_checkout_yet_opens_the_view_as_it_always_did() {
        // Every machine before its first clone. There is no path to name, and
        // naming one anyway — the default the picker would offer, say — would
        // point the view at a directory nobody has cloned into yet.
        let script = script_for(&[], None);
        assert!(
            script.contains(&format!("set -- {ALLOW_BYPASS} agents\n")),
            "{script}"
        );
        // On the lines that run, not in the whole file: the comment above the
        // branch names the flag, and a whole-script search would fail on a
        // launcher that never passes it.
        for line in script.lines().map(str::trim) {
            if line.starts_with("set -- ") || line.starts_with("exec ") {
                assert!(!line.contains(VIEW_CWD), "{line}");
            }
        }
        // No `case "$PWD"` and no `-d` test either — there is nothing to
        // match against, so the branch is the single line it has always been.
        assert!(!script.contains(r#"case "$PWD""#), "{script}");
        assert!(!script.contains("[ -d "), "{script}");
    }

    #[test]
    fn a_checkout_that_is_gone_opens_the_view_as_it_always_did() {
        // A developer who deleted or renamed their checkout by hand, in the gap
        // between two riabuild runs. Claude Code does not refuse a `--cwd` that
        // is not there — it opens the view on an empty list naming a directory
        // nobody has, which is a worse `claude` than the one this launcher
        // wrote before the flag existed. The guard is shared across every
        // resolved checkout now, rather than written once per project, because
        // which path fills `$project` is no longer known until the script runs.
        let script = script();
        assert!(
            script.contains(r#"if [ -n "$project" ] && [ -d "$project" ]; then"#),
            "{script}"
        );
        assert!(
            script.contains(&format!("  else\n    set -- {ALLOW_BYPASS} agents\n  fi")),
            "{script}"
        );
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
        let project = home.path().join("checkout");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let script = launcher_script(
            &home.path().join("profile"),
            &claude.to_string_lossy(),
            &settings,
            &home.path().join("bin"),
            std::slice::from_ref(&project),
            Some(&project),
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

        // Neither launch may carry `--cwd`: it is an option of the `agents`
        // subcommand, and on either of these lines Claude Code's own parser
        // stops with "unknown option". Asserted on the arguments the launcher
        // really produced rather than on its text, because this is the failure
        // that takes `claude -p` away from every laptop at once.
        for args in [&bare, &carried] {
            assert!(!args.iter().any(|arg| arg == VIEW_CWD), "{args:?}");
        }
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

        // Half one: exactly one line carries a developer's own arguments, and
        // it carries the flag ahead of `"$@"` — so those arguments still reach
        // Claude Code as arguments, rather than landing after a flag that has
        // already consumed the line.
        //
        // Counted rather than asserted at three, which is what the agents view
        // now spells itself in — one line with `--cwd` and one without, chosen
        // by whether the checkout is on disk. Both of those are the *same*
        // branch as far as this test is concerned: neither may carry the flag,
        // and a fourth spelling of the view would not make this assertion
        // wrong.
        let built: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("set -- "))
            .collect();
        assert_eq!(
            built.iter().filter(|line| line.contains(r#""$@""#)).count(),
            1,
            "{script}"
        );
        assert_eq!(
            built
                .iter()
                .filter(|line| line.contains(STATIC_SYSTEM_PROMPT))
                .count(),
            1,
            "{script}"
        );
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
        //
        // Three lines rather than two, because the view spells itself twice —
        // with `--cwd` and without, chosen by whether the checkout is on disk.
        // That is exactly why this loops over every line it finds instead of
        // checking the two it expects: a branch added later is covered the day
        // it is written, and the machine with no checkout is the one that would
        // otherwise have been left out.
        let script = script();
        let built: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("set -- "))
            .collect();
        assert_eq!(built.len(), 3, "{script}");
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
            &[PathBuf::from(PROJECT)],
            Some(Path::new(PROJECT)),
        );
        assert!(script.contains(r#"case "$claude_binary" in"#), "{script}");
        assert!(script.contains(r#"*) claude_binary="" ;;"#), "{script}");
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

    /// Pins `--cwd` where the launcher puts it, against a real Claude Code.
    ///
    /// Two claims, and the launcher is wrong if either stops holding.
    ///
    /// It is an option of the **`agents` subcommand**, so it is accepted after
    /// the positional and rejected before it. The rejection is the half worth
    /// testing: `--cwd` ahead of the line does not scope a session, it stops
    /// Claude Code's parser, and a refactor that "tidied" the flag in beside
    /// `--settings` would take `claude -p` off every laptop at once.
    ///
    /// And an option in that position does not cost the view. Ahead of the
    /// positional an unrecognised option does — see
    /// `the_agents_view_and_the_static_prompt_flag_never_share_a_line` — so
    /// "after `agents` is the safe place for options" is a fact about Claude
    /// Code rather than something the shape of the launcher implies.
    ///
    /// `#[ignore]`d because it needs a real install: run
    /// `cargo test -- --ignored` when the pinned Claude Code version moves.
    #[tokio::test]
    #[ignore = "needs a real Claude Code install; pins where --cwd is accepted"]
    async fn the_view_cwd_is_an_agents_option_and_only_an_agents_option() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let checkout = home.path().join("checkout");
        tokio::fs::create_dir_all(&checkout).await.unwrap();
        let checkout = checkout.to_string_lossy().into_owned();

        // Where the launcher puts it. `--json` stands in for the interactive
        // view, which cannot run under `cargo test`: it takes the same parse
        // and the same `cwd` filter, and prints instead of mounting.
        let accepted = runner
            .run(
                "claude",
                &["agents", VIEW_CWD, &checkout, "--json"],
                &RunOptions::default(),
            )
            .await
            .expect("claude agents --cwd <path> --json");
        assert!(
            accepted.ok(),
            "claude rejected {VIEW_CWD} after the positional: {accepted:?}"
        );
        let sessions: serde_json::Value = serde_json::from_str(accepted.trimmed())
            .expect("claude agents --cwd <path> --json must print JSON");
        assert!(sessions.is_array(), "{sessions:?}");

        // And the spelling the launcher must never write.
        let rejected = runner
            .run(
                "claude",
                &[VIEW_CWD, &checkout, "mcp", "list"],
                &RunOptions::default(),
            )
            .await
            .expect("claude --cwd <path> mcp list");
        assert!(
            !rejected.ok(),
            "claude now accepts {VIEW_CWD} as a root option — the launcher's \
             reason for keeping it off the other branch has gone: {rejected:?}"
        );
    }
}

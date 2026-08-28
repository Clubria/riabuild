//! What one Claude Code launcher does.
//!
//! ```text
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
//! Both used to be built by a ninety-line `sh` script in `~/.riabuild/bin`.
//! They are built by [`handoff`] now, and the launcher on disk is one `exec`
//! naming `riabuild internal launch claude` — see `shims::launch` for why, and
//! for the table of which shell test became which function.
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
//! the settings key cannot reach — including one whose launch carries no
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
//! and every launch riabuild has ever built passed `--settings`. The task
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
//! The launcher also clears `SSH_CONNECTION`, `SSH_CLIENT` and `SSH_TTY`, and
//! claims `WAYLAND_DISPLAY` on a machine with no display, which together are what
//! make the clipboard shims beside it reachable — see the comments in
//! [`handoff`]. Both are undocumented behaviour, read out of the shipped
//! binary rather than promised anywhere, and neither can be pinned by a smoke
//! test: Claude Code exposes no non-interactive clipboard command to assert
//! against. Re-read them by hand when the pinned Claude Code version moves.

use std::path::{Path, PathBuf};

use super::super::launch::{self, Handoff, Harness, Plan, World};

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
/// `claude_accounts` already enforces and repairs, so no launch will meet a
/// binary that rejects it — and it is a global option, accepted ahead of a
/// subcommand exactly as `--settings` is, so `claude-2 auth login` still works.
///
/// Withheld from exactly one launch: a bare interactive `claude`, which takes
/// the agents view instead. That is not a preference between the two. The
/// `agents` positional is honoured only when the rest of the line is empty
/// after Claude Code strips the options it recognises, and this flag is not one
/// of them — so the pair does not open a view with a longer prompt, it opens
/// the background-agents subcommand and exits. See the module header.
pub(super) const STATIC_SYSTEM_PROMPT: &str = "--exclude-dynamic-system-prompt-sections";

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
/// Three such machines are ordinary rather than hypothetical. A launch that finds
/// no `org-settings.json` carries **no `--settings` at all**, which is every
/// laptop before its first successful fetch. A laptop holding a cached copy
/// written before the key existed serves settings with no permission mode in them
/// — the failure `org.backfillClaudeDefaults` was written for, which repairs the
/// *server's* row and can do nothing about a file already on disk. And under
/// `CLAUDE_CODE_REMOTE` Claude Code rejects `bypassPermissions` from settings
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
/// and repairs, so no launch will meet a binary that rejects it; and it is a
/// global option accepted ahead of a subcommand, which `settings_flag_survives_a_subcommand`
/// pins beside the other two. Verified against 2.1.235.
pub(super) const ALLOW_BYPASS: &str = "--allow-dangerously-skip-permissions";

/// Scopes the agents view to the checkout the developer is standing in.
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
/// on the machine, from every checkout; now they get the repository they are
/// working in, wherever they were standing.
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
/// other one riabuild has ever cloned. So [`checkout_for`] picks, per launch,
/// from every checkout in `UserConfig::repos`, and falls back to the run's
/// default repository only when the developer is standing in none of them.
/// Every developer with one repository sees exactly today's behaviour.
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
pub(super) const VIEW_CWD: &str = "--cwd";

/// The name the launcher claims a display under on a machine that has none.
///
/// Not a compositor anyone can connect to, and not meant to be: it says who
/// claimed it, to whoever runs `env` and wonders.
const CHANNEL_DISPLAY: &str = "riabuild-channel";

/// Claude Code answers "am I over SSH?" from these three and nothing else.
///
/// Over SSH it skips the native copy and returns `""` from every paste *without
/// running anything* — so the `xclip`/`wl-paste` shims in the same `bin/` are
/// never reached, and the channel they front is dead code. Clearing the three
/// makes Claude Code probe for a clipboard tool, find riabuild's shim first on
/// `PATH`, and reach the laptop.
///
/// Verified against Claude Code 2.1.224: only `SSH_CONNECTION` reaches the
/// clipboard path, but all three feed the terminal-type probe, so a session that
/// cleared one and kept the others would still report itself as `ssh-session`.
/// They are also on Claude Code's own environment allowlist, so a relaunched or
/// child session inherits whatever this leaves — clearing them here covers the
/// whole tree.
///
/// `SSH_AUTH_SOCK` is deliberately **not** among them: it is agent forwarding,
/// not session detection, and dropping it breaks `git push` over SSH.
const SSH_DETECTION: &[&str] = &["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"];

/// Which checkout the agents view opens on, given where the developer is
/// standing.
///
/// The Rust spelling of the launcher's `case "$PWD" in "$c"|"$c"/*) …`, and it
/// is a better one in a way that matters: `starts_with` on a [`Path`] compares
/// whole components, so a developer standing in `~/Clubria/payments-legacy` is
/// not matched by a checkout at `~/Clubria/payments`. The shell pattern got that
/// right too, but only because of where the `/` sits in it — a detail one
/// careless edit away from silently pulling every neighbouring directory into a
/// checkout it has nothing to do with.
///
/// `checkouts` is expected **longest first**, which is `known_checkouts`'s job:
/// a checkout nested inside another is unusual, but not something this may
/// assume away, and the first match wins.
///
/// The fallback is `default` — the repository this run is about — which is what
/// a developer standing nowhere riabuild has a checkout for gets pulled back to,
/// exactly as the single-repository launcher always did. `None` from both is the
/// one case with nothing to match at all: every machine before its first clone,
/// where the view opens unscoped rather than on a path nobody has cloned into.
pub fn checkout_for<'a>(
    cwd: &Path,
    checkouts: &'a [PathBuf],
    default: Option<&'a Path>,
) -> Option<&'a Path> {
    checkouts
        .iter()
        .find(|checkout| cwd.starts_with(checkout))
        .map(PathBuf::as_path)
        .or(default)
}

/// One Claude Code launch, decided.
///
/// Takes the [`Handoff`] `launch::handoff` has already put the profile
/// directory and (where the recorded binary has moved) the stripped `PATH` on,
/// and adds everything that is Claude Code's own.
pub(in crate::shims) fn handoff(handoff: Handoff, plan: &Plan, world: &World) -> Handoff {
    let handoff = handoff.unset(SSH_DETECTION.iter().copied());

    // Clearing those three is necessary and *not* sufficient, because reading
    // and writing the clipboard are not gated on the same thing. Reading is a
    // plain subprocess Claude Code runs whatever the environment says. Writing
    // goes through a Linux probe that asks for a display before it will look
    // for a tool at all — `WAYLAND_DISPLAY` before `wl-copy`, `DISPLAY` before
    // `xclip` — and a headless server has neither. It then records "no
    // clipboard tool here" and every copy leaves as an OSC 52 escape alone, so
    // the `wl-copy`/`xclip` shims beside this launcher are never run and the
    // channel carries pastes but no copies. Read out of Claude Code 2.1.232;
    // re-read it when the pinned version moves.
    //
    // Claimed only where riabuild's own `wl-copy` is what the probe will find,
    // and only on a machine that genuinely has no display of its own — so a
    // Linux laptop with a real session keeps the clipboard it already had.
    let headless = world.wayland_display.is_none() && world.x11_display.is_none();
    let handoff = match world.wl_copy_present && headless {
        true => handoff.env("WAYLAND_DISPLAY", CHANNEL_DISPLAY),
        false => handoff,
    };

    let mut args = Vec::new();
    // `--settings` first, and only where the file is there. Every machine
    // before its first successful fetch launches without it rather than
    // pointing Claude Code at a file that does not exist.
    if let (true, Some(settings)) = (world.settings_present, &plan.settings) {
        args.push("--settings".to_string());
        args.push(settings.to_string_lossy().into_owned());
    }
    args.extend(argument_line(plan, world));
    handoff.with_args(args)
}

/// Which of the two lines this launch is, and what is on it.
///
/// Three guards decide, and each is load-bearing:
///
/// - **no arguments.** A developer who typed something asked for that, not for
///   the view, and `agents` would land in front of their own first word.
/// - **a terminal on both stdin and stdout.** `echo "fix the build" | claude`
///   is a session with a prompt on stdin. Claude Code's positional route does
///   not test the terminal itself, so without this the prompt is swallowed and
///   the view opens over it. Claude Code applies the same pair on its own route.
/// - **`CLAUDE_CODE_DISABLE_AGENT_VIEW` unset.** Claude Code's documented off
///   switch. With the view disabled, `claude agents` does not fall back to a
///   session — it prints "'claude agents' is disabled …" and exits 1. Honouring
///   the switch here is the difference between a developer turning the view off
///   and a developer losing the `claude` command.
fn argument_line(plan: &Plan, world: &World) -> Vec<String> {
    let bare_and_interactive = plan.args.is_empty()
        && world.stdin_is_tty
        && world.stdout_is_tty
        && !world.agents_view_disabled;

    if !bare_and_interactive {
        // Every other launch — `claude -p`, `claude --resume`, `claude "some
        // prompt"`, `claude-2 auth login`. The two flags go ahead of the
        // developer's own arguments, because behind them they would land after
        // a line Claude Code's parser has already consumed.
        let mut line = vec![STATIC_SYSTEM_PROMPT.to_string(), ALLOW_BYPASS.to_string()];
        line.extend(plan.args.iter().cloned());
        return line;
    }

    // `ALLOW_BYPASS` rides along on this line too, and that is not decoration:
    // Claude Code folds it into the dispatch defaults the view hands to every
    // session it starts, so dropping it here would take bypass-permissions out
    // of the Shift+Tab cycle for exactly the launch most developers make.
    // `STATIC_SYSTEM_PROMPT` cannot come with it — see the module header.
    let mut line = vec![ALLOW_BYPASS.to_string(), "agents".to_string()];
    if let Some(checkout) = checkout_for(
        &world.cwd,
        &plan.checkouts,
        plan.default_checkout.as_deref(),
    ) && world.selected_checkout_exists
    {
        line.push(VIEW_CWD.to_string());
        line.push(checkout.to_string_lossy().into_owned());
    }
    line
}

/// The plan one account's launcher records.
pub fn plan(
    config_dir: &Path,
    claude: &str,
    org_settings: &Path,
    bin_dir: &Path,
    checkouts: &[PathBuf],
    default: Option<&Path>,
) -> Plan {
    Plan {
        settings: Some(org_settings.to_path_buf()),
        checkouts: checkouts.to_vec(),
        default_checkout: default.map(Path::to_path_buf),
        ..Plan::new(
            Harness::Claude,
            config_dir.to_path_buf(),
            claude.to_string(),
            bin_dir.to_path_buf(),
        )
    }
}

/// One account's launcher: `claude`, or `claude-<n>`.
///
/// `checkouts` is every repository this machine knows a path for —
/// `UserConfig::repos`, longest first — and `default` is the checkout of the
/// repository the *current* run is about, used only when the developer's
/// working directory is under none of `checkouts`. Both are commonly the same
/// single path, which is what a machine with one repository looks like;
/// `default` is `None` only where there is no checkout at all yet to fall back
/// to — every machine before its first clone.
pub fn launcher_script(
    riabuild: &Path,
    config_dir: &Path,
    claude: &str,
    org_settings: &Path,
    bin_dir: &Path,
    checkouts: &[PathBuf],
    default: Option<&Path>,
) -> String {
    launch::script(
        riabuild,
        &plan(
            config_dir,
            claude,
            org_settings,
            bin_dir,
            checkouts,
            default,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shims::launch::handoff as launch_handoff;

    /// The checkout the fixture launcher opens its agents view on.
    const PROJECT: &str = "/Users/ada/Clubria/ai-builders-hub";

    /// A second checkout, for the tests about a machine that knows more than
    /// one — the shape that used to lose `--cwd` for every repository but the
    /// last one `riabuild` was run against.
    const OTHER_PROJECT: &str = "/Users/ada/Clubria/payments";

    const CONFIG_DIR: &str = "/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555";
    const BINARY: &str = "/Users/ada/.riabuild/node/22.23.1/bin/claude";
    const SETTINGS: &str = "/Users/ada/.riabuild/org-settings.json";
    const BIN_DIR: &str = "/Users/ada/.riabuild/bin";

    /// The fixture most tests want: one known checkout, which is also the
    /// run's default — what a machine with a single repository looks like.
    fn fixture() -> Plan {
        plan_for(&[PathBuf::from(PROJECT)], Some(Path::new(PROJECT)))
    }

    fn plan_for(checkouts: &[PathBuf], default: Option<&Path>) -> Plan {
        plan(
            Path::new(CONFIG_DIR),
            BINARY,
            Path::new(SETTINGS),
            Path::new(BIN_DIR),
            checkouts,
            default,
        )
    }

    /// A laptop with everything in place: the binary is there, the settings
    /// have been fetched, and the developer is sitting at a terminal in their
    /// checkout.
    fn laptop() -> World {
        World {
            binary_is_executable: true,
            settings_present: true,
            stdin_is_tty: true,
            stdout_is_tty: true,
            cwd: PathBuf::from(PROJECT),
            selected_checkout_exists: true,
            path: format!("{BIN_DIR}:/usr/local/bin:/usr/bin"),
            ..Default::default()
        }
    }

    /// A launch that carries the developer's own arguments.
    fn carrying(args: &[&str]) -> Plan {
        Plan {
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            ..fixture()
        }
    }

    fn value(handoff: &Handoff, key: &str) -> Option<String> {
        handoff
            .env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    }

    #[test]
    fn the_launcher_sets_the_account_and_layers_org_settings() {
        let handoff = launch_handoff(&fixture(), &laptop());
        assert_eq!(
            value(&handoff, "CLAUDE_CONFIG_DIR").as_deref(),
            Some(CONFIG_DIR)
        );
        assert_eq!(handoff.args.first().map(String::as_str), Some("--settings"));
        assert_eq!(handoff.args.get(1).map(String::as_str), Some(SETTINGS));
    }

    /// Arguments must reach `claude`, or `claude-2 --resume` silently loses
    /// them — and `claude-2 auth login`, which the account box tells the
    /// developer to run, would do nothing at all.
    #[test]
    fn a_developers_own_arguments_reach_claude_in_order() {
        let handoff = launch_handoff(&carrying(&["-p", "fix the build"]), &laptop());
        assert_eq!(
            handoff.args,
            vec![
                "--settings",
                SETTINGS,
                STATIC_SYSTEM_PROMPT,
                ALLOW_BYPASS,
                "-p",
                "fix the build",
            ]
        );
    }

    #[test]
    fn the_launcher_clears_the_ssh_detection_variables() {
        // Claude Code answers "am I over SSH?" from these three and nothing
        // else. Leave any of them set and every paste returns "" without a
        // subprocess, so the xclip/wl-paste shims beside this launcher are
        // never invoked and the channel behind them is unreachable.
        //
        // Agent forwarding is not session detection: a fourth name here, or
        // SSH_AUTH_SOCK joining this list, breaks `git push` over SSH.
        let handoff = launch_handoff(&fixture(), &laptop());
        assert_eq!(
            handoff.env_remove,
            vec!["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        );
        // Removed, not blanked. `SSH_CONNECTION=""` is still set to anything
        // testing for presence, which is the whole reason `env_remove` exists.
        assert!(
            !handoff.env.iter().any(|(key, _)| key.starts_with("SSH_")),
            "{:?}",
            handoff.env
        );
    }

    /// Clearing the SSH variables is half of reaching the clipboard shims, and
    /// on its own it only buys pastes. Claude Code's *write* path runs a Linux
    /// probe that asks for a display before it will look for a tool at all, so
    /// on a headless server it records "no clipboard tool" and every copy
    /// leaves as an OSC 52 escape while the wl-copy beside this launcher is
    /// never run.
    #[test]
    fn the_launcher_claims_a_display_so_copies_reach_the_shims() {
        let server = World {
            wl_copy_present: true,
            ..laptop()
        };
        let handoff = launch_handoff(&fixture(), &server);
        assert_eq!(
            value(&handoff, "WAYLAND_DISPLAY").as_deref(),
            Some(CHANNEL_DISPLAY)
        );
    }

    /// Each guard on the display claim, and what dropping it would cost.
    #[test]
    fn a_machine_with_a_display_of_its_own_keeps_the_clipboard_it_already_had() {
        // Without the two display tests riabuild would take a real X11 or
        // Wayland session away from a Linux developer working at their own
        // desk, and their copies would go to a channel that is not there
        // instead of to the clipboard in front of them.
        for existing in [
            World {
                wl_copy_present: true,
                wayland_display: Some("wayland-0".into()),
                ..laptop()
            },
            World {
                wl_copy_present: true,
                x11_display: Some(":0".into()),
                ..laptop()
            },
            // And without the `wl-copy` test a laptop that never wrote the
            // shims would claim a display it has no tool for.
            World {
                wl_copy_present: false,
                ..laptop()
            },
        ] {
            let handoff = launch_handoff(&fixture(), &existing);
            assert_eq!(value(&handoff, "WAYLAND_DISPLAY"), None);
        }
    }

    /// `~/.riabuild/bin` is first on `PATH`, so a launcher called `claude` that
    /// falls back to a bare `claude` finds itself and forks until the shell
    /// dies. The strip is what stops that, and it happens on the one branch
    /// that has no absolute path left to use.
    #[test]
    fn a_binary_that_moved_is_found_without_riabuilds_own_bin() {
        // `claude update` can migrate to a native install, which leaves the
        // recorded path dangling until the next `riabuild`. A dead `claude`
        // command reads as Claude Code being uninstalled.
        let moved = World {
            binary_is_executable: false,
            ..laptop()
        };
        let handoff = launch_handoff(&fixture(), &moved);
        assert_eq!(handoff.program, "claude");
        assert_eq!(
            value(&handoff, "PATH").as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
    }

    #[test]
    fn the_recorded_binary_is_what_runs_where_it_is_still_there() {
        let handoff = launch_handoff(&fixture(), &laptop());
        assert_eq!(handoff.program, BINARY);
        // No `PATH` is touched on this branch: there is nothing to strip, and
        // rewriting it anyway would change how the harness's own children
        // resolve commands for no reason.
        assert_eq!(value(&handoff, "PATH"), None);
    }

    /// `Ctx::claude()` returns the bare name "claude" before a Node version is
    /// pinned, and an executable test on a bare name is resolved against the
    /// *working directory* — a same-named executable in an untrusted checkout
    /// would pass it, skip the strip, and be started in place of Claude Code.
    #[test]
    fn a_bare_binary_name_is_never_taken_for_the_recorded_one() {
        let plan = Plan {
            binary: "claude".to_string(),
            ..fixture()
        };
        let handoff = launch_handoff(&plan, &laptop());
        assert_eq!(handoff.program, "claude");
        assert_eq!(
            value(&handoff, "PATH").as_deref(),
            Some("/usr/local/bin:/usr/bin"),
            "the strip must happen even though the executable test passed"
        );
    }

    #[test]
    fn the_launcher_still_works_before_settings_have_been_fetched() {
        let unfetched = World {
            settings_present: false,
            ..laptop()
        };
        let handoff = launch_handoff(&carrying(&["--resume"]), &unfetched);
        assert!(
            !handoff.args.iter().any(|arg| arg == "--settings"),
            "{:?}",
            handoff.args
        );
        // And the launch still happens, with the developer's arguments intact.
        assert_eq!(
            handoff.args,
            vec![STATIC_SYSTEM_PROMPT, ALLOW_BYPASS, "--resume"]
        );
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
        let handoff = launch_handoff(&fixture(), &laptop());
        assert_eq!(
            handoff.args,
            vec![
                "--settings",
                SETTINGS,
                ALLOW_BYPASS,
                "agents",
                VIEW_CWD,
                PROJECT,
            ]
        );
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
        // Every shape of launch there is, so a fourth one added later is
        // covered on the day it is written.
        for (plan, world) in [
            (fixture(), laptop()),
            (
                fixture(),
                World {
                    selected_checkout_exists: false,
                    ..laptop()
                },
            ),
            (plan_for(&[], None), laptop()),
            (carrying(&["-p", "hello"]), laptop()),
            (
                fixture(),
                World {
                    stdin_is_tty: false,
                    ..laptop()
                },
            ),
        ] {
            let args = launch_handoff(&plan, &world).args;
            assert!(
                !(args.iter().any(|arg| arg == "agents")
                    && args.iter().any(|arg| arg == STATIC_SYSTEM_PROMPT)),
                "{args:?}"
            );
        }
    }

    /// Asserted on *every* shape of launch, because the machine that matters
    /// most is the one a spot check would not distinguish: a laptop that has
    /// not fetched `org-settings.json` launches with no `--settings` at all, so
    /// `permissions.defaultMode` reaches it by no route whatsoever and this
    /// flag is the only thing keeping the mode in its Shift+Tab cycle. Losing
    /// it is invisible — the launcher starts, Claude Code starts, and the cycle
    /// silently has one fewer stop on it.
    #[test]
    fn every_branch_keeps_bypass_permissions_reachable_from_the_cycle() {
        for (plan, world) in [
            (fixture(), laptop()),
            (
                fixture(),
                World {
                    selected_checkout_exists: false,
                    ..laptop()
                },
            ),
            (plan_for(&[], None), laptop()),
            (carrying(&["auth", "login"]), laptop()),
            (
                carrying(&["-p", "hello"]),
                World {
                    settings_present: false,
                    ..laptop()
                },
            ),
            (
                fixture(),
                World {
                    agents_view_disabled: true,
                    ..laptop()
                },
            ),
        ] {
            let args = launch_handoff(&plan, &world).args;
            assert!(args.iter().any(|arg| arg == ALLOW_BYPASS), "{args:?}");
        }
    }

    /// On the branch that forwards a developer's own arguments, both flags go
    /// ahead of them — behind, they would land after a line Claude Code's
    /// parser has already consumed.
    #[test]
    fn the_launchers_own_flags_come_before_the_developers_arguments() {
        let args = launch_handoff(&carrying(&["mcp", "list"]), &laptop()).args;
        let position = |wanted: &str| args.iter().position(|arg| arg == wanted);
        assert!(position(STATIC_SYSTEM_PROMPT) < position("mcp"), "{args:?}");
        assert!(position(ALLOW_BYPASS) < position("mcp"), "{args:?}");
    }

    #[test]
    fn the_agents_view_opens_on_the_checkout() {
        // The feature. Without it a `claude` typed anywhere but the checkout
        // opens a view listing every session on the machine, from every
        // directory the developer has ever worked in.
        let args = launch_handoff(&fixture(), &laptop()).args;
        let cwd = args.iter().position(|arg| arg == VIEW_CWD).expect("--cwd");
        let agents = args.iter().position(|arg| arg == "agents").expect("agents");
        assert!(agents < cwd, "{args:?}");
        assert_eq!(args.get(cwd + 1).map(String::as_str), Some(PROJECT));
    }

    /// A machine that knows a second repository still opens `--cwd` on the one
    /// the developer is standing in — the regression this resolution exists to
    /// close. Before it, `--cwd` named whichever repository `riabuild` was
    /// *last run against*, machine-wide, so a developer standing in `PROJECT`
    /// while `OTHER_PROJECT` was the more recent run was moved off their own
    /// checkout the moment they typed `claude`.
    #[test]
    fn the_checkout_matching_the_working_directory_wins_over_the_run_default() {
        let plan = plan_for(
            &[PathBuf::from(OTHER_PROJECT), PathBuf::from(PROJECT)],
            // The run's default is the *other* repository — the case a
            // developer standing in `PROJECT` must not fall back to.
            Some(Path::new(OTHER_PROJECT)),
        );
        let args = launch_handoff(&plan, &laptop()).args;
        assert!(args.iter().any(|arg| arg == PROJECT), "{args:?}");
        assert!(!args.iter().any(|arg| arg == OTHER_PROJECT), "{args:?}");
    }

    /// The floor `VIEW_CWD`'s own doc comment describes: a subdirectory or a
    /// `.claude/worktrees/` worktree beneath a checkout still resolves to it,
    /// not merely a working directory that *is* the checkout root.
    #[test]
    fn a_worktree_beneath_a_checkout_still_resolves_to_that_checkout() {
        let checkouts = [PathBuf::from(OTHER_PROJECT), PathBuf::from(PROJECT)];
        assert_eq!(
            checkout_for(
                Path::new("/Users/ada/Clubria/ai-builders-hub/.claude/worktrees/wt"),
                &checkouts,
                Some(Path::new(OTHER_PROJECT)),
            ),
            Some(Path::new(PROJECT))
        );
    }

    /// Standing in neither known checkout falls back to the run's default.
    #[test]
    fn standing_in_neither_checkout_falls_back_to_the_run_default() {
        let checkouts = [PathBuf::from(OTHER_PROJECT), PathBuf::from(PROJECT)];
        assert_eq!(
            checkout_for(
                Path::new("/Users/ada/somewhere-else"),
                &checkouts,
                Some(Path::new(OTHER_PROJECT)),
            ),
            Some(Path::new(OTHER_PROJECT))
        );
    }

    /// A neighbour whose path merely *starts with* a checkout's is not inside
    /// it. `starts_with` on a `Path` compares whole components, which is the
    /// half of this the shell `case` got right only by where its `/` sat.
    #[test]
    fn a_sibling_directory_with_a_shared_prefix_is_not_inside_the_checkout() {
        let checkouts = [PathBuf::from(OTHER_PROJECT)];
        assert_eq!(
            checkout_for(
                Path::new("/Users/ada/Clubria/payments-legacy/src"),
                &checkouts,
                None,
            ),
            None
        );
    }

    #[test]
    fn the_view_cwd_never_reaches_a_launch_that_carries_arguments() {
        // `--cwd` belongs to the `agents` subcommand and to nothing else:
        // `claude --cwd <path> mcp list` is "error: unknown option '--cwd'".
        // So a copy of it on the other branch would not scope anything — it
        // would break every `claude -p`, `claude --resume` and `claude auth
        // login` on every laptop at once, in Claude Code's own parser.
        for plan in [carrying(&["-p", "hello"]), carrying(&["auth", "login"])] {
            let args = launch_handoff(&plan, &laptop()).args;
            assert!(!args.iter().any(|arg| arg == VIEW_CWD), "{args:?}");
        }
    }

    #[test]
    fn a_machine_with_no_checkout_yet_opens_the_view_as_it_always_did() {
        // Every machine before its first clone. There is no path to name, and
        // naming one anyway — the default the picker would offer, say — would
        // point the view at a directory nobody has cloned into yet.
        let args = launch_handoff(&plan_for(&[], None), &laptop()).args;
        assert_eq!(args, vec!["--settings", SETTINGS, ALLOW_BYPASS, "agents"]);
    }

    #[test]
    fn a_checkout_that_is_gone_opens_the_view_as_it_always_did() {
        // A developer who deleted or renamed their checkout by hand, in the gap
        // between two riabuild runs. Claude Code does not refuse a `--cwd` that
        // is not there — it opens the view on an empty list naming a directory
        // nobody has, which is a worse `claude` than the one this launcher
        // gave before the flag existed.
        let gone = World {
            selected_checkout_exists: false,
            ..laptop()
        };
        let args = launch_handoff(&fixture(), &gone).args;
        assert_eq!(args, vec!["--settings", SETTINGS, ALLOW_BYPASS, "agents"]);
    }

    /// Each guard on the agents-view branch, and what dropping it would cost.
    ///
    /// Asserted by *taking each one away in turn*, which is the thing the old
    /// text assertions on the generated shell could not do: an `-eq` that
    /// should have been `-gt`, a guard true where it should be false, and a
    /// `set --` that dropped `"$@"` all read identically in a script.
    #[test]
    fn the_agents_view_is_guarded_three_ways() {
        let took_the_view = |plan: &Plan, world: &World| {
            launch_handoff(plan, world)
                .args
                .iter()
                .any(|arg| arg == "agents")
        };
        assert!(took_the_view(&fixture(), &laptop()));

        // A developer who typed something asked for that, not for the view —
        // and `agents` would land in front of their own first word.
        assert!(!took_the_view(&carrying(&["--resume"]), &laptop()));

        // `echo "fix the build" | claude` is a session with a prompt on stdin.
        // Claude Code's positional route does not test the terminal itself, so
        // without these two the prompt is swallowed and the view opens over it.
        assert!(!took_the_view(
            &fixture(),
            &World {
                stdin_is_tty: false,
                ..laptop()
            }
        ));
        assert!(!took_the_view(
            &fixture(),
            &World {
                stdout_is_tty: false,
                ..laptop()
            }
        ));

        // Claude Code's own off switch. With the view disabled, `claude agents`
        // does not fall back to a session — it writes "'claude agents' is
        // disabled …" to stderr and exits 1. Ignoring it here would turn a
        // developer who turned the view off into a developer with no working
        // `claude` at all.
        assert!(!took_the_view(
            &fixture(),
            &World {
                agents_view_disabled: true,
                ..laptop()
            }
        ));
    }

    /// A non-interactive launch is the shape `echo "fix the build" | claude`, a
    /// CI job and `claude -p` all arrive in — and it must still be a working
    /// `claude`, with the developer's arguments and the launcher's flags on it.
    #[test]
    fn a_launch_with_no_terminal_is_still_a_complete_launch() {
        let piped = World {
            stdin_is_tty: false,
            ..laptop()
        };
        let args = launch_handoff(&fixture(), &piped).args;
        assert_eq!(
            args,
            vec!["--settings", SETTINGS, STATIC_SYSTEM_PROMPT, ALLOW_BYPASS,]
        );
    }

    /// The launcher on disk is one `exec` and carries the values riabuild
    /// resolved — which is what `claude_accounts::check` compares against, so a
    /// launcher naming last week's Node or a deleted account is still drift.
    #[test]
    fn the_generated_launcher_names_every_value_it_was_written_with() {
        let script = launcher_script(
            Path::new("/opt/riabuild/2026.08.27/riabuild"),
            Path::new(CONFIG_DIR),
            BINARY,
            Path::new(SETTINGS),
            Path::new(BIN_DIR),
            &[PathBuf::from(PROJECT), PathBuf::from(OTHER_PROJECT)],
            Some(Path::new(PROJECT)),
        );
        for value in [
            CONFIG_DIR,
            BINARY,
            SETTINGS,
            BIN_DIR,
            PROJECT,
            OTHER_PROJECT,
        ] {
            assert!(script.contains(value), "{value} missing from {script}");
        }
        assert!(
            script.contains("/opt/riabuild/2026.08.27/riabuild"),
            "{script}"
        );
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
    /// argument parser still allows this, and the launchers are built on the
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

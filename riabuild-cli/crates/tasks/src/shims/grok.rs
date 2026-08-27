//! `~/.riabuild/bin/grok` — the Grok Build launcher.
//!
//! The same shape as the Codex launcher next door, for the same reasons: a
//! small generated script in `bin/` that pins the config directory riabuild
//! owns, adds the flag the team wants by default, and falls back to `PATH` when
//! the binary it recorded has moved.
//!
//! ```sh
//! GROK_HOME=~/.riabuild/grok/1 grok --permission-mode bypassPermissions
//! ```
//!
//! Nine of them, `grok-1` … `grok-9`, plus `grok` for the first — because Grok
//! Build keeps several sign-ins apart the way Claude Code and Codex do. Its
//! credentials live in `$GROK_HOME/auth.json`, keyed by auth scope, and in no
//! OS keychain, so pointing it at nine directories really is nine independent
//! accounts. `GROK_HOME` carries the rest of that account's state too —
//! `config.toml`, sessions, MCP registrations, hooks and plugins.
//!
//! The nine exist from the first run rather than being created on demand, for
//! the reason `shims::codex` gives: riabuild signs nobody in to Grok Build, so
//! there is no moment at which it would learn a developer wants a second one.
//! A Grok sign-in is the developer's own xAI account, nothing riabuild brokers.
//!
//! What is **not** copied from the Claude launcher: `unset SSH_CONNECTION
//! SSH_CLIENT SSH_TTY`, the `WAYLAND_DISPLAY` claim, and the `--settings`
//! layer. All three are workarounds for behaviour read out of the Claude Code
//! binary; none is a fact about Grok Build, and asserting them here would be
//! inventing an upstream behaviour rather than accommodating one.
//!
//! Nor is Codex's Node handling. Codex is a Node script whose `bin/codex` is a
//! symlink to a `codex.js` with a `#!/usr/bin/env node` shebang, so its
//! launcher has to put riabuild's own Node on `PATH` first. Grok Build is a
//! static-pie ELF / Mach-O executable that needs nothing on `PATH` at all —
//! verified against 1.0.5 — so a launcher that carried a Node would be carrying
//! it for no one.

use anyhow::Result;
use std::path::Path;

/// How many Grok Build profiles riabuild makes.
///
/// Nine, for the reason `accounts::MAX` and `shims::codex::PROFILES` are nine:
/// it keeps every launcher name single digit, so `grok-9` is the last one and
/// `grok-12` is an obvious typo rather than something to interpret.
/// Deliberately its own constant rather than a reference to either of those —
/// the three happen to agree, and a Grok profile, a Codex profile and a Claude
/// account are not the same thing. Coupling them would make changing one
/// silently change the others.
pub const PROFILES: usize = 9;

/// The permission mode that approves every tool call without asking.
///
/// One of six `--permission-mode` values (`default`, `acceptEdits`, `auto`,
/// `dontAsk`, `bypassPermissions`, `plan`), and the only one that is a full
/// bypass — `dontAsk` silently *denies* anything not pre-approved, which is the
/// opposite thing under a name that reads like this one.
///
/// Passing it on the command line rather than writing `permission_mode =
/// "always-approve"` into each profile's `config.toml` is deliberate, and it is
/// not merely the tidier of two equal options. Grok Build resolves the launch
/// mode as *CLI beats `[ui]` config beats remote*, so the flag is the only
/// spelling that cannot be silently overridden by a value already on disk —
/// and `config.toml` is a file the developer owns and edits, which riabuild
/// would then be rewriting under them on every run. The launcher is riabuild's
/// file and says so at the top.
///
/// Read out of Grok Build 1.0.5's `resolve_effective_yolo`, and pinned by the
/// `#[ignore]`d smoke tests at the end of this file.
const BYPASS: &str = "bypassPermissions";

/// The flag riabuild adds, and the one it has to look for in the developer's
/// own arguments before adding it.
///
/// Grok Build rejects `--permission-mode` twice — "the argument
/// '--permission-mode <MODE>' cannot be used multiple times", in both the
/// spaced and `=` spellings — so a launcher that always appended it would turn
/// a developer typing `grok --permission-mode plan` into a parser error naming
/// a flag they did not type. Exactly the trap `--yolo` sets for the Codex
/// launcher.
const PERMISSION_MODE: &str = "--permission-mode";

/// The launcher, which is what decides which profile a `grok` gets.
///
/// The environment shell exports a `GROK_HOME` of its own — profile 1, the one
/// this same file writes the unnumbered `grok` for — so that a Grok Build
/// reached by any route other than a launcher still lands in riabuild's tree
/// rather than in `~/.grok`, a directory it would otherwise create on the spot.
/// This line is what keeps the nine apart regardless: it `export`s over
/// whatever was inherited, so `grok-3` is profile 3 inside the environment
/// shell and outside it alike. See `shell::environment`.
pub fn launcher_script(grok_home: &Path, grok: &str, bin_dir: &Path) -> String {
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Launches Grok Build against the config directory riabuild owns, with tool
# approvals bypassed by default.
set -e
GROK_HOME="{grok_home}"
export GROK_HOME
# Grok Build creates a GROK_HOME that is not there rather than refusing to start
# — unlike Codex, which hard-fails with "Error finding codex home". Created here
# anyway so that a profile riabuild reports as present is present: the gap
# between two riabuild runs is exactly where a `rm -rf` lands, and a directory
# conjured by the tool on first use is not the same as one riabuild put there.
[ -d "$GROK_HOME" ] || mkdir -p "$GROK_HOME"
grok_binary="{grok}"
case "$grok_binary" in
  /*) ;;
  # `Ctx::grok()` names a versioned directory and is always absolute, so this
  # arm is unreachable today. It stays because the `-x` test below is
  # cwd-relative for anything that is not: a same-named executable in whatever
  # directory the developer is standing in would pass it, skip the PATH strip,
  # and exec a bare name that PATH search resolves straight back to this script.
  *) grok_binary="" ;;
esac
if [ ! -x "$grok_binary" ]; then
  # The recorded binary is gone: a version bump since the last run, or a
  # half-removed install. Fall back to PATH with riabuild's own bin/ removed —
  # without that this script finds itself, because bin/ comes first inside the
  # environment shell.
  PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "{bin_dir}" | paste -sd: -)
  export PATH
  grok_binary=grok
fi
# --permission-mode {bypass} is a default, not an imposition, and it has
# to be: Grok Build rejects the flag twice ("cannot be used multiple times"), so
# appending it unconditionally would make `grok --permission-mode plan` — and
# `grok --permission-mode ask`, the thing a developer reaches for precisely when
# they want the prompts back — fail in the parser, naming a flag they never
# typed. So it is added only where the developer expressed no policy of their
# own.
#
# `--always-approve` / `--yolo` is deliberately not matched here: it is a
# separate boolean that Grok Build accepts happily alongside this flag, and both
# mean the same thing, so standing aside for it would buy nothing.
#
# The scan reads the developer's arguments as text, so a prompt that happens to
# contain the flag's name — `grok -p 'what does --permission-mode do?'` — makes
# it stand aside. That is the safe direction to be wrong in: the session asks
# for approvals rather than silently granting them.
for riabuild_arg do
  case "$riabuild_arg" in
    {flag} | {flag}=*)
      exec "$grok_binary" "$@"
      ;;
  esac
done
# Ahead of "$@", because Grok Build accepts this only as a root option: after a
# subcommand it is "unexpected argument '--permission-mode' found", so
# `grok mcp list` has to become `grok --permission-mode ... mcp list` rather
# than the other way round. Verified against 1.0.5.
exec "$grok_binary" {flag} {bypass} "$@"
"#,
        grok_home = grok_home.display(),
        bin_dir = bin_dir.display(),
        flag = PERMISSION_MODE,
        bypass = BYPASS,
    )
}

/// Writes `grok` and `grok-1` … `grok-9`.
///
/// `grok` and `grok-1` are the same script, which is what makes the bare name
/// mean profile 1 — the shape `shims::write_all` and `shims::codex::write`
/// already use.
///
/// Each goes through [`shims::write_script`](super::write_script), like every
/// other generated script, so it is landed by rename and then made executable:
/// the hazard is an interrupt mid-write leaving a truncated launcher that fails
/// with a shell syntax error.
pub async fn write(ctx: &crate::Ctx) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    let grok = ctx.grok();

    for profile in 1..=PROFILES {
        let script = launcher_script(&ctx.paths.grok_profile_dir(profile), &grok, &bin);
        super::write_script(&bin, &format!("grok-{profile}"), &script).await?;
        if profile == 1 {
            super::write_script(&bin, "grok", &script).await?;
        }
    }
    Ok(())
}

/// Every launcher this module owns, in the order `check()` reports them.
///
/// One list, so a `check()` that verifies the set and an `apply()` that writes
/// it cannot come to disagree about what the set is.
pub fn launcher_names() -> Vec<String> {
    let mut names = vec!["grok".to_string()];
    names.extend((1..=PROFILES).map(|profile| format!("grok-{profile}")));
    names
}

/// Which profile a launcher opens: `grok` and `grok-1` both open profile 1.
pub fn profile_of(name: &str) -> usize {
    name.strip_prefix("grok-")
        .and_then(|number| number.parse().ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ctx_with;
    use riabuild_fetch::archive::make_executable;
    use riabuild_runner::FakeRunner;

    fn script() -> String {
        launcher_script(
            Path::new("/home/ada/.riabuild/grok/1"),
            "/home/ada/.riabuild/grok/1.0.5/grok",
            Path::new("/home/ada/.riabuild/bin"),
        )
    }

    #[test]
    fn the_launcher_pins_the_config_directory_riabuild_owns() {
        let script = script();
        assert!(
            script.contains(r#"GROK_HOME="/home/ada/.riabuild/grok/1""#),
            "{script}"
        );
        // A dropped `export` would leave every profile sharing whatever
        // GROK_HOME the environment already had — all nine collapsing into one
        // — with the rest of this test still green.
        assert!(script.contains("export GROK_HOME"), "{script}");
    }

    #[test]
    fn the_launcher_creates_a_config_directory_that_is_not_there() {
        let script = script();
        assert!(
            script.contains(r#"[ -d "$GROK_HOME" ] || mkdir -p "$GROK_HOME""#),
            "{script}"
        );
    }

    #[test]
    fn the_launcher_bypasses_permissions_by_default() {
        // The whole feature. `bypassPermissions` and not `dontAsk`, which reads
        // like the same thing and silently denies instead.
        let script = script();
        assert!(
            script.contains(r#"exec "$grok_binary" --permission-mode bypassPermissions "$@""#),
            "{script}"
        );
        assert!(!script.contains("dontAsk"), "{script}");
    }

    #[test]
    fn the_bypass_flag_is_passed_ahead_of_the_developers_own_arguments() {
        // Grok Build accepts `--permission-mode` as a root option only: after a
        // subcommand it is "unexpected argument". A launcher that appended it
        // would break `grok mcp list` while still containing the right flag.
        let script = script();
        let exec = script
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("exec ") && line.contains(BYPASS))
            .expect("the launcher execs grok with the bypass");
        assert!(exec.find(PERMISSION_MODE) < exec.find(r#""$@""#), "{exec}");
    }

    #[test]
    fn the_launcher_stands_aside_for_a_permission_policy_the_developer_chose() {
        // Grok Build refuses `--permission-mode` twice, in both spellings, so
        // appending it unconditionally turns `grok --permission-mode plan` into
        // a parser error naming a flag the developer never typed.
        let script = script();
        for spelling in ["--permission-mode", "--permission-mode=*"] {
            assert!(
                script.contains(spelling),
                "{spelling} is not matched: {script}"
            );
        }
        assert!(script.contains(r#"exec "$grok_binary" "$@""#), "{script}");
    }

    #[test]
    fn the_launcher_can_never_exec_itself() {
        // `bin/` leads PATH inside the environment shell, so a bare `grok`
        // would resolve straight back to this script. The strip is what stops
        // the fallback becoming an exec loop.
        let script = script();
        assert!(
            script.contains(r#"grep -vxF "/home/ada/.riabuild/bin""#),
            "{script}"
        );
        assert!(!script.contains("exec grok"), "{script}");
        // `tr '\n' ':'` would leave a trailing colon, and an empty PATH entry
        // means the current directory.
        assert!(script.contains("paste -sd: -"), "{script}");
    }

    #[test]
    fn a_grok_that_is_not_an_absolute_path_is_treated_as_no_path_at_all() {
        let script = launcher_script(
            Path::new("/home/ada/.riabuild/grok/1"),
            "grok",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert!(script.contains(r#"*) grok_binary="" ;;"#), "{script}");
    }

    #[test]
    fn the_launcher_carries_no_node_and_no_claude_workarounds() {
        // Grok Build is a static binary, and the SSH/display handling next door
        // is read out of the Claude Code binary rather than being a fact about
        // this one. Copying either would be inventing upstream behaviour.
        let script = script();
        assert!(!script.contains("node"), "{script}");
        assert!(!script.contains("SSH_CONNECTION"), "{script}");
        assert!(!script.contains("WAYLAND_DISPLAY"), "{script}");
        assert!(!script.contains("--settings"), "{script}");
    }

    #[test]
    fn the_launcher_set_is_the_bare_name_and_nine_numbers() {
        let names = launcher_names();
        assert_eq!(names.len(), PROFILES + 1);
        assert_eq!(names[0], "grok");
        assert_eq!(names[1], "grok-1");
        assert_eq!(names[PROFILES], "grok-9");
    }

    #[test]
    fn the_bare_name_and_the_first_number_are_one_profile() {
        assert_eq!(profile_of("grok"), 1);
        assert_eq!(profile_of("grok-1"), 1);
        assert_eq!(profile_of("grok-9"), 9);
    }

    #[tokio::test]
    async fn writing_lays_down_every_launcher() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write(&ctx).await.expect("the launchers are written");
        // Safe to run twice, like every other apply().
        write(&ctx).await.expect("a second write");

        for name in launcher_names() {
            let path = ctx.paths.bin_dir().join(&name);
            assert!(is_executable(&path).await, "{name} is not executable");
        }
    }

    /// The regression that would make nine launchers worthless.
    ///
    /// Nine scripts that all export the same `GROK_HOME` look right in every
    /// other test — present, executable, carrying the bypass flag, and they run
    /// — and yet every one opens the same account. Grok Build keeps sign-ins
    /// apart per `GROK_HOME` and by nothing else, so *distinct* is the whole
    /// feature.
    #[tokio::test]
    async fn the_nine_launchers_open_nine_different_accounts() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write(&ctx).await.expect("the launchers are written");

        let mut homes = std::collections::BTreeSet::new();
        for profile in 1..=PROFILES {
            let script =
                tokio::fs::read_to_string(ctx.paths.bin_dir().join(format!("grok-{profile}")))
                    .await
                    .unwrap();
            let home = ctx.paths.grok_profile_dir(profile);
            let line = format!("GROK_HOME=\"{}\"", home.display());
            assert!(script.contains(&line), "grok-{profile} does not pin {line}");
            homes.insert(home);
        }
        assert_eq!(
            homes.len(),
            PROFILES,
            "the launchers share a GROK_HOME, so they share an account"
        );
    }

    #[tokio::test]
    async fn the_bare_name_is_the_first_profile() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write(&ctx).await.expect("the launchers are written");

        let bare = tokio::fs::read_to_string(ctx.paths.bin_dir().join("grok"))
            .await
            .unwrap();
        let first = tokio::fs::read_to_string(ctx.paths.bin_dir().join("grok-1"))
            .await
            .unwrap();
        assert_eq!(bare, first);
    }

    /// Runs the generated launcher against a real Grok Build.
    ///
    /// Everything above asserts the *text* of a shell script, which is as far
    /// as a unit test can go and is not the same as the script working. The
    /// three facts this launcher is built on are all undocumented — read out of
    /// `xai-grok-shell`'s `resolve_effective_yolo` and confirmed against the
    /// shipped 1.0.5 binary — so an upstream change should surface as a test
    /// failure rather than as broken laptops:
    ///
    /// - `--permission-mode bypassPermissions` is accepted as a **root** option
    ///   ahead of any subcommand,
    /// - Grok Build **rejects** it twice, which is why the launcher stands
    ///   aside instead of always appending,
    /// - and it does **not** reject it beside `--always-approve`, which is why
    ///   the stand-aside list does not mention that flag.
    ///
    /// `#[ignore]`d because it needs a real install: run
    /// `cargo test -- --ignored` when `MIN_VERSION` moves.
    #[tokio::test]
    #[ignore = "needs a real Grok Build install; pins the undocumented behaviour the launcher is built on"]
    async fn the_generated_launcher_runs_a_real_grok() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(grok) = runner.which("grok") else {
            panic!("grok is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join("bin");
        // Deliberately not created: the launcher creating it is the behaviour
        // under test.
        let grok_home = home.path().join("grok-home");
        tokio::fs::create_dir_all(&bin).await.unwrap();

        let launcher = bin.join("grok");
        let script = launcher_script(&grok_home, &grok.to_string_lossy(), &bin);
        tokio::fs::write(&launcher, script.as_bytes())
            .await
            .unwrap();
        make_executable(&launcher).await.unwrap();

        let path = launcher.to_string_lossy().into_owned();
        let version = runner
            .run(&path, &["--version"], &RunOptions::default())
            .await
            .expect("the launcher runs");
        assert!(version.ok(), "{version:?}");
        assert!(version.stdout.contains("grok"), "{version:?}");
        assert!(
            tokio::fs::try_exists(&grok_home).await.unwrap(),
            "the launcher did not create its GROK_HOME"
        );

        // The invocation an unconditional bypass would break, failing in Grok
        // Build's parser and naming a flag the developer never typed.
        let chosen = runner
            .run(
                &path,
                &["--permission-mode", "plan", "--version"],
                &RunOptions::default(),
            )
            .await
            .expect("the launcher runs");
        assert!(
            chosen.ok(),
            "the launcher did not stand aside for a chosen policy: {chosen:?}"
        );

        // A root option, so it has to survive a subcommand after it.
        let sub = runner
            .run(&path, &["mcp", "list"], &RunOptions::default())
            .await
            .expect("the launcher runs");
        assert!(sub.ok(), "the bypass broke a subcommand: {sub:?}");

        // And the flag riabuild does *not* stand aside for must still be
        // accepted beside the one it adds.
        let both = runner
            .run(
                &path,
                &["--always-approve", "--version"],
                &RunOptions::default(),
            )
            .await
            .expect("the launcher runs");
        assert!(both.ok(), "--always-approve is not compatible: {both:?}");
    }

    /// Two launchers, two accounts, against a real Grok Build.
    ///
    /// This is the claim the other eight launchers exist for, and the one that
    /// cannot be made by reading a generated script: that pointing Grok Build
    /// at two `GROK_HOME`s really does keep two sign-ins apart, rather than
    /// both landing in one store the way a keychain-backed tool would.
    ///
    /// riabuild signs nobody in, so this asserts on the store rather than on a
    /// session: `auth.json` is written under the `GROK_HOME` in force and
    /// nowhere else. Reading the developer's own `~/.grok/auth.json` would be
    /// the failure, and it is the one that would silently merge two accounts.
    ///
    /// `#[ignore]`d because it needs a real install, and a sign-in performed by
    /// hand: run `grok-1 login`, then `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "needs a real Grok Build install and a hand-performed sign-in; pins that GROK_HOME really does separate sign-ins"]
    async fn a_sign_in_lands_in_the_profile_that_was_pinned() {
        let home = std::env::var("GROK_HOME").expect("run this under a launcher's GROK_HOME");
        let auth = Path::new(&home).join("auth.json");
        assert!(
            tokio::fs::try_exists(&auth).await.unwrap(),
            "no auth.json under {home} — sign in with `grok-1 login` first"
        );
        let ambient = dirs_next_home().join(".grok").join("auth.json");
        assert_ne!(
            auth, ambient,
            "the profile in force is the developer's own ~/.grok"
        );
    }

    #[cfg(test)]
    fn dirs_next_home() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
    }

    #[cfg(unix)]
    async fn is_executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::metadata(path)
            .await
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    async fn is_executable(path: &Path) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }
}

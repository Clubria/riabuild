//! `~/.riabuild/bin/grok` — the Grok Build launcher.
//!
//! The same shape as the Codex launcher next door, for the same reasons: a
//! small generated file in `bin/` that pins the config directory riabuild owns,
//! adds the flag the team wants by default, and falls back to `PATH` when the
//! binary it recorded has moved.
//!
//! ```sh
//! GROK_HOME=~/.riabuild/grok/1 grok --permission-mode bypassPermissions
//! ```
//!
//! What decides all of that is [`handoff`], in Rust; the file in `bin/` is one
//! `exec` naming `riabuild internal launch grok` — see `shims::launch`.
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
//! What is **not** copied from the Claude launcher: clearing `SSH_CONNECTION`,
//! `SSH_CLIENT` and `SSH_TTY`, the `WAYLAND_DISPLAY` claim, and the
//! `--settings` layer. All three are workarounds for behaviour read out of the
//! Claude Code binary; none is a fact about Grok Build, and asserting them here
//! would be inventing an upstream behaviour rather than accommodating one.
//!
//! Nor is Codex's Node handling. Codex is a Node script whose `bin/codex` is a
//! symlink to a `codex.js` with a `#!/usr/bin/env node` shebang, so its
//! launcher has to put riabuild's own Node on `PATH` first. Grok Build is a
//! static-pie ELF / Mach-O executable that needs nothing on `PATH` at all —
//! verified against 1.0.5 — so a launcher that carried a Node would be carrying
//! it for no one.

use anyhow::Result;
use std::path::Path;

use super::launch::{self, Handoff, Harness, Plan};

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
/// would then be rewriting under them on every run.
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

/// Whether this argument is the developer saying what permission mode they
/// want.
///
/// Both spellings, because Grok Build accepts both and rejects a second
/// `--permission-mode` in either. `--always-approve` / `--yolo` is deliberately
/// **not** matched: it is a separate boolean that Grok Build accepts happily
/// alongside this flag and which means the same thing, so standing aside for it
/// would buy nothing.
///
/// The scan reads the developer's arguments as text, so a prompt that happens
/// to contain the flag's name — `grok -p 'what does --permission-mode do?'` —
/// makes riabuild stand aside. That is the safe direction to be wrong in: the
/// session asks for approvals rather than silently granting them.
fn names_a_permission_mode(arg: &str) -> bool {
    arg == PERMISSION_MODE || arg.starts_with(&format!("{PERMISSION_MODE}="))
}

/// One Grok Build launch, decided.
///
/// The bypass goes **ahead of** the developer's own arguments, because Grok
/// Build accepts it only as a root option: after a subcommand it is "unexpected
/// argument '--permission-mode' found", so `grok mcp list` has to become `grok
/// --permission-mode … mcp list` rather than the other way round. Verified
/// against 1.0.5.
pub(super) fn handoff(handoff: Handoff, plan: &Plan) -> Handoff {
    let stands_aside = plan.args.iter().any(|arg| names_a_permission_mode(arg));
    let mut args = Vec::new();
    if !stands_aside {
        args.push(PERMISSION_MODE.to_string());
        args.push(BYPASS.to_string());
    }
    args.extend(plan.args.iter().cloned());
    handoff.with_args(args)
}

/// The plan one profile's launcher records.
///
/// The launcher is the only thing that names `GROK_HOME`. It is not exported
/// into the environment shell, for the reason `CLAUDE_CONFIG_DIR` and
/// `CODEX_HOME` are not: an exported value follows every `grok` a developer
/// starts by any route, including one they deliberately ran from outside
/// riabuild's tree — and one exported value would quietly make all nine
/// profiles share a directory.
pub fn plan(grok_home: &Path, grok: &str, bin_dir: &Path) -> Plan {
    Plan::new(
        Harness::Grok,
        grok_home.to_path_buf(),
        grok.to_string(),
        bin_dir.to_path_buf(),
    )
}

/// One profile's launcher: `grok`, or `grok-<n>`.
pub fn launcher_script(riabuild: &Path, grok_home: &Path, grok: &str, bin_dir: &Path) -> String {
    launch::script(riabuild, &plan(grok_home, grok, bin_dir))
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
    let riabuild = super::running_binary()?;

    for profile in 1..=PROFILES {
        let script = launcher_script(&riabuild, &ctx.paths.grok_profile_dir(profile), &grok, &bin);
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
    use crate::shims::launch::World;
    use crate::shims::launch::handoff as launch_handoff;
    use crate::testing::ctx_with;
    use riabuild_runner::FakeRunner;
    use std::path::PathBuf;

    const GROK_HOME: &str = "/home/ada/.riabuild/grok/1";
    const BINARY: &str = "/home/ada/.riabuild/grok/1.0.5/grok";
    const BIN_DIR: &str = "/home/ada/.riabuild/bin";

    fn fixture() -> Plan {
        plan(Path::new(GROK_HOME), BINARY, Path::new(BIN_DIR))
    }

    fn carrying(args: &[&str]) -> Plan {
        Plan {
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            ..fixture()
        }
    }

    fn laptop() -> World {
        World {
            binary_is_executable: true,
            path: format!("{BIN_DIR}:/usr/local/bin:/usr/bin"),
            ..Default::default()
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
    fn the_launcher_pins_the_config_directory_riabuild_owns() {
        let handoff = launch_handoff(&fixture(), &laptop());
        assert_eq!(value(&handoff, "GROK_HOME").as_deref(), Some(GROK_HOME));
    }

    /// Grok Build creates a `GROK_HOME` that is not there rather than refusing
    /// to start — unlike Codex, which hard-fails with "Error finding codex
    /// home". riabuild creates it anyway, so that a profile riabuild reports as
    /// present *is* present: the gap between two riabuild runs is exactly where
    /// a `rm -rf` lands, and a directory conjured by the tool on first use is
    /// not the same as one riabuild put there.
    #[tokio::test]
    async fn the_launcher_creates_a_config_directory_that_is_not_there() {
        let home = tempfile::TempDir::new().unwrap();
        let grok_home = home.path().join("grok").join("1");
        let plan = plan(&grok_home, BINARY, Path::new(BIN_DIR));

        let runner = FakeRunner::new();
        launch::run(&runner, &plan).await.expect("the launch runs");

        assert!(
            tokio::fs::metadata(&grok_home)
                .await
                .map(|meta| meta.is_dir())
                .unwrap_or(false),
            "{}",
            grok_home.display()
        );
    }

    #[test]
    fn the_launcher_bypasses_permissions_by_default() {
        // The whole feature. `bypassPermissions` and not `dontAsk`, which reads
        // like the same thing and silently denies instead.
        let args = launch_handoff(&fixture(), &laptop()).args;
        assert_eq!(args, vec![PERMISSION_MODE, BYPASS]);
        assert!(!args.iter().any(|arg| arg == "dontAsk"), "{args:?}");
    }

    #[test]
    fn the_bypass_flag_is_passed_ahead_of_the_developers_own_arguments() {
        // Grok Build accepts `--permission-mode` as a root option only: after a
        // subcommand it is "unexpected argument". A launcher that appended it
        // would break `grok mcp list` while still passing the right flag.
        let args = launch_handoff(&carrying(&["mcp", "list"]), &laptop()).args;
        assert_eq!(args, vec![PERMISSION_MODE, BYPASS, "mcp", "list"]);
    }

    #[test]
    fn the_launcher_stands_aside_for_a_permission_policy_the_developer_chose() {
        // Grok Build refuses `--permission-mode` twice, in both spellings, so
        // appending it unconditionally turns `grok --permission-mode plan` into
        // a parser error naming a flag the developer never typed.
        for chosen in [
            vec!["--permission-mode", "plan"],
            vec!["--permission-mode=plan"],
        ] {
            let args = launch_handoff(&carrying(&chosen), &laptop()).args;
            assert_eq!(
                args,
                chosen.iter().map(|a| a.to_string()).collect::<Vec<_>>()
            );
        }
    }

    /// And it does *not* stand aside for `--always-approve`, which Grok Build
    /// accepts happily beside the flag riabuild adds and which means the same
    /// thing — so standing aside for it would buy nothing.
    #[test]
    fn the_launcher_does_not_stand_aside_for_always_approve() {
        let args = launch_handoff(&carrying(&["--always-approve"]), &laptop()).args;
        assert_eq!(args, vec![PERMISSION_MODE, BYPASS, "--always-approve"]);
    }

    #[test]
    fn the_launcher_can_never_exec_itself() {
        // `bin/` leads PATH inside the environment shell, so a bare `grok`
        // would resolve straight back to this launcher. The strip is what stops
        // the fallback becoming an exec loop.
        let moved = World {
            binary_is_executable: false,
            ..laptop()
        };
        let handoff = launch_handoff(&fixture(), &moved);
        assert_eq!(handoff.program, "grok");
        assert_eq!(
            value(&handoff, "PATH").as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
    }

    #[test]
    fn a_grok_that_is_not_an_absolute_path_is_treated_as_no_path_at_all() {
        let plan = Plan {
            binary: "grok".to_string(),
            ..fixture()
        };
        let handoff = launch_handoff(&plan, &laptop());
        assert_eq!(handoff.program, "grok");
        assert_eq!(
            value(&handoff, "PATH").as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
    }

    #[test]
    fn the_launcher_carries_no_node_and_no_claude_workarounds() {
        // Grok Build is a static binary, and the SSH/display handling next door
        // is read out of the Claude Code binary rather than being a fact about
        // this one. Copying either would be inventing upstream behaviour.
        let handoff = launch_handoff(&fixture(), &laptop());
        assert!(handoff.env_remove.is_empty(), "{:?}", handoff.env_remove);
        assert_eq!(value(&handoff, "WAYLAND_DISPLAY"), None);
        // No Node directory prepended: `PATH` is untouched on the branch where
        // the recorded binary is there.
        assert_eq!(value(&handoff, "PATH"), None);
        assert!(
            !handoff.args.iter().any(|arg| arg == "--settings"),
            "{:?}",
            handoff.args
        );
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
    /// Nine launchers that all name the same `GROK_HOME` look right in every
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
            let named = format!("--home '{}'", home.display());
            assert!(
                script.contains(&named),
                "grok-{profile} does not name {named}:\n{script}"
            );
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

    /// Runs a real Grok Build with the arguments the launcher decides on.
    ///
    /// The three facts this launcher is built on are all undocumented — read
    /// out of `xai-grok-shell`'s `resolve_effective_yolo` and confirmed against
    /// the shipped 1.0.5 binary — so an upstream change should surface as a
    /// test failure rather than as broken laptops:
    ///
    /// - `--permission-mode bypassPermissions` is accepted as a **root** option
    ///   ahead of any subcommand,
    /// - Grok Build **rejects** it twice, which is why the launcher stands
    ///   aside instead of always appending,
    /// - and it does **not** reject it beside `--always-approve`, which is why
    ///   the stand-aside test does not mention that flag.
    ///
    /// Driven through [`handoff`] rather than by running the generated
    /// launcher, which would need a built riabuild binary to exec into and
    /// would be asking the same question one process further away. What is
    /// under test here is Grok Build, not the `exec` line.
    ///
    /// `#[ignore]`d because it needs a real install: run
    /// `cargo test -- --ignored` when `MIN_VERSION` moves.
    #[tokio::test]
    #[ignore = "needs a real Grok Build install; pins the undocumented behaviour the launcher is built on"]
    async fn a_real_grok_accepts_what_the_launcher_decides() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(grok) = runner.which("grok") else {
            panic!("grok is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join("bin");
        // Deliberately not created: `launch::run` creating it is the behaviour
        // under test.
        let grok_home = home.path().join("grok-home");
        tokio::fs::create_dir_all(&bin).await.unwrap();

        let world = World {
            binary_is_executable: true,
            path: std::env::var("PATH").unwrap_or_default(),
            ..Default::default()
        };
        let run = |args: Vec<String>| {
            let plan = Plan {
                args,
                ..plan(&grok_home, &grok.to_string_lossy(), &bin)
            };
            let world = world.clone();
            async move {
                tokio::fs::create_dir_all(&plan.home).await.unwrap();
                let handoff = launch_handoff(&plan, &world);
                let borrowed: Vec<&str> = handoff.args.iter().map(String::as_str).collect();
                RealRunner
                    .run(
                        &handoff.program,
                        &borrowed,
                        &RunOptions {
                            env: handoff.env.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("grok runs")
            }
        };

        let version = run(vec!["--version".to_string()]).await;
        assert!(version.ok(), "{version:?}");
        assert!(version.stdout.contains("grok"), "{version:?}");
        assert!(
            tokio::fs::try_exists(&grok_home).await.unwrap(),
            "the launch did not create its GROK_HOME"
        );

        // The invocation an unconditional bypass would break, failing in Grok
        // Build's parser and naming a flag the developer never typed.
        let chosen = run(vec![
            "--permission-mode".to_string(),
            "plan".to_string(),
            "--version".to_string(),
        ])
        .await;
        assert!(
            chosen.ok(),
            "the launcher did not stand aside for a chosen policy: {chosen:?}"
        );

        // A root option, so it has to survive a subcommand after it.
        let sub = run(vec!["mcp".to_string(), "list".to_string()]).await;
        assert!(sub.ok(), "the bypass broke a subcommand: {sub:?}");

        // And the flag riabuild does *not* stand aside for must still be
        // accepted beside the one it adds.
        let both = run(vec![
            "--always-approve".to_string(),
            "--version".to_string(),
        ])
        .await;
        assert!(both.ok(), "--always-approve is not compatible: {both:?}");
    }

    /// Two profiles, two accounts, against a real Grok Build.
    ///
    /// This is the claim the other eight launchers exist for, and the one no
    /// amount of reading the decision can make: that pointing Grok Build at two
    /// `GROK_HOME`s really does keep two sign-ins apart, rather than both
    /// landing in one store the way a keychain-backed tool would.
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
        let ambient = home_dir().join(".grok").join("auth.json");
        assert_ne!(
            auth, ambient,
            "the profile in force is the developer's own ~/.grok"
        );
    }

    fn home_dir() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
    }

    #[cfg(unix)]
    async fn is_executable(path: &PathBuf) -> bool {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::metadata(path)
            .await
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    async fn is_executable(path: &PathBuf) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }
}

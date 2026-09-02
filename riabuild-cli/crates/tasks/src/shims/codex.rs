//! `~/.riabuild/bin/codex` — the Codex CLI launcher.
//!
//! The same shape as the Claude Code launcher next door, for the same reasons:
//! a small generated file in `bin/` that pins the config directory riabuild
//! owns, adds the flags the team wants by default, and falls back to `PATH`
//! when the binary it recorded has moved.
//!
//! ```sh
//! CODEX_HOME=~/.riabuild/codex codex --yolo --dangerously-bypass-hook-trust
//! ```
//!
//! What decides all of that is [`handoff`], in Rust; the file in `bin/` is one
//! `exec` naming `riabuild internal launch codex` — see `shims::launch`.
//!
//! Nine of them, `codex-1` … `codex-9`, plus `codex` for the first — because
//! Codex keeps several sign-ins apart exactly the way Claude Code does. Its
//! credentials live in `$CODEX_HOME/auth.json` and nowhere else, so pointing it
//! at nine directories really is nine independent accounts. Verified against
//! 0.147.0: two homes hold two different API keys at the same time, and
//! `codex logout` in one leaves the other logged in.
//!
//! The nine exist from the first run rather than being created on demand.
//! Claude's are made by riabuild's own sign-in flow, which is what gives it
//! something to count; riabuild signs nobody in to Codex, so there is no moment
//! at which it would learn that a developer wants a second one. Nine empty
//! directories cost nothing, and `codex-3 login` then works the first time it
//! is typed instead of failing on a `CODEX_HOME` that is not there.
//!
//! Three things the Claude launcher does are deliberately **not** copied here.
//! Clearing `SSH_CONNECTION`, `SSH_CLIENT` and `SSH_TTY` and the
//! `WAYLAND_DISPLAY` claim are workarounds for behaviour read out of the Claude
//! Code binary, and the `--settings` layer carries the org's Claude settings;
//! none of the three is a fact about Codex. Asserting them here would be
//! inventing an upstream behaviour rather than accommodating one.

use anyhow::Result;
use std::path::Path;

use super::launch::{self, Handoff, Harness, Plan, Resolved, World};

/// What `--yolo` is short for, and what the launcher has to look for in the
/// developer's own arguments before adding it.
///
/// Codex rejects the flag twice — "cannot be used multiple times" — so a
/// launcher that always appended it would turn a developer typing the obvious
/// `codex --yolo` into a parser error naming a flag they did not type.
const YOLO_LONG: &str = "--dangerously-bypass-approvals-and-sandbox";

/// The flag riabuild adds where the developer named no policy of their own.
const YOLO: &str = "--yolo";

/// Runs the checkout's hooks without Codex stopping to ask whether it may trust
/// them. The checkout is the one riabuild selected and provisioned, and its
/// `.codex/hooks.json` is versioned code from that repository rather than a
/// program supplied by riabuild-web.
const TRUST_HOOKS: &str = "--dangerously-bypass-hook-trust";

/// How many Codex profiles riabuild makes.
///
/// Nine, for the reason `accounts::MAX` is nine: it keeps every launcher name
/// single digit, so `codex-9` is the last one and `codex-12` is an obvious typo
/// rather than something to interpret. Deliberately its own constant rather
/// than a reference to `accounts::MAX` — the two happen to agree, but a Codex
/// profile and a Claude account are not the same thing, and coupling them would
/// make changing one silently change the other.
pub const PROFILES: usize = 9;

/// Whether this argument is the developer saying what approvals they want.
///
/// The Rust spelling of the launcher's `case "$arg" in --yolo | … | -a | -a* |
/// --ask-for-approval | --ask-for-approval=*)`. Both prefix forms are here for
/// the same reason the globs were: `-a on-request` and `-aon-request` are one
/// flag spelled two ways, and so are `--ask-for-approval on-request` and
/// `--ask-for-approval=on-request`.
///
/// `-a` and `--ask-for-approval` do not overlap, which is worth saying out loud
/// because the glob made it look as though they might: `--ask-for-approval`
/// begins `--`, so it is not caught by the `-a` prefix and needs its own arm.
///
/// The scan reads the developer's arguments as text, so a prompt that happens
/// to contain a flag's name makes riabuild stand aside. That is the safe
/// direction to be wrong in: the session asks for approvals rather than
/// silently granting them.
fn names_an_approval_policy(arg: &str) -> bool {
    arg == YOLO
        || arg == YOLO_LONG
        || arg.starts_with("-a")
        || arg.starts_with("--ask-for-approval")
}

/// One Codex launch, decided.
///
/// `--yolo` is a default, not an imposition, and it has to be: Codex refuses
/// the flag twice ("cannot be used multiple times") and refuses it beside
/// `--ask-for-approval` ("cannot be used with"). Appending it unconditionally
/// would make `codex --yolo` and `codex -a on-request` — the two things a
/// developer who knows this tool is most likely to type — fail in the parser,
/// naming a flag they never typed.
pub(super) fn handoff(
    handoff: Handoff,
    plan: &Plan,
    world: &World,
    resolved: &Resolved,
) -> Handoff {
    // Codex is a Node script, not a native binary: npm installs `bin/codex` as
    // a symlink to a `codex.js` whose shebang is `#!/usr/bin/env node`, so
    // starting it asks `PATH` for a Node before Codex gets a say. riabuild's
    // own Node sits in the same directory as the binary riabuild recorded, and
    // naming it here is what makes this launcher work in a shell that never
    // sourced riabuild's environment — `ssh box bin/codex-3`, a cron entry, an
    // editor that sanitised its environment. Without it those all fail with
    // "env: 'node': No such file or directory", on a machine where Codex is
    // installed perfectly well.
    //
    // Only on this branch. The fallback has no recorded binary, so there is no
    // directory to derive a Node from, and whatever `PATH` offers is all it
    // ever had.
    let handoff = match resolved {
        Resolved::Recorded(binary) => match Path::new(binary).parent() {
            Some(node_bin) if !node_bin.as_os_str().is_empty() => {
                handoff.env("PATH", format!("{}:{}", node_bin.display(), world.path))
            }
            _ => handoff,
        },
        Resolved::OnPath { .. } => handoff,
    };

    let stands_aside = plan.args.iter().any(|arg| names_an_approval_policy(arg));
    let mut args = Vec::new();
    if !stands_aside {
        args.push(YOLO.to_string());
    }
    if !plan.args.iter().any(|arg| arg == TRUST_HOOKS) {
        args.push(TRUST_HOOKS.to_string());
    }
    let bare_and_interactive = plan.args.is_empty() && world.stdin_is_tty && world.stdout_is_tty;
    if bare_and_interactive {
        args.push("agents".to_string());
    }
    args.extend(plan.args.iter().cloned());
    handoff.with_args(args)
}

/// The plan one profile's launcher records.
///
/// The launcher is the only thing that names `CODEX_HOME`. It is not exported
/// into the environment shell, for the reason `CLAUDE_CONFIG_DIR` is not: an
/// exported value follows every `codex` a developer starts by any route,
/// including one they deliberately ran from outside riabuild's tree.
pub fn plan(codex_home: &Path, codex: &str, bin_dir: &Path) -> Plan {
    Plan::new(
        Harness::Codex,
        codex_home.to_path_buf(),
        codex.to_string(),
        bin_dir.to_path_buf(),
    )
}

/// One profile's launcher: `codex`, or `codex-<n>`.
pub fn launcher_script(riabuild: &Path, codex_home: &Path, codex: &str, bin_dir: &Path) -> String {
    launch::script(riabuild, &plan(codex_home, codex, bin_dir))
}

/// Writes `codex` and `codex-1` … `codex-9`.
///
/// `codex` and `codex-1` are the same script, which is what makes the bare name
/// mean profile 1 — the shape `shims::write_all` already uses for `claude` and
/// `claude-1`.
///
/// Each goes through [`shims::write_script`](super::write_script), like every
/// other generated script, so it is landed by rename and then made executable:
/// the hazard is an interrupt mid-write leaving a truncated launcher that fails
/// with a shell syntax error.
pub async fn write(ctx: &crate::Ctx) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    let codex = ctx.codex();
    let riabuild = super::running_binary()?;

    for profile in 1..=PROFILES {
        let script = launcher_script(
            &riabuild,
            &ctx.paths.codex_profile_dir(profile),
            &codex,
            &bin,
        );
        super::write_script(&bin, &format!("codex-{profile}"), &script).await?;
        if profile == 1 {
            super::write_script(&bin, "codex", &script).await?;
        }
    }
    Ok(())
}

/// Every launcher this module owns, in the order `check()` reports them.
///
/// One list, so a `check()` that verifies the set and an `apply()` that writes
/// it cannot come to disagree about what the set is.
pub fn launcher_names() -> Vec<String> {
    let mut names = vec!["codex".to_string()];
    names.extend((1..=PROFILES).map(|profile| format!("codex-{profile}")));
    names
}

/// Which profile a launcher opens: `codex` and `codex-1` both open profile 1.
pub fn profile_of(name: &str) -> usize {
    name.strip_prefix("codex-")
        .and_then(|number| number.parse().ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shims::launch::handoff as launch_handoff;
    use crate::testing::ctx_with;
    use riabuild_runner::FakeRunner;
    use std::path::PathBuf;

    const CODEX_HOME: &str = "/home/ada/.riabuild/codex/3";
    const NODE_BIN: &str = "/home/ada/.riabuild/node/24.19.0/bin";
    const BINARY: &str = "/home/ada/.riabuild/node/24.19.0/bin/codex";
    const BIN_DIR: &str = "/home/ada/.riabuild/bin";

    fn fixture() -> Plan {
        plan(Path::new(CODEX_HOME), BINARY, Path::new(BIN_DIR))
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
        assert_eq!(value(&handoff, "CODEX_HOME").as_deref(), Some(CODEX_HOME));
    }

    /// Codex hard-fails on a missing `CODEX_HOME` — "Error finding codex home"
    /// — rather than creating one, so a launcher that only *named* it would
    /// break the moment a developer cleaned up, and stay broken until the next
    /// `riabuild`. Created on every launch, not only by the setup task, because
    /// the gap between two riabuild runs is exactly where a `rm -rf` lands.
    #[tokio::test]
    async fn the_launcher_creates_a_config_directory_that_is_not_there() {
        let home = tempfile::TempDir::new().unwrap();
        let codex_home = home.path().join("codex").join("3");
        let plan = plan(&codex_home, BINARY, Path::new(BIN_DIR));

        let runner = FakeRunner::new();
        launch::run(&runner, &plan).await.expect("the launch runs");

        assert!(
            tokio::fs::metadata(&codex_home)
                .await
                .map(|meta| meta.is_dir())
                .unwrap_or(false),
            "{}",
            codex_home.display()
        );
    }

    #[test]
    fn the_launcher_adds_yolo_by_default() {
        assert_eq!(
            launch_handoff(&fixture(), &laptop()).args,
            vec![YOLO, TRUST_HOOKS]
        );
    }

    #[test]
    fn a_bare_interactive_launch_opens_the_agents_view() {
        let interactive = World {
            stdin_is_tty: true,
            stdout_is_tty: true,
            ..laptop()
        };
        assert_eq!(
            launch_handoff(&fixture(), &interactive).args,
            vec![YOLO, TRUST_HOOKS, "agents"]
        );
    }

    #[test]
    fn a_noninteractive_or_explicit_launch_does_not_open_the_agents_view() {
        let stdin_is_a_pipe = World {
            stdin_is_tty: false,
            stdout_is_tty: true,
            ..laptop()
        };
        assert_eq!(
            launch_handoff(&fixture(), &stdin_is_a_pipe).args,
            vec![YOLO, TRUST_HOOKS]
        );

        let interactive = World {
            stdin_is_tty: true,
            stdout_is_tty: true,
            ..laptop()
        };
        assert_eq!(
            launch_handoff(&carrying(&["resume"]), &interactive).args,
            vec![YOLO, TRUST_HOOKS, "resume"]
        );
    }

    #[test]
    fn the_launcher_autotrusts_checkout_hooks_without_duplicating_the_flag() {
        assert_eq!(
            launch_handoff(&carrying(&[TRUST_HOOKS, "--version"]), &laptop()).args,
            vec![YOLO, TRUST_HOOKS, "--version"]
        );
    }

    #[test]
    fn the_launcher_stands_aside_for_an_approval_policy_the_developer_chose() {
        // Codex refuses --yolo twice and refuses it beside --ask-for-approval,
        // so appending it unconditionally turns `codex --yolo` and
        // `codex -a on-request` into parser errors naming a flag the developer
        // never typed. Every spelling, because the two `=`/attached forms are
        // the ones a glob makes look optional.
        for chosen in [
            vec!["--yolo"],
            vec![YOLO_LONG],
            vec!["-a", "on-request"],
            vec!["-aon-request"],
            vec!["--ask-for-approval", "never"],
            vec!["--ask-for-approval=never"],
        ] {
            let args = launch_handoff(&carrying(&chosen), &laptop()).args;
            assert_eq!(
                args,
                std::iter::once(TRUST_HOOKS.to_string())
                    .chain(chosen.iter().map(|a| a.to_string()))
                    .collect::<Vec<_>>(),
                "riabuild added a flag beside the developer's own policy"
            );
        }
    }

    /// And it does *not* stand aside for anything else, or the default is a
    /// default in name only.
    #[test]
    fn a_launch_that_names_no_policy_still_gets_the_default() {
        let args = launch_handoff(&carrying(&["exec", "--full-auto"]), &laptop()).args;
        assert_eq!(args, vec![YOLO, TRUST_HOOKS, "exec", "--full-auto"]);
    }

    #[test]
    fn the_launcher_can_never_exec_itself() {
        // `bin/` leads PATH inside the environment shell, so a bare `codex`
        // would resolve straight back to this launcher. The strip is what stops
        // the fallback becoming an exec loop.
        let moved = World {
            binary_is_executable: false,
            ..laptop()
        };
        let handoff = launch_handoff(&fixture(), &moved);
        assert_eq!(handoff.program, "codex");
        assert_eq!(
            value(&handoff, "PATH").as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
    }

    #[test]
    fn a_codex_with_no_pinned_node_is_treated_as_no_path_at_all() {
        // The bare name is what `Ctx::codex()` falls back to before a Node is
        // pinned, and an executable test on it would be satisfied by a file of
        // that name in whatever directory the developer is standing in.
        let plan = Plan {
            binary: "codex".to_string(),
            ..fixture()
        };
        let handoff = launch_handoff(&plan, &laptop());
        assert_eq!(handoff.program, "codex");
        assert_eq!(
            value(&handoff, "PATH").as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
    }

    /// Codex is a Node script — npm installs `bin/codex` as a symlink to a
    /// `codex.js` whose shebang is `#!/usr/bin/env node` — so a launcher that
    /// named only the binary works exactly on the machines that happen to have
    /// a Node of their own. The server shape is the one that fails: a
    /// non-interactive SSH exec whose `PATH` is `/usr/local/bin:/usr/bin:/bin`
    /// and carries no Node at all.
    #[test]
    fn the_launcher_carries_riabuilds_own_node() {
        let server = World {
            binary_is_executable: true,
            path: "/usr/local/bin:/usr/bin:/bin".to_string(),
            ..Default::default()
        };
        let handoff = launch_handoff(&fixture(), &server);
        assert_eq!(
            value(&handoff, "PATH").as_deref(),
            Some(&*format!("{NODE_BIN}:/usr/local/bin:/usr/bin:/bin")),
        );
    }

    /// Prepended rather than replacing: the ambient `PATH` is how the tools
    /// Codex shells out to are found, and a launcher that cleared it would
    /// trade one missing-program failure for several.
    #[test]
    fn the_node_directory_leads_the_path_rather_than_replacing_it() {
        let handoff = launch_handoff(&fixture(), &laptop());
        let path = value(&handoff, "PATH").expect("PATH");
        assert!(path.starts_with(&format!("{NODE_BIN}:")), "{path}");
        assert!(path.ends_with("/usr/local/bin:/usr/bin"), "{path}");
    }

    /// The fallback has no recorded binary, so there is no directory to derive
    /// a Node from — carrying one anyway would mean inventing a path.
    #[test]
    fn the_fallback_branch_carries_no_node_directory() {
        let moved = World {
            binary_is_executable: false,
            ..laptop()
        };
        let path = value(&launch_handoff(&fixture(), &moved), "PATH").expect("PATH");
        assert!(!path.contains(NODE_BIN), "{path}");
    }

    #[test]
    fn the_launcher_set_is_the_bare_name_and_nine_numbers() {
        let names = launcher_names();
        assert_eq!(names.len(), PROFILES + 1);
        assert_eq!(names[0], "codex");
        assert_eq!(names[1], "codex-1");
        assert_eq!(names[PROFILES], "codex-9");
    }

    #[test]
    fn the_bare_name_and_the_first_number_are_one_profile() {
        assert_eq!(profile_of("codex"), 1);
        assert_eq!(profile_of("codex-1"), 1);
        assert_eq!(profile_of("codex-9"), 9);
    }

    #[tokio::test]
    async fn writing_lays_down_every_launcher() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write(&ctx).await.expect("the launchers are written");

        for name in launcher_names() {
            let path = ctx.paths.bin_dir().join(&name);
            assert!(is_executable(&path).await, "{name} is not executable");
        }
    }

    #[tokio::test]
    async fn the_launcher_is_written_executable() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write(&ctx).await.expect("the launcher is written");

        let path = ctx.paths.bin_dir().join("codex");
        let script = tokio::fs::read_to_string(&path)
            .await
            .expect("read it back");
        assert!(
            script.contains(&ctx.paths.codex_dir().to_string_lossy().into_owned()),
            "{script}"
        );
        assert!(is_executable(&path).await, "{}", path.display());
    }

    /// Runs a real Codex with the arguments and environment the launcher
    /// decides on.
    ///
    /// The three facts this launcher is built on — `--yolo` accepted ahead of a
    /// subcommand, Codex refusing a `CODEX_HOME` that does not exist, and Codex
    /// rejecting `--yolo` beside `--ask-for-approval` or twice — are all
    /// undocumented, so an upstream change should surface as a test failure
    /// rather than as broken laptops.
    ///
    /// Driven through [`handoff`] rather than by running the generated
    /// launcher, which would need a built riabuild binary to exec into and
    /// would be asking the same question one process further away. What is
    /// under test here is Codex, not the `exec` line.
    ///
    /// `#[ignore]`d because it needs a real install: run
    /// `cargo test -- --ignored` when `MIN_VERSION` moves.
    #[tokio::test]
    #[ignore = "needs a real Codex CLI install; pins the undocumented behaviour the launcher is built on"]
    async fn a_real_codex_accepts_what_the_launcher_decides() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(codex) = runner.which("codex") else {
            panic!("codex is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        // Deliberately not created: `launch::run` creating it is the behaviour
        // under test, and a Codex pointed at a missing one refuses to start.
        let codex_home = home.path().join("codex");

        let world = World {
            binary_is_executable: true,
            path: std::env::var("PATH").unwrap_or_default(),
            ..Default::default()
        };
        let run = |args: Vec<String>| {
            let plan = Plan {
                args,
                ..plan(
                    &codex_home,
                    &codex.to_string_lossy(),
                    &home.path().join("bin"),
                )
            };
            let world = world.clone();
            async move {
                // What `launch::run` does before the hand-over.
                tokio::fs::create_dir_all(&plan.home).await.unwrap();
                let handoff = launch_handoff(&plan, &world);
                let borrowed: Vec<&str> = handoff.args.iter().map(String::as_str).collect();
                RealRunner
                    .run(
                        &handoff.program,
                        &borrowed,
                        &RunOptions {
                            env: handoff.env.clone(),
                            env_remove: handoff.env_remove.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("codex runs")
            }
        };

        let version = run(vec!["--version".to_string()]).await;
        assert!(version.ok(), "{version:?}");
        assert!(version.stdout.contains("codex-cli"), "{version:?}");
        assert!(
            tokio::fs::try_exists(&codex_home).await.unwrap(),
            "the launch creates the CODEX_HOME Codex refuses to start without"
        );

        // The two invocations an unconditional `--yolo` would break, each
        // failing in Codex's parser and naming a flag the developer never
        // typed.
        for args in [
            vec!["--yolo".to_string(), "--version".to_string()],
            vec![
                "-a".to_string(),
                "on-request".to_string(),
                "--version".to_string(),
            ],
        ] {
            let output = run(args.clone()).await;
            assert!(output.ok(), "{args:?} was rejected: {output:?}");
        }
    }

    /// Two profiles, two accounts, against a real Codex.
    ///
    /// This is the claim the other eight launchers exist for, and the one no
    /// amount of reading the decision can make: that pointing Codex at two
    /// `CODEX_HOME`s really does keep two sign-ins apart, rather than both
    /// landing in one store the way a keychain-backed tool would. Nine
    /// launchers that each name a different directory and yet share an account
    /// would pass every other test in this file.
    ///
    /// Uses `login --with-api-key`, which reads the key from stdin, so it needs
    /// no browser and no real credential — Codex records whatever it is given
    /// and reports it back, which is all this asserts.
    ///
    /// `#[ignore]`d because it needs a real install: run
    /// `cargo test -- --ignored` when `MIN_VERSION` moves.
    #[tokio::test]
    #[ignore = "needs a real Codex CLI install; pins that CODEX_HOME really does separate sign-ins"]
    async fn two_profiles_hold_two_independent_logins() {
        use riabuild_runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(codex) = runner.which("codex") else {
            panic!("codex is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join("bin");
        tokio::fs::create_dir_all(&bin).await.unwrap();
        let world = World {
            binary_is_executable: true,
            path: std::env::var("PATH").unwrap_or_default(),
            ..Default::default()
        };

        // The environment each profile's launcher hands Codex — which is the
        // only thing that differs between the nine, and the whole claim under
        // test.
        let codex = codex.to_string_lossy().into_owned();
        let env_of = |profile: usize| {
            let codex_home = home.path().join("codex").join(profile.to_string());
            launch_handoff(&plan(&codex_home, &codex, &bin), &world).env
        };
        async fn call(
            codex: &str,
            env: Vec<(String, String)>,
            args: &[&str],
            stdin: Option<&str>,
        ) -> riabuild_runner::CommandOutput {
            RealRunner
                .run(
                    codex,
                    args,
                    &RunOptions {
                        env,
                        stdin: stdin.map(|text| text.as_bytes().to_vec()),
                        ..Default::default()
                    },
                )
                .await
                .expect("codex runs")
        }
        // `codex login status` reports on **stderr**, not stdout — verified
        // against 0.147.0. Reading `.stdout` here gets an empty string and an
        // assertion that fails for the wrong reason, so both streams are joined
        // rather than one being guessed at.
        async fn status(codex: &str, env: Vec<(String, String)>) -> String {
            let out = call(codex, env, &["login", "status"], None).await;
            format!("{}{}", out.stdout, out.stderr)
        }

        let first = call(
            &codex,
            env_of(1),
            &["login", "--with-api-key"],
            Some("sk-test-AAAA1111\n"),
        )
        .await;
        assert!(first.ok(), "signing in to profile 1: {first:?}");
        let second = call(
            &codex,
            env_of(2),
            &["login", "--with-api-key"],
            Some("sk-test-BBBB2222\n"),
        )
        .await;
        assert!(second.ok(), "signing in to profile 2: {second:?}");

        // Both at once. If the two shared a store, whichever signed in last
        // would be the only account either profile could see.
        let one = status(&codex, env_of(1)).await;
        let two = status(&codex, env_of(2)).await;
        assert!(
            one.contains("A1111"),
            "profile 1 lost its own account: {one}"
        );
        assert!(
            two.contains("B2222"),
            "profile 2 lost its own account: {two}"
        );

        // And leaving one does not leave the other.
        let out = call(&codex, env_of(2), &["logout"], None).await;
        assert!(out.ok(), "{out:?}");
        let one = status(&codex, env_of(1)).await;
        let two = status(&codex, env_of(2)).await;
        assert!(
            one.contains("A1111"),
            "profile 2's logout took profile 1: {one}"
        );
        assert!(
            !two.contains("B2222"),
            "profile 2 is still signed in after logout: {two}"
        );
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

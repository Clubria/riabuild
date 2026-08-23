//! Performing the upgrade, and re-execing into what it installed.

use anyhow::Result;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_theme::Theme;
use riabuild_ui::{Failure, Ui};

use super::strategy::{Strategy, strategy_on};

/// Upgrades riabuild and re-execs it with the original arguments.
///
/// On Linux this asks first. The upgrade needs sudo, and a password prompt
/// appearing unannounced at startup reads as something having gone wrong — so
/// the sentence explaining what it is for comes immediately before it. Under
/// `--quiet`, or with no terminal to ask at, the command is printed instead: a
/// prompt nobody can answer is a hang.
pub async fn upgrade_and_reexec(
    runner: &dyn CommandRunner,
    ui: &Ui,
    to: &str,
    mandatory: bool,
) -> Result<()> {
    if replace_binary(runner, ui, to, mandatory).await? {
        // Never returns: `reexec` ends in `process::exit`.
        return reexec(runner).await;
    }
    Ok(())
}

/// Everything `upgrade_and_reexec` does except replacing the process, and
/// whether it got as far as needing to.
///
/// Split out because the caller ends in `process::exit`, which no test
/// survives — so with the two together the only part of this path a test could
/// reach was `run_upgrade`, against a strategy handed to it. The decision
/// above it (what owns this binary, whether to ask, what a "no" means) and the
/// re-exec below it were covered by nothing at all.
async fn replace_binary(
    runner: &dyn CommandRunner,
    ui: &Ui,
    to: &str,
    mandatory: bool,
) -> Result<bool> {
    replace_binary_on(cfg!(target_os = "macos"), runner, ui, to, mandatory).await
}

/// [`replace_binary`] with the platform question as a parameter, for the same
/// reason [`strategy_on`] takes one.
///
/// With `cfg!` inline this decided *two different things* depending on which
/// CI host ran it, and a test could only ever see one of them: the apt case
/// below scripted `dpkg -S` and got `Apt` on Linux, while the macOS job asked
/// the same fake for `brew`, found none, and got `Unmanaged` — a mandatory
/// upgrade that stops the run. That is not a test failing on one platform, it
/// is a test asserting nothing on it. The parameter is what lets both branches
/// of the decision — including a Mac's, from Linux — be driven through the
/// fake runner, and
/// `the_upgrade_asks_the_platform_it_is_actually_running_on` pins the wrapper
/// so the untested branch cannot move up into it.
async fn replace_binary_on(
    is_macos: bool,
    runner: &dyn CommandRunner,
    ui: &Ui,
    to: &str,
    mandatory: bool,
) -> Result<bool> {
    let executable = std::env::current_exe()?;
    let executable = executable.to_string_lossy().into_owned();
    let strategy = strategy_on(is_macos, runner, &executable).await;

    if strategy == Strategy::Unmanaged {
        decline(
            ui,
            &strategy,
            to,
            mandatory,
            "no package manager installed it",
        )?;
        return Ok(false);
    }

    if strategy.needs_sudo() {
        match ui.confirm(&format!(
            "A newer riabuild is available ({to}). Update now?"
        )) {
            Some(true) => {}
            Some(false) => {
                decline(ui, &strategy, to, mandatory, "you said no")?;
                return Ok(false);
            }
            None => {
                decline(
                    ui,
                    &strategy,
                    to,
                    mandatory,
                    "riabuild had no terminal to ask at",
                )?;
                return Ok(false);
            }
        }
    }

    // A blank line above, the same way `heading` gets one. This now runs
    // before any command has printed anything — including the banner, since
    // the check moved ahead of `provision` — so the line is the first thing on
    // the screen and reads as its own stage of the run rather than as a
    // footnote on whatever came before it.
    ui.info("");
    ui.info(&format!("Updating riabuild to {to}…"));

    if !run_upgrade(runner, &strategy, ui.theme()).await? {
        if mandatory {
            return Err(Failure::new(
                format!("updating riabuild to {to}, which your team now requires"),
                format!(
                    "Run `{}` yourself and read what it says.",
                    strategy.command()
                ),
            )
            .command(strategy.command())
            .into());
        }
        // An optional update that failed is not worth blocking anyone over.
        ui.warn("Could not update riabuild; carrying on with this version.");
        return Ok(false);
    }

    Ok(true)
}

/// Reports an upgrade that was not performed, and stops only if it had to be.
fn decline(ui: &Ui, strategy: &Strategy, to: &str, mandatory: bool, why: &str) -> Result<()> {
    if mandatory {
        return Err(Failure::new(
            format!("updating riabuild to {to}, which your team now requires"),
            format!("Run `{}`, then run `riabuild` again.", strategy.command()),
        )
        .detail(format!("riabuild did not update it because {why}"))
        .command(strategy.command())
        .into());
    }
    ui.note(&format!(
        "riabuild {to} is available. Update with `{}`.",
        strategy.command()
    ));
    Ok(())
}

async fn run_upgrade(
    runner: &dyn CommandRunner,
    strategy: &Strategy,
    theme: Theme,
) -> Result<bool> {
    // Interactive, so the sudo password prompt reaches the developer's
    // terminal — and subdued, because apt and dnf print more, louder, and in
    // their own colours than anything riabuild says around them. This is the
    // one command in a run whose output riabuild takes responsibility for the
    // look of; the pty is what makes that possible without taking the
    // controlling terminal away from `sudo`.
    let subdued = RunOptions {
        subdued: Some(theme),
        ..Default::default()
    };
    match strategy {
        Strategy::Homebrew => Ok(runner
            .run(
                "brew",
                &["upgrade", "clubria/tap/riabuild"],
                &RunOptions::default(),
            )
            .await?
            .ok()),
        // `apt-get update` first, because the version the developer was just
        // offered is one the local package lists have never seen.
        Strategy::Apt => {
            let refreshed = runner
                .run_interactive("sudo", &["apt-get", "update"], &subdued)
                .await?;
            if refreshed != 0 {
                return Ok(false);
            }
            let installed = runner
                .run_interactive(
                    "sudo",
                    &["apt-get", "install", "--only-upgrade", "-y", "riabuild"],
                    &subdued,
                )
                .await?;
            Ok(installed == 0)
        }
        // `--refresh` for the same reason: dnf would otherwise serve cached
        // metadata until `metadata_expire` passes and report nothing to do.
        Strategy::Dnf => Ok(runner
            .run_interactive(
                "sudo",
                &["dnf", "upgrade", "-y", "--refresh", "riabuild"],
                &subdued,
            )
            .await?
            == 0),
        Strategy::Unmanaged => Ok(false),
    }
}

async fn reexec(runner: &dyn CommandRunner) -> Result<()> {
    let code = run_replacement(runner).await?;
    std::process::exit(code);
}

/// Runs the binary that has just replaced this one, with this run's own
/// arguments, and hands back its exit code.
///
/// Separated from the `process::exit` above it for the same reason
/// `replace_binary` is: a test can call this and read back what was run, and
/// cannot call anything that exits. The two things it must get right are the
/// arguments — a developer typed them once and must not have to again — and
/// `RIABUILD_UPDATED`, without which a build that still reports old re-execs
/// itself for ever.
async fn run_replacement(runner: &dyn CommandRunner) -> Result<i32> {
    let executable = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    runner
        .run_interactive(
            &executable.to_string_lossy(),
            &arg_refs,
            &RunOptions {
                // Prevents an upgrade loop if the new build still reports old.
                env: vec![("RIABUILD_UPDATED".into(), "1".into())],
                // Not subdued: the child here is riabuild, whose output is
                // already themed. Dimming it would dim a whole second run and
                // nest its indent under the first.
                ..Default::default()
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    /// A Mac whose Homebrew prefix contains *this test binary*, because that
    /// is the path `replace_binary_on` probes: it asks `current_exe()` who
    /// owns it, so a fixed `/opt/homebrew` would answer "not this one" and
    /// send every macOS assertion below down the `Unmanaged` branch instead.
    fn a_mac_that_poured_this_binary() -> FakeRunner {
        let executable = std::env::current_exe().expect("a test binary has a path");
        let prefix = executable
            .parent()
            .expect("and a directory it lives in")
            .to_string_lossy()
            .into_owned();
        FakeRunner::new()
            .with("brew --prefix", 0, &format!("{prefix}\n"), "")
            .with("brew list --formula riabuild", 0, "riabuild\n", "")
            .with("brew upgrade", 0, "", "")
    }

    #[tokio::test]
    async fn a_package_manager_upgrade_prints_through_riabuild() {
        // apt is the loudest thing in a run and none of it is riabuild's.
        let runner = FakeRunner::new();
        run_upgrade(&runner, &Strategy::Apt, Theme::plain())
            .await
            .expect("upgrade");
        assert_eq!(
            runner.subdued_calls(),
            vec![
                "sudo apt-get update",
                "sudo apt-get install --only-upgrade -y riabuild",
            ]
        );
    }

    #[tokio::test]
    async fn dnf_is_subdued_the_same_way_apt_is() {
        let runner = FakeRunner::new();
        run_upgrade(&runner, &Strategy::Dnf, Theme::plain())
            .await
            .expect("upgrade");
        assert_eq!(
            runner.subdued_calls(),
            vec!["sudo dnf upgrade -y --refresh riabuild"]
        );
    }

    #[tokio::test]
    async fn homebrew_is_captured_rather_than_subdued() {
        // `brew` goes through `run`, which never reaches a terminal — there is
        // nothing there to subdue, and asking for a pty would be noise.
        let runner = FakeRunner::new();
        run_upgrade(&runner, &Strategy::Homebrew, Theme::plain())
            .await
            .expect("upgrade");
        assert_eq!(runner.subdued_calls(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_mandatory_upgrade_runs_the_owning_package_managers_command() {
        // The whole of `upgrade_and_reexec` bar the `process::exit`: what owns
        // this binary, the question before the sudo, the command itself, and
        // the answer that says the process should now be replaced. Every one
        // of those was previously reachable only by a developer's laptop.
        //
        // `false` rather than the host's own answer: this scripts a Linux
        // machine, and asked with `cfg!` it scripted one only on Linux — the
        // macOS job ran the same fake through the Homebrew probe, found no
        // brew, and got the `Unmanaged` refusal instead of any of this.
        let runner = FakeRunner::new()
            .with("dpkg -S", 0, "riabuild: /usr/bin/riabuild", "")
            .with("sudo apt-get", 0, "", "");

        let replaced = replace_binary_on(false, &runner, &Ui::scripted(["y"]), "2026.08.12", true)
            .await
            .expect("the upgrade runs");

        assert!(replaced, "a successful upgrade has to be re-exec'd into");
        assert_eq!(
            runner.subdued_calls(),
            vec![
                "sudo apt-get update",
                "sudo apt-get install --only-upgrade -y riabuild",
            ]
        );
    }

    #[tokio::test]
    async fn a_mandatory_upgrade_on_a_mac_pours_the_tap_without_asking() {
        // The same path on the other platform, asserted on every host rather
        // than only on the one CI job with a Mac. The `Ui` has no answer to
        // give, which is the assertion that nothing here asks: a question put
        // to it reads as "could not ask", and a mandatory upgrade that cannot
        // ask stops the run rather than reaching `brew`.
        let runner = a_mac_that_poured_this_binary();

        let replaced = replace_binary_on(
            true,
            &runner,
            &Ui::scripted([] as [&str; 0]),
            "2026.08.12",
            true,
        )
        .await
        .expect("the upgrade runs");

        assert!(replaced, "a successful upgrade has to be re-exec'd into");
        assert!(
            runner
                .calls()
                .iter()
                .any(|call| call == "brew upgrade clubria/tap/riabuild"),
            "{:?}",
            runner.calls()
        );
        assert!(runner.calls().iter().all(|call| !call.contains("sudo")));
        assert_eq!(runner.subdued_calls(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn the_upgrade_asks_the_platform_it_is_actually_running_on() {
        // The one `cfg!(target_os = "macos")` left in this path, and this is
        // what pins it: a parameter without a test on the wrapper *moves* the
        // untested branch up a level rather than removing it. dpkg owns this
        // binary and there is no brew, so the two platforms disagree and a
        // wrapper hardcoding either one fails on one of the two CI hosts —
        // which is the whole point. Optional rather than
        // mandatory so neither answer ends the run: Linux upgrades, a Mac
        // finds nothing that owns this and prints how to do it by hand.
        let runner = FakeRunner::new()
            .with("dpkg -S", 0, "riabuild: /usr/bin/riabuild", "")
            .with("sudo apt-get", 0, "", "");

        let replaced = replace_binary(&runner, &Ui::scripted(["y"]), "2026.08.12", false)
            .await
            .expect("an optional upgrade is never a failure");

        assert_eq!(replaced, !cfg!(target_os = "macos"));
    }

    #[tokio::test]
    async fn a_binary_nothing_owns_is_never_replaced_and_never_re_execd() {
        // `Unmanaged` plus `mandatory` is the one combination that must stop
        // the run with an instruction rather than pretend: there is nothing
        // riabuild can do here, and a re-exec would loop on the same binary.
        // Both platforms, because a bare machine reaches this on both — the
        // Mac through "no brew", Linux through "no dpkg and no rpm".
        for is_macos in [false, true] {
            let runner = FakeRunner::new();

            let error = replace_binary_on(is_macos, &runner, &Ui::new(true), "2026.08.12", true)
                .await
                .expect_err("a floor this machine cannot climb past stops the run");

            assert!(format!("{error}").contains("riabuild"), "{error}");
            assert!(runner.calls().iter().all(|call| !call.contains("sudo")));
        }
    }

    #[tokio::test]
    async fn an_optional_upgrade_nothing_owns_carries_on() {
        for is_macos in [false, true] {
            assert!(
                !replace_binary_on(
                    is_macos,
                    &FakeRunner::new(),
                    &Ui::new(true),
                    "2026.08.12",
                    false
                )
                .await
                .expect("an optional upgrade that cannot happen is not a failure"),
                "is_macos: {is_macos}"
            );
        }
    }

    #[tokio::test]
    async fn the_replacement_carries_this_runs_arguments_and_the_no_loop_flag() {
        // Without `RIABUILD_UPDATED` a build that still reports old re-execs
        // itself for ever; without the arguments the developer types their
        // command a second time.
        let runner = FakeRunner::new();
        let executable = std::env::current_exe().unwrap();
        let executable = executable.to_string_lossy().into_owned();

        run_replacement(&runner)
            .await
            .expect("the replacement runs");

        let call = runner
            .calls()
            .into_iter()
            .find(|call| call.starts_with(&executable))
            .expect("the freshly installed binary is what runs");
        let expected: Vec<String> = std::env::args().skip(1).collect();
        for argument in &expected {
            assert!(call.contains(argument.as_str()), "{call}");
        }
        assert_eq!(
            runner.env_of(&executable),
            vec![("RIABUILD_UPDATED".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn a_declined_mandatory_upgrade_stops_with_the_command_to_run() {
        let ui = Ui::new(true);
        let error = decline(&ui, &Strategy::Apt, "2026.08.12", true, "you said no")
            .unwrap_err()
            .to_string();
        assert!(error.contains("apt-get install"), "{error}");
    }

    #[test]
    fn a_declined_optional_upgrade_carries_on() {
        let ui = Ui::new(true);
        assert!(decline(&ui, &Strategy::Dnf, "2026.08.12", false, "you said no").is_ok());
    }
}

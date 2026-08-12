//! Startup update check.
//!
//! `GET /api/v1/org/config` already tells riabuild the version floor and the
//! latest release — a request it makes anyway. Only when a newer version
//! actually exists does it shell out to a package manager. Running `brew
//! update` on every launch would cost 5–30 s of tap fetching before the
//! developer sees anything.

use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use crate::version;
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do.
    Continue,
    /// A newer build exists; upgrade and re-exec.
    Upgrade { to: String, mandatory: bool },
}

/// Decides what to do, without touching the machine.
pub fn decide(current: &str, min: &str, latest: &str, already_updated: bool) -> Action {
    let below_floor = !version::at_least(current, min);

    if already_updated {
        // The upgrade ran and we came back still too old: looping would spin
        // forever, so let the request fail with the server's own 409 instead.
        return Action::Continue;
    }

    if below_floor {
        return Action::Upgrade {
            to: latest.to_string(),
            mandatory: true,
        };
    }
    if version::at_least(latest, current) && !version::same(latest, current) {
        return Action::Upgrade {
            to: latest.to_string(),
            mandatory: false,
        };
    }
    Action::Continue
}

pub fn already_updated() -> bool {
    std::env::var("RIABUILD_UPDATED").is_ok_and(|value| value == "1")
}

/// How this copy of riabuild can replace itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    /// macOS, through the tap. Needs no sudo.
    Homebrew,
    /// Installed from the apt repository.
    Apt,
    /// Installed from the dnf repository.
    Dnf,
    /// No package manager owns this binary — a `cargo build`, an unpacked
    /// tarball, a copy someone moved. Nothing here can upgrade it.
    Unmanaged,
}

impl Strategy {
    /// The command a developer would run by hand.
    pub fn command(&self) -> &'static str {
        match self {
            Strategy::Homebrew => "brew upgrade clubria/tap/riabuild",
            Strategy::Apt => "sudo apt-get update && sudo apt-get install --only-upgrade riabuild",
            Strategy::Dnf => "sudo dnf upgrade --refresh riabuild",
            Strategy::Unmanaged => "reinstall riabuild — see https://riabuild.clubria.com",
        }
    }

    /// Whether performing it will ask for a password.
    pub fn needs_sudo(&self) -> bool {
        matches!(self, Strategy::Apt | Strategy::Dnf)
    }
}

/// Works out how this binary was installed, by asking which package manager
/// owns it.
///
/// Deliberately not "which package manager is installed": a Fedora machine can
/// have `apt` on it, and a riabuild built with `cargo` or unpacked from a
/// tarball is owned by nothing at all. Running `sudo apt-get install riabuild`
/// against that second case either fails or installs a *second* riabuild at a
/// different path — leaving the developer on the old one forever while every
/// upgrade reports success.
pub async fn strategy(runner: &dyn CommandRunner, executable: &str) -> Strategy {
    if cfg!(target_os = "macos") {
        return Strategy::Homebrew;
    }

    if owns(runner, "dpkg", &["-S", executable]).await {
        return Strategy::Apt;
    }
    if owns(runner, "rpm", &["-qf", executable]).await {
        return Strategy::Dnf;
    }
    Strategy::Unmanaged
}

async fn owns(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> bool {
    if runner.which(program).is_none() {
        return false;
    }
    runner
        .run(program, args, &RunOptions::default())
        .await
        .map(|output| output.ok())
        .unwrap_or(false)
}

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
    let executable = std::env::current_exe()?;
    let executable = executable.to_string_lossy().into_owned();
    let strategy = strategy(runner, &executable).await;

    if strategy == Strategy::Unmanaged {
        return decline(
            ui,
            &strategy,
            to,
            mandatory,
            "no package manager installed it",
        );
    }

    if strategy.needs_sudo() {
        match ui.confirm(&format!(
            "A newer riabuild is available ({to}). Update now?"
        )) {
            Some(true) => {}
            Some(false) => {
                return decline(ui, &strategy, to, mandatory, "you said no");
            }
            None => {
                return decline(
                    ui,
                    &strategy,
                    to,
                    mandatory,
                    "riabuild had no terminal to ask at",
                );
            }
        }
    }

    // Separated from the identity lines above it the same way `heading` is:
    // the update is a new stage of the run, not a footnote on who signed in.
    ui.info("");
    ui.info(&format!("Updating riabuild to {to}…"));

    if !run_upgrade(runner, &strategy).await? {
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
        return Ok(());
    }

    reexec(runner).await
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

async fn run_upgrade(runner: &dyn CommandRunner, strategy: &Strategy) -> Result<bool> {
    match strategy {
        Strategy::Homebrew => Ok(runner
            .run(
                "brew",
                &["upgrade", "clubria/tap/riabuild"],
                &RunOptions::default(),
            )
            .await?
            .ok()),
        // Interactive so the sudo password prompt reaches the developer's
        // terminal. `apt-get update` first, because the version the developer
        // was just offered is one the local package lists have never seen.
        Strategy::Apt => {
            let refreshed = runner
                .run_interactive("sudo", &["apt-get", "update"], &RunOptions::default())
                .await?;
            if refreshed != 0 {
                return Ok(false);
            }
            let installed = runner
                .run_interactive(
                    "sudo",
                    &["apt-get", "install", "--only-upgrade", "-y", "riabuild"],
                    &RunOptions::default(),
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
                &RunOptions::default(),
            )
            .await?
            == 0),
        Strategy::Unmanaged => Ok(false),
    }
}

async fn reexec(runner: &dyn CommandRunner) -> Result<()> {
    let executable = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let code = runner
        .run_interactive(
            &executable.to_string_lossy(),
            &arg_refs,
            &RunOptions {
                // Prevents an upgrade loop if the new build still reports old.
                env: vec![("RIABUILD_UPDATED".into(), "1".into())],
                ..Default::default()
            },
        )
        .await?;
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    #[test]
    fn a_current_build_carries_on() {
        assert_eq!(decide("0.4.0", "0.1.0", "0.4.0", false), Action::Continue);
    }

    #[test]
    fn a_newer_release_is_an_optional_upgrade() {
        assert_eq!(
            decide("0.4.0", "0.1.0", "0.5.0", false),
            Action::Upgrade {
                to: "0.5.0".into(),
                mandatory: false
            }
        );
    }

    #[test]
    fn below_the_floor_the_upgrade_is_mandatory() {
        assert_eq!(
            decide("0.1.0", "0.4.0", "0.5.0", false),
            Action::Upgrade {
                to: "0.5.0".into(),
                mandatory: true
            }
        );
    }

    #[test]
    fn a_build_ahead_of_the_published_latest_is_left_alone() {
        // A developer running a local build must not be downgraded on every run.
        assert_eq!(decide("0.9.0", "0.1.0", "0.5.0", false), Action::Continue);
    }

    #[test]
    fn a_later_release_date_is_an_optional_upgrade() {
        assert_eq!(
            decide("2026.08.04", "2026.08.01", "2026.08.12", false),
            Action::Upgrade {
                to: "2026.08.12".into(),
                mandatory: false
            }
        );
    }

    #[test]
    fn below_the_date_floor_the_upgrade_is_mandatory() {
        assert_eq!(
            decide("2026.07.30", "2026.08.04", "2026.08.12", false),
            Action::Upgrade {
                to: "2026.08.12".into(),
                mandatory: true
            }
        );
    }

    #[test]
    fn a_same_day_rebuild_is_offered() {
        // The fourth component is how a second release on one date is named.
        assert_eq!(
            decide("2026.08.04", "2026.08.01", "2026.08.04.1", false),
            Action::Upgrade {
                to: "2026.08.04.1".into(),
                mandatory: false
            }
        );
    }

    #[test]
    fn a_development_build_never_upgrades_itself() {
        // Working on riabuild must not make riabuild replace the binary being
        // worked on. The dev sentinel sits above every real date, so this goes
        // through the same path as a local build ahead of the published latest.
        assert_eq!(
            decide("9999.0.0-dev", "2026.08.04", "2026.08.12", false),
            Action::Continue
        );
    }

    #[test]
    fn the_second_pass_never_upgrades_again() {
        // RIABUILD_UPDATED is what stops an upgrade that does not take from
        // re-execing forever.
        assert_eq!(decide("0.1.0", "0.4.0", "0.5.0", true), Action::Continue);
    }

    #[tokio::test]
    async fn a_deb_install_upgrades_with_apt() {
        let runner = FakeRunner::new().with("dpkg -S", 0, "riabuild: /usr/bin/riabuild", "");
        let chosen = strategy(&runner, "/usr/bin/riabuild").await;
        let expected = if cfg!(target_os = "macos") {
            Strategy::Homebrew
        } else {
            Strategy::Apt
        };
        assert_eq!(chosen, expected);
    }

    #[tokio::test]
    async fn an_rpm_install_upgrades_with_dnf() {
        // dpkg is present and answers "no such file", which is the state on a
        // Fedora box that happens to have apt installed. Asking *which tools
        // exist* rather than *what owns this binary* gets this one wrong.
        let runner = FakeRunner::new()
            .with("dpkg -S", 1, "", "dpkg-query: no path found matching")
            .with("rpm -qf", 0, "riabuild-2026.08.06-1.x86_64", "");
        let chosen = strategy(&runner, "/usr/bin/riabuild").await;
        let expected = if cfg!(target_os = "macos") {
            Strategy::Homebrew
        } else {
            Strategy::Dnf
        };
        assert_eq!(chosen, expected);
    }

    #[tokio::test]
    async fn a_binary_no_package_manager_owns_is_never_sudoed_over() {
        // A `cargo build`, or a tarball someone unpacked. `sudo apt-get install
        // riabuild` here would install a *second* riabuild somewhere else and
        // leave this one in place — an upgrade that reports success forever
        // while changing nothing.
        let runner = FakeRunner::new()
            .with("dpkg -S", 1, "", "no path found matching")
            .with("rpm -qf", 1, "", "file /home/ada/riabuild is not owned");
        let chosen = strategy(&runner, "/home/ada/riabuild").await;
        if cfg!(target_os = "macos") {
            assert_eq!(chosen, Strategy::Homebrew);
        } else {
            assert_eq!(chosen, Strategy::Unmanaged);
            assert!(!chosen.needs_sudo());
        }
    }

    #[tokio::test]
    async fn a_machine_with_neither_packaging_tool_is_unmanaged() {
        let chosen = strategy(&FakeRunner::new(), "/usr/local/bin/riabuild").await;
        let expected = if cfg!(target_os = "macos") {
            Strategy::Homebrew
        } else {
            Strategy::Unmanaged
        };
        assert_eq!(chosen, expected);
    }

    #[test]
    fn only_the_linux_strategies_ask_for_a_password() {
        // brew never needs sudo, which is why macOS keeps upgrading silently.
        assert!(!Strategy::Homebrew.needs_sudo());
        assert!(Strategy::Apt.needs_sudo());
        assert!(Strategy::Dnf.needs_sudo());
        assert!(!Strategy::Unmanaged.needs_sudo());
    }

    #[test]
    fn every_strategy_can_name_the_command_a_developer_would_run() {
        // Printed whenever riabuild cannot or may not upgrade itself, so it has
        // to be something that can actually be pasted into a shell.
        assert!(Strategy::Homebrew.command().contains("brew upgrade"));
        assert!(Strategy::Apt.command().contains("apt-get install"));
        assert!(Strategy::Dnf.command().contains("dnf upgrade"));
        for strategy in [Strategy::Apt, Strategy::Dnf] {
            assert!(strategy.command().starts_with("sudo "), "{strategy:?}");
        }
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

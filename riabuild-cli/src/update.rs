//! Startup update check.
//!
//! `GET /api/v1/org/config` already tells riabuild the version floor and the
//! latest release — a request it makes anyway. Only when a newer version
//! actually exists does it shell out to Homebrew. Running `brew update` on every
//! launch would cost 5–30 s of tap fetching before the developer sees anything.

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

/// Runs `brew upgrade` and re-execs this binary with the original arguments.
pub fn upgrade_and_reexec(
    runner: &dyn CommandRunner,
    ui: &Ui,
    to: &str,
    mandatory: bool,
) -> Result<()> {
    ui.info(&format!("Updating riabuild to {to}…"));

    let output = runner.run(
        "brew",
        &["upgrade", "clubria/tap/riabuild"],
        &RunOptions::default(),
    )?;

    if !output.ok() {
        if mandatory {
            return Err(Failure::new(
                format!("updating riabuild to {to}, which your team now requires"),
                "Run `brew upgrade clubria/tap/riabuild` yourself and read what it says.",
            )
            .command("brew upgrade clubria/tap/riabuild")
            .detail(output.stderr)
            .into());
        }
        // An optional update that failed is not worth blocking anyone over.
        ui.warn("Could not update riabuild; carrying on with this version.");
        return Ok(());
    }

    let executable = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let code = runner.run_interactive(
        &executable.to_string_lossy(),
        &arg_refs,
        &RunOptions {
            // Prevents an upgrade loop if the new build still reports old.
            env: vec![("RIABUILD_UPDATED".into(), "1".into())],
            ..Default::default()
        },
    )?;
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_second_pass_never_upgrades_again() {
        // RIABUILD_UPDATED is what stops an upgrade that does not take from
        // re-execing forever.
        assert_eq!(decide("0.1.0", "0.4.0", "0.5.0", true), Action::Continue);
    }
}

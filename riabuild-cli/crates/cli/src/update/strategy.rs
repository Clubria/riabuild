//! What owns the riabuild that is running, and therefore what can replace it.
//!
//! Asked of the machine rather than assumed from the platform: a Fedora box
//! can have `apt` on it, and a riabuild built with `cargo` is owned by
//! nothing. The platform arrives as a *parameter* for the reason
//! `keychain::select` takes one — with `cfg!` inline, the other branch is
//! compiled out of the test binary and only the host's own answer could ever
//! be asserted.

use riabuild_runner::{CommandRunner, RunOptions};

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
///
/// **macOS is asked the same question, and used not to be.** This returned
/// `Homebrew` for any macOS binary with no probe at all — the exact failure the
/// Linux branch exists to avoid, on the platform where a `cargo build` riabuild
/// is most common, since this is the repository developers work on from Macs.
/// A tarball or `cargo` riabuild there ran `brew upgrade clubria/tap/riabuild`,
/// which installs a second copy under the brew prefix and leaves this one
/// running, for ever, reporting success every time.
///
/// The platform arrives as `is_macos` rather than being read from `cfg!`
/// here, and there is exactly one `cfg!(target_os = "macos")` above this — in
/// [`super::apply`], pinned by
/// `the_upgrade_asks_the_platform_it_is_actually_running_on`.
///
/// `cfg!` compiles the other branch out of the test binary, so with it inline
/// the tests below could assert nothing about `dpkg`/`rpm` on the macOS job
/// and nothing about Homebrew on Linux — each of them was an `if cfg!(macos)
/// { … } else { … }` asserting only whichever half the host happened to be.
/// That is the half-applied pattern `riabuild-cli/CLAUDE.md` records under
/// `keychain::select`, which shipped two binary-less releases. The parameter
/// is what makes both branches assertable on every host; keeping only one
/// `cfg!` in the whole path is what stops the untested branch from moving up
/// a level instead of going away.
pub async fn strategy_on(is_macos: bool, runner: &dyn CommandRunner, executable: &str) -> Strategy {
    if is_macos {
        return match homebrew_owns(runner, executable).await {
            true => Strategy::Homebrew,
            false => Strategy::Unmanaged,
        };
    }

    if owns(runner, "dpkg", &["-S", executable]).await {
        return Strategy::Apt;
    }
    if owns(runner, "rpm", &["-qf", executable]).await {
        return Strategy::Dnf;
    }
    Strategy::Unmanaged
}

/// Whether Homebrew owns `executable` — macOS's answer to `dpkg -S`.
///
/// Two questions, and both are needed. `brew --prefix` names the one directory
/// tree brew installs into (the Cellar and the `bin` symlinks into it both sit
/// under it), so a binary outside it was put there by something else — a
/// tarball, a `cargo build`, a copy somebody moved — and `brew upgrade` would
/// install a second riabuild rather than replace this one. And the formula has
/// to actually be installed, because `brew upgrade` on a formula brew has
/// never installed is not an upgrade of anything.
///
/// `brew --prefix` rather than a hardcoded `/opt/homebrew`: Apple silicon and
/// Intel disagree about it, and a developer may have moved it.
async fn homebrew_owns(runner: &dyn CommandRunner, executable: &str) -> bool {
    if runner.which("brew").is_none() {
        return false;
    }
    let Ok(output) = runner
        .run("brew", &["--prefix"], &RunOptions::default())
        .await
    else {
        return false;
    };
    let prefix = output.stdout.trim();
    if !output.ok() || prefix.is_empty() {
        return false;
    }
    if !std::path::Path::new(executable).starts_with(prefix) {
        return false;
    }
    owns(runner, "brew", &["list", "--formula", "riabuild"]).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    /// A Mac with Homebrew installed and riabuild poured from the tap.
    fn a_mac_with_brew() -> FakeRunner {
        FakeRunner::new()
            .with("brew --prefix", 0, "/opt/homebrew\n", "")
            .with("brew list --formula riabuild", 0, "riabuild\n", "")
    }

    #[tokio::test]
    async fn a_deb_install_upgrades_with_apt() {
        let runner = FakeRunner::new().with("dpkg -S", 0, "riabuild: /usr/bin/riabuild", "");
        assert_eq!(
            strategy_on(false, &runner, "/usr/bin/riabuild").await,
            Strategy::Apt
        );
    }

    #[tokio::test]
    async fn an_rpm_install_upgrades_with_dnf() {
        // dpkg is present and answers "no such file", which is the state on a
        // Fedora box that happens to have apt installed. Asking *which tools
        // exist* rather than *what owns this binary* gets this one wrong.
        let runner = FakeRunner::new()
            .with("dpkg -S", 1, "", "dpkg-query: no path found matching")
            .with("rpm -qf", 0, "riabuild-2026.08.06-1.x86_64", "");
        assert_eq!(
            strategy_on(false, &runner, "/usr/bin/riabuild").await,
            Strategy::Dnf
        );
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
        let chosen = strategy_on(false, &runner, "/home/ada/riabuild").await;
        assert_eq!(chosen, Strategy::Unmanaged);
        assert!(!chosen.needs_sudo());
    }

    #[tokio::test]
    async fn a_machine_with_neither_packaging_tool_is_unmanaged() {
        assert_eq!(
            strategy_on(false, &FakeRunner::new(), "/usr/local/bin/riabuild").await,
            Strategy::Unmanaged
        );
    }

    #[tokio::test]
    async fn a_riabuild_poured_from_the_tap_upgrades_with_brew() {
        // The macOS branch, asserted on every host rather than only on the one
        // CI job that has a Mac.
        assert_eq!(
            strategy_on(true, &a_mac_with_brew(), "/opt/homebrew/bin/riabuild").await,
            Strategy::Homebrew
        );
        // The Cellar path the `bin` symlink points into is under the same
        // prefix, and `current_exe` may hand back either.
        assert_eq!(
            strategy_on(
                true,
                &a_mac_with_brew(),
                "/opt/homebrew/Cellar/riabuild/2026.08.12/bin/riabuild"
            )
            .await,
            Strategy::Homebrew
        );
    }

    #[tokio::test]
    async fn a_tarball_riabuild_on_a_mac_is_unmanaged_rather_than_brews() {
        // The bug: macOS returned `Homebrew` with no ownership probe at all.
        // `brew upgrade clubria/tap/riabuild` against this binary installs a
        // *second* riabuild under the brew prefix and leaves this one running
        // — the same "upgrade reports success forever while changing nothing"
        // the Linux branch has always guarded against, and this is the
        // platform riabuild itself is developed on.
        let chosen = strategy_on(true, &a_mac_with_brew(), "/Users/ada/bin/riabuild").await;
        assert_eq!(chosen, Strategy::Unmanaged);
        assert!(!chosen.needs_sudo());
    }

    #[tokio::test]
    async fn a_mac_with_no_homebrew_at_all_is_unmanaged() {
        assert_eq!(
            strategy_on(true, &FakeRunner::new(), "/usr/local/bin/riabuild").await,
            Strategy::Unmanaged
        );
    }

    #[tokio::test]
    async fn a_mac_where_the_formula_was_never_installed_is_unmanaged() {
        // Under the prefix, but brew has no `riabuild` to upgrade — a copy
        // dropped into `/opt/homebrew/bin` by hand.
        let runner = FakeRunner::new()
            .with("brew --prefix", 0, "/opt/homebrew\n", "")
            .with(
                "brew list --formula riabuild",
                1,
                "",
                "No available formula",
            );
        assert_eq!(
            strategy_on(true, &runner, "/opt/homebrew/bin/riabuild").await,
            Strategy::Unmanaged
        );
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
}

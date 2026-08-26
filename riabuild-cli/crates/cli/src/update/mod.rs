//! Startup update check.
//!
//! `GET /api/v1/org/config` already tells riabuild the version floor and the
//! latest release — a request it makes anyway. Only when a newer version
//! actually exists does it shell out to a package manager. Running `brew
//! update` on every launch would cost 5–30 s of tap fetching before the
//! developer sees anything.
//!
//! It runs on **every** command a developer types, not just the setup flow,
//! which is why [`applies_to`] exists to name the handful it must not run on.//!
//! Three files. This one decides *whether* to upgrade — the floor, the latest
//! release, and the commands this must not run on. [`strategy`] answers what
//! owns the running binary, and [`apply`] performs the upgrade and re-execs.

use crate::cli::Command;
use riabuild_tasks::Ctx;
use riabuild_version as version;

mod apply;
mod strategy;

pub use apply::upgrade_and_reexec;

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

/// Whether this invocation may replace the binary running it.
///
/// riabuild keeps itself current on every command a developer types, so that
/// no path can quietly go on running last month's build. The rule for the
/// exceptions is one sentence: **riabuild updates on every command whose
/// stdout is a terminal a human is reading.** The four that answer `false`
/// are the ones whose stdout is a payload, or that must work on a machine
/// riabuild cannot read:
///
/// - **`internal …`** — plumbing the laptop runs on a server over SSH, never
///   typed by anyone. `askpass` is the sharp one: `ssh` runs it from inside an
///   authentication attempt, several times per `riabuild remote`, and reads
///   its stdout *as the password*. An "Updating riabuild…" line printed here
///   would be the answer riabuild gave the prompt.
/// - **`channel …`** — the clipboard and browser shims, which run on every
///   Ctrl+V and whose stdout is a payload, not a page.
/// - **`env`** — prints `export` lines for a shell to evaluate. `Ui::info`
///   writes to stdout, so an upgrade notice would be evaluated as shell.
/// - **`reset`** — dispatched before the tree is read at all, because the
///   state a check would want may be the reason someone is resetting.
///
/// This is why the update cannot happen "before argv is parsed": telling those
/// four apart from `riabuild status` is exactly what parsing argv is for.
///
/// Matched exhaustively for the same reason [`crate::opens_shell`] is — adding
/// a subcommand should be a compile error here, not a silent `false`.
pub fn applies_to(command: Option<&Command>) -> bool {
    match command {
        None
        | Some(
            Command::Login
            | Command::Logout
            | Command::Status
            | Command::Shell
            | Command::MoveProject { .. }
            | Command::Remote { .. }
            | Command::Claude { .. }
            // Its stdout is a page a person reads, so the update runs — and it
            // runs before the alternate screen is entered, because
            // `keep_current` is the first thing `run_inner` does.
            | Command::Agents { .. },
        ) => true,
        Some(
            Command::Internal { .. }
            | Command::Channel { .. }
            | Command::Env
            | Command::Reset { .. },
        ) => false,
    }
}

/// What this run should do about its own version.
///
/// The two guards are here rather than at the call site because there is now
/// only one call site, and both are the kind of rule that must not be
/// restated: a second copy is a second thing to forget.
pub fn action_for(ctx: &Ctx) -> Action {
    // A managed server must never try to replace itself: no package manager
    // watching this binary put it there, the laptop that provisioned the
    // server did. It is also what keeps `upgrade_and_reexec`'s
    // `process::exit` away from any run holding a GitHub-session marker —
    // only a remote scope claims one, and a remote scope is exactly
    // `server.is_some()`, so `close` can never be skipped by an upgrade.
    if ctx.server.is_some() {
        return Action::Continue;
    }
    // A laptop with no session yet has no floor and no latest to compare
    // against. Not an error: `login` is moments away, and the run after it
    // has both.
    let Some(org) = &ctx.org else {
        return Action::Continue;
    };
    decide(
        &ctx.cli_version,
        &org.min_cli_version,
        &org.latest_cli_version,
        already_updated(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;
    use riabuild_runner::FakeRunner;
    use riabuild_tasks::Ctx;

    fn command_of(argv: &[&str]) -> Option<crate::cli::Command> {
        Cli::parse_from(argv).command
    }

    /// A laptop running `current`, whose org publishes `min` and `latest`.
    async fn laptop(current: &str, min: &str, latest: &str) -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        ctx.cli_version = current.to_string();
        let mut org = riabuild_tasks::testing::org_config();
        org.min_cli_version = min.to_string();
        org.latest_cli_version = latest.to_string();
        ctx.org = Some(org);
        (ctx, home)
    }

    #[test]
    fn the_default_flow_keeps_itself_up_to_date() {
        assert!(applies_to(command_of(&["riabuild"]).as_ref()));
    }

    #[test]
    fn riabuild_remote_keeps_itself_up_to_date() {
        // The laptop and the server it provisions are only ever tested as a
        // matched pair — see `remote::install::version_for_server`.
        assert!(applies_to(
            command_of(&["riabuild", "remote", "build-01"]).as_ref()
        ));
        assert!(applies_to(
            command_of(&["riabuild", "remote", "list"]).as_ref()
        ));
    }

    #[test]
    fn managing_claude_accounts_keeps_itself_up_to_date() {
        assert!(applies_to(command_of(&["riabuild", "claude"]).as_ref()));
    }

    #[test]
    fn the_askpass_shim_never_replaces_the_binary() {
        // `ssh` runs this from inside an authentication attempt and reads its
        // stdout *as the password*. An "Updating riabuild…" line printed here
        // would be the answer riabuild gave the prompt.
        assert!(!applies_to(
            command_of(&["riabuild", "internal", "askpass", "ada@box's password:"]).as_ref()
        ));
    }

    #[test]
    fn the_clipboard_shim_never_replaces_the_binary() {
        // Runs on every Ctrl+V, and its stdout is a payload, not a page.
        assert!(!applies_to(
            command_of(&["riabuild", "channel", "shim", "xclip"]).as_ref()
        ));
    }

    #[test]
    fn printing_the_environment_never_replaces_the_binary() {
        // `riabuild env` prints `export` lines for a shell to evaluate, and
        // `Ui::info` writes to stdout — so an upgrade notice would be eval'd.
        assert!(!applies_to(command_of(&["riabuild", "env"]).as_ref()));
    }

    #[test]
    fn resetting_never_replaces_the_binary() {
        // Dispatched before the tree is read at all: the state a check would
        // need may be the reason someone is resetting.
        assert!(!applies_to(command_of(&["riabuild", "reset"]).as_ref()));
    }

    #[tokio::test]
    async fn a_managed_server_never_replaces_its_own_binary() {
        // No package manager owns a server's riabuild — the laptop installed
        // it. This is also what keeps a re-exec out of any run holding a
        // GitHub-session marker: only a remote scope claims one, and a remote
        // scope is exactly `ctx.server.is_some()`.
        let (mut ctx, _home) = laptop("2026.07.30", "2026.08.04", "2026.08.12").await;
        ctx.server = Some("build-01".into());
        assert_eq!(action_for(&ctx), Action::Continue);
    }

    #[tokio::test]
    async fn a_laptop_that_is_not_signed_in_yet_is_left_alone() {
        // No session means no org config, and a version floor nobody fetched
        // is not a reason to refuse to run.
        let (mut ctx, _home) = laptop("2026.07.30", "2026.08.04", "2026.08.12").await;
        ctx.org = None;
        assert_eq!(action_for(&ctx), Action::Continue);
    }

    #[tokio::test]
    async fn a_laptop_behind_the_latest_release_is_offered_it() {
        let (ctx, _home) = laptop("2026.08.04", "2026.08.01", "2026.08.12").await;
        assert_eq!(
            action_for(&ctx),
            Action::Upgrade {
                to: "2026.08.12".into(),
                mandatory: false
            }
        );
    }

    #[tokio::test]
    async fn a_laptop_below_the_floor_must_upgrade() {
        let (ctx, _home) = laptop("2026.07.30", "2026.08.04", "2026.08.12").await;
        assert_eq!(
            action_for(&ctx),
            Action::Upgrade {
                to: "2026.08.12".into(),
                mandatory: true
            }
        );
    }

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
}

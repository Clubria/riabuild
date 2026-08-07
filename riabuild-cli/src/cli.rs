//! Command-line surface.
//!
//! `riabuild` with no arguments is the whole product: check the machine, repair
//! what drifted, drop into the environment. Everything else is a way to do less
//! than that.

use clap::{Parser, Subcommand};

/// riabuild is versioned by release date, not by semver.
///
/// The version comes from the git tag, injected by the release workflow, and
/// deliberately **not** from `CARGO_PKG_VERSION`: Cargo requires valid semver,
/// which forbids both the leading zeros in `2026.08.04` and the fourth
/// component a same-day rebuild needs. Taking it from the tag also makes the
/// tag the only place a version is written down, so a binary that reports a
/// different version than the release it shipped in is not a mistake anyone
/// can make.
///
/// A local `cargo build` has no tag, and gets a sentinel that sits above every
/// real date. That is the useful direction to fail in: it reads as obviously
/// not-a-release, it clears any `minCliVersion` the server enforces, and
/// `update::decide` already leaves a build ahead of the published latest alone
/// — so working on riabuild never triggers riabuild upgrading itself.
pub const VERSION: &str = match option_env!("RIABUILD_VERSION") {
    Some(version) => version,
    None => "9999.0.0-dev",
};

#[derive(Debug, Parser)]
#[command(
    name = "riabuild",
    version = VERSION,
    about = "Set up this machine for building with Clubria",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Where the Clubria repository should live.
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<String>,

    /// Check everything and report, changing nothing.
    #[arg(long, global = true)]
    pub check: bool,

    /// Only print what needs attention.
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// Set the machine up but do not open the environment shell.
    #[arg(long, global = true)]
    pub no_shell: bool,

    /// The SSH host key fingerprint to trust without prompting, e.g.
    /// `SHA256:qKqv...`. `riabuild remote` compares this verbatim against
    /// what the server offers and fails on a mismatch rather than prompting —
    /// it does not weaken the check, it just answers it non-interactively.
    /// This is how an unattended run (CI, a container test) gets past a
    /// prompt that has no terminal to show on.
    #[arg(long, global = true, value_name = "FINGERPRINT")]
    pub accept_host_key: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Sign this machine in to riabuild.
    Login,
    /// Forget this machine's riabuild session.
    Logout,
    /// Report what riabuild knows about this machine.
    Status,
    /// Open the Clubria environment shell without checking anything.
    Shell,
    /// Print the environment riabuild would apply, as `export` lines.
    Env,
    /// Set up a server and open the Clubria environment on it.
    Remote {
        /// A saved server's name, or `[user@]host[:port]` to add one.
        #[arg(value_name = "SERVER")]
        target: Option<String>,
        #[command(subcommand)]
        action: Option<RemoteAction>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum RemoteAction {
    /// Show the servers this machine knows about.
    List,
    /// Remove a server: its key, its session, and riabuild's traces on it.
    Forget {
        #[arg(value_name = "SERVER")]
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_version_is_always_comparable() {
        // Injected from a tag or fallen back to the dev sentinel, VERSION is
        // sent to /api/v1 and compared against minCliVersion. A value the
        // comparator cannot parse fails closed and locks the machine out.
        assert!(
            crate::version::parse(VERSION).is_some(),
            "VERSION {VERSION:?} does not parse as a version"
        );
    }

    #[test]
    fn bare_riabuild_is_the_default_flow() {
        let cli = Cli::parse_from(["riabuild"]);
        assert!(cli.command.is_none());
        assert!(!cli.check);
        assert!(!cli.no_shell);
    }

    #[test]
    fn check_mode_is_available_on_subcommands_too() {
        let cli = Cli::parse_from(["riabuild", "--check", "status"]);
        assert!(cli.check);
        assert!(matches!(cli.command, Some(Command::Status)));
    }

    #[test]
    fn a_project_path_can_be_chosen() {
        let cli = Cli::parse_from(["riabuild", "--project", "~/work/hub"]);
        assert_eq!(cli.project.as_deref(), Some("~/work/hub"));
    }

    #[test]
    fn bare_remote_reconnects_to_what_is_saved() {
        let cli = Cli::parse_from(["riabuild", "remote"]);
        assert!(matches!(
            cli.command,
            Some(Command::Remote {
                target: None,
                action: None
            })
        ));
    }

    #[test]
    fn a_remote_can_be_named_or_spelled_out() {
        let by_name = Cli::parse_from(["riabuild", "remote", "build-01"]);
        let Some(Command::Remote {
            target: Some(target),
            ..
        }) = by_name.command
        else {
            panic!("expected a target");
        };
        assert_eq!(target, "build-01");

        let spelled = Cli::parse_from(["riabuild", "remote", "ada@box:2222"]);
        let Some(Command::Remote {
            target: Some(target),
            ..
        }) = spelled.command
        else {
            panic!("expected a target");
        };
        assert_eq!(target, "ada@box:2222");
    }

    #[test]
    fn remote_has_list_and_forget() {
        let list = Cli::parse_from(["riabuild", "remote", "list"]);
        assert!(matches!(
            list.command,
            Some(Command::Remote {
                action: Some(RemoteAction::List),
                ..
            })
        ));

        let forget = Cli::parse_from(["riabuild", "remote", "forget", "build-01"]);
        let Some(Command::Remote {
            action: Some(RemoteAction::Forget { name }),
            ..
        }) = forget.command
        else {
            panic!("expected forget");
        };
        assert_eq!(name, "build-01");
    }

    #[test]
    fn the_check_flag_still_works_with_remote() {
        let cli = Cli::parse_from(["riabuild", "--check", "remote", "build-01"]);
        assert!(cli.check);
    }

    #[test]
    fn accept_host_key_feeds_the_flag_verbatim() {
        // No shape validation here — `identity::trust_host` (Task 15) does an
        // exact string comparison against what `ssh-keyscan` offers, so the
        // CLI layer's job is only to carry the developer's text through
        // unmodified, not to guess at what a valid fingerprint looks like.
        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y",
        ]);
        assert_eq!(
            cli.accept_host_key.as_deref(),
            Some("SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y")
        );
    }

    #[test]
    fn accept_host_key_is_absent_by_default() {
        let cli = Cli::parse_from(["riabuild", "remote", "build-01"]);
        assert_eq!(cli.accept_host_key, None);
    }
}

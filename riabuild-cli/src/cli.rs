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
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign this machine in to riabuild.
    Login,
    /// Forget this machine's riabuild session.
    Logout,
    /// Report what riabuild knows about this machine.
    Status,
    /// Open the Clubria environment shell without checking anything.
    Shell,
    /// Move the Clubria checkout somewhere else.
    MoveProject {
        /// Where to move it to. Asked for if left out.
        #[arg(value_name = "PATH")]
        path: Option<String>,
    },
    /// Print the environment riabuild would apply, as `export` lines.
    Env,
    /// The laptop channel: what makes paste work over a remote session.
    Channel {
        #[command(subcommand)]
        action: ChannelAction,
    },
    /// Remove `~/.riabuild` so the next run sets this machine up from scratch.
    ///
    /// Runs no setup tasks: the point of a reset is the machine no check can
    /// repair, and checking first would mean fixing the tree about to be
    /// deleted.
    Reset {
        /// Remove it without asking.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ChannelAction {
    /// Serve this laptop's clipboard to a remote session.
    ///
    /// Hidden: started by the remote flow, not by a developer.
    #[command(hide = true)]
    Agent {
        /// Where to listen. Defaults to the session's runtime directory.
        #[arg(long, value_name = "PATH")]
        socket: Option<String>,
    },
    /// Stand in for `xclip` or `wl-paste` on the server.
    ///
    /// Hidden: invoked by the generated shims in `~/.riabuild/bin`.
    #[command(hide = true)]
    Shim {
        /// The tool being shadowed.
        tool: String,
        /// That tool's own arguments, passed through untouched.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Report whether the clipboard channel is up.
    Status,
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
    fn the_checkout_can_be_moved_with_or_without_a_path() {
        // Without one it asks; with one it is scriptable, and usable over a
        // session that has no terminal to ask through.
        let cli = Cli::parse_from(["riabuild", "move-project"]);
        assert!(matches!(
            cli.command,
            Some(Command::MoveProject { path: None })
        ));

        let cli = Cli::parse_from(["riabuild", "move-project", "~/work/hub"]);
        let Some(Command::MoveProject { path }) = cli.command else {
            panic!("expected move-project");
        };
        assert_eq!(path.as_deref(), Some("~/work/hub"));
    }

    #[test]
    fn reset_asks_before_removing_anything() {
        let cli = Cli::parse_from(["riabuild", "reset"]);
        assert!(matches!(cli.command, Some(Command::Reset { yes: false })));
    }

    #[test]
    fn reset_can_be_told_not_to_ask() {
        let cli = Cli::parse_from(["riabuild", "reset", "--yes"]);
        assert!(matches!(cli.command, Some(Command::Reset { yes: true })));
    }

    #[test]
    fn the_shim_passes_its_arguments_through_verbatim() {
        // The generated ~/.riabuild/bin/xclip runs exactly this. Flags that
        // look like riabuild's own must reach the parser as the tool's.
        let cli = Cli::parse_from([
            "riabuild",
            "channel",
            "shim",
            "xclip",
            "-selection",
            "clipboard",
            "-t",
            "TARGETS",
            "-o",
        ]);
        let Some(Command::Channel {
            action: ChannelAction::Shim { tool, args },
        }) = cli.command
        else {
            panic!("expected a shim invocation");
        };
        assert_eq!(tool, "xclip");
        assert_eq!(args, ["-selection", "clipboard", "-t", "TARGETS", "-o"]);
    }

    /// `--quiet` is a riabuild flag, and the shim must not eat it out of the
    /// tool's own argument list.
    #[test]
    fn a_tool_flag_that_collides_with_riabuilds_own_still_reaches_the_tool() {
        let cli = Cli::parse_from(["riabuild", "channel", "shim", "xclip", "-quiet", "-o"]);
        let Some(Command::Channel {
            action: ChannelAction::Shim { args, .. },
        }) = cli.command
        else {
            panic!("expected a shim invocation");
        };
        assert_eq!(args, ["-quiet", "-o"]);
    }

    #[test]
    fn the_agent_can_be_told_where_to_listen() {
        let cli = Cli::parse_from(["riabuild", "channel", "agent", "--socket", "/tmp/a.sock"]);
        let Some(Command::Channel {
            action: ChannelAction::Agent { socket },
        }) = cli.command
        else {
            panic!("expected the agent");
        };
        assert_eq!(socket.as_deref(), Some("/tmp/a.sock"));
    }

    #[test]
    fn channel_status_is_a_plain_subcommand() {
        let cli = Cli::parse_from(["riabuild", "channel", "status"]);
        assert!(matches!(
            cli.command,
            Some(Command::Channel {
                action: ChannelAction::Status
            })
        ));
    }

    #[test]
    fn a_project_path_can_be_chosen() {
        let cli = Cli::parse_from(["riabuild", "--project", "~/work/hub"]);
        assert_eq!(cli.project.as_deref(), Some("~/work/hub"));
    }
}

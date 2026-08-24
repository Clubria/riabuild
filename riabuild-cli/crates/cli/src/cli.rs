//! Command-line surface.
//!
//! `riabuild` with no arguments is the whole product: check the machine, repair
//! what drifted, drop into the environment. Everything else is a way to do less
//! than that.

use clap::{Parser, Subcommand, ValueEnum};
use riabuild_version::VERSION;

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

    /// Where the repository you are working on should live.
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<String>,

    /// Which repository to work on, as `owner/repo` or a bare name in the org.
    ///
    /// Skips the question `riabuild` otherwise asks, which is what an
    /// unattended run or a script wants. Global rather than scoped to the
    /// default flow because `riabuild remote --repo payments build-01` has to
    /// reach the server's own riabuild, the same way `--project` does.
    #[arg(long, global = true, value_name = "OWNER/REPO")]
    pub repo: Option<String>,

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
    /// Move the Clubria checkout somewhere else.
    MoveProject {
        /// Where to move it to. Asked for if left out.
        #[arg(value_name = "PATH")]
        path: Option<String>,
    },
    /// Print the environment riabuild would apply, as `export` lines.
    Env,
    /// Set up a server and open the Clubria environment on it.
    Remote {
        /// A saved server's name, or `[user@]host[:port]` to add one.
        #[arg(value_name = "SERVER")]
        target: Option<String>,

        /// The SSH host key fingerprint the server must offer, e.g.
        /// `SHA256:qKqv...`. Compared verbatim against what it answers with,
        /// and fails the run on a mismatch. Without it riabuild trusts the key
        /// it scanned on first sight, so this flag is what turns that into a
        /// verified connection — it strengthens the check rather than skipping
        /// one. Only `riabuild remote` ever reads a host key, so this flag
        /// lives here rather than as a global: nothing about `status`,
        /// `login`, or the default flow can use it.
        #[arg(long, value_name = "FINGERPRINT", value_parser = accept_host_key_shape)]
        accept_host_key: Option<String>,

        #[command(subcommand)]
        action: Option<RemoteAction>,
    },
    /// Internal plumbing, invoked by riabuild over SSH. Not for people.
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        action: InternalAction,
    },
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
    /// Manage your Claude Code accounts.
    Claude {
        #[command(subcommand)]
        action: Option<ClaudeAction>,
    },
    /// Run Claude Code, Codex and Grok Build sessions in one window.
    ///
    /// Every session is started with that harness's approvals turned off, which
    /// is what riabuild's own launchers already do — see `Kind::bypass` in
    /// `riabuild-harness` for the three spellings.
    Agents {
        /// A harness to open a session with on start. Repeatable, and the same
        /// one twice opens two sessions.
        #[arg(long = "with", value_name = "HARNESS")]
        with: Vec<HarnessArg>,

        /// The first thing to say to each session `--with` opened.
        #[arg(long, value_name = "TEXT")]
        prompt: Option<String>,
    },
}

/// Which agent harness, as a developer spells it on the command line.
///
/// A clap type, so it lives here: `riabuild-harness::Kind` is the library's
/// word for the same thing and `dispatch` maps between them. A library crate
/// that derived `ValueEnum` would have to be compiled with the parser, which is
/// what the layout rule in `CLAUDE.md` exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HarnessArg {
    Claude,
    Codex,
    Grok,
}

#[derive(Debug, Clone, Subcommand)]
pub enum InternalAction {
    /// Read a GitHub token on stdin and hand it to `gh`.
    SeedGithub,
    /// Remove what a session that died without cleaning up left behind.
    GhSweep,
    /// Print the team's ngrok authtoken on stdout.
    ///
    /// Hidden: run by the generated `~/.riabuild/bin/ngrok` on every
    /// invocation, never by a person. Its stdout is the token itself, which is
    /// why — like `askpass` — it is one of the commands riabuild does not
    /// print anything else during.
    NgrokToken,
    /// Answer an `ssh` password prompt. Run by `ssh` itself, via SSH_ASKPASS.
    ///
    /// `trailing_var_arg` because the one argument is `ssh`'s own prompt text
    /// — `ada@box's password: `, or `Enter passphrase for key '…': ` — which
    /// riabuild neither chooses nor can quote, and which clap would otherwise
    /// try to parse as flags the moment one of them began with a dash.
    Askpass {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        prompt: Vec<String>,
    },
}

/// `host_key::fingerprint_of` (Task 15) only ever extracts a token starting
/// with `SHA256:` out of `ssh-keygen -lf` output, so a value lacking that
/// prefix can never match one — rejecting it here loses nothing that would
/// otherwise have succeeded. Letting it through instead would surface as
/// Task 15's mismatch message ("expected X, the server offered Y" under
/// "trusting <host>"), wording meant for a possible man-in-the-middle. A
/// typo or a truncated paste must not read as an attack.
fn accept_host_key_shape(value: &str) -> Result<String, String> {
    if value.starts_with("SHA256:") {
        Ok(value.to_string())
    } else {
        Err("must look like a `ssh-keygen -lf` fingerprint, e.g. `SHA256:qKqv...`".to_string())
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum ClaudeAction {
    /// List your Claude Code accounts.
    List,
    /// Add an account and sign it in.
    New,
    /// Remove an account. Later accounts move up a number.
    Delete {
        /// Which account, as shown by `riabuild claude`.
        #[arg(value_name = "NUMBER")]
        number: usize,
        /// Remove it without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Make an account the one `claude` runs.
    Primary {
        #[arg(value_name = "NUMBER")]
        number: usize,
    },
}

#[derive(Debug, Clone, Subcommand)]
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
    /// Open a link in the laptop's browser.
    ///
    /// Hidden: invoked by the generated `xdg-open` shim and by `$BROWSER`.
    #[command(hide = true)]
    Open {
        /// The link, and any options the caller passed alongside it.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Relay this server's clipboard requests to the laptop over stdio.
    ///
    /// Hidden: the laptop runs it over `ssh -T`, and its stdin and stdout are
    /// the channel itself. Running it by hand does nothing useful — there is no
    /// laptop on the other end of a terminal.
    #[command(hide = true)]
    Pump {
        /// Where to bind. Defaults to the session's runtime directory.
        #[arg(long, value_name = "PATH")]
        socket: Option<String>,
    },
    /// Report whether the clipboard channel is up.
    Status,
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
            riabuild_version::parse(VERSION).is_some(),
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
    fn bare_claude_lists_the_accounts() {
        let cli = Cli::parse_from(["riabuild", "claude"]);
        assert!(matches!(
            cli.command,
            Some(Command::Claude { action: None })
        ));
    }

    #[test]
    fn deleting_an_account_takes_a_number_and_can_skip_the_prompt() {
        let cli = Cli::parse_from(["riabuild", "claude", "delete", "3", "--yes"]);
        let Some(Command::Claude {
            action: Some(ClaudeAction::Delete { number, yes }),
        }) = cli.command
        else {
            panic!("expected claude delete");
        };
        assert_eq!(number, 3);
        assert!(yes);
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
                action: None,
                ..
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
        // Beyond the `SHA256:` prefix, no further shape validation:
        // `host_key::trust_host` (Task 15) does an exact string comparison
        // against what `ssh-keyscan` offers, so the CLI layer's job is only
        // to carry the developer's text through unmodified from there.
        let cli = Cli::parse_from([
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y",
        ]);
        let Some(Command::Remote {
            accept_host_key: Some(fingerprint),
            ..
        }) = cli.command
        else {
            panic!("expected a fingerprint");
        };
        assert_eq!(
            fingerprint,
            "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y"
        );
    }

    #[test]
    fn accept_host_key_is_absent_by_default() {
        let cli = Cli::parse_from(["riabuild", "remote", "build-01"]);
        let Some(Command::Remote {
            accept_host_key, ..
        }) = cli.command
        else {
            panic!("expected the remote command");
        };
        assert_eq!(accept_host_key, None);
    }

    #[test]
    fn accept_host_key_is_scoped_to_remote_not_global() {
        // R13: a global `Cli` field let this parse — and be silently
        // discarded — on any other subcommand, or on a bare invocation.
        // Scoped to `Command::Remote`, clap must reject both.
        assert!(
            Cli::try_parse_from(["riabuild", "--accept-host-key", "SHA256:aaaa", "status"])
                .is_err()
        );
        assert!(Cli::try_parse_from(["riabuild", "--accept-host-key", "SHA256:aaaa"]).is_err());
    }

    #[test]
    fn internal_plumbing_parses_and_stays_hidden_from_help() {
        // Not for people: no developer ever types `riabuild internal ...`
        // themselves, so it must not clutter `--help`.
        let seed = Cli::parse_from(["riabuild", "internal", "seed-github"]);
        assert!(matches!(
            seed.command,
            Some(Command::Internal {
                action: InternalAction::SeedGithub
            })
        ));

        let sweep = Cli::parse_from(["riabuild", "internal", "gh-sweep"]);
        assert!(matches!(
            sweep.command,
            Some(Command::Internal {
                action: InternalAction::GhSweep
            })
        ));

        let token = Cli::parse_from(["riabuild", "internal", "ngrok-token"]);
        assert!(matches!(
            token.command,
            Some(Command::Internal {
                action: InternalAction::NgrokToken
            })
        ));

        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("internal"), "{help}");
    }

    #[test]
    fn accept_host_key_must_look_like_a_sha256_fingerprint() {
        // Task 15's `fingerprint_of` only ever extracts a `SHA256:`-prefixed
        // token, so anything else can never match — and letting a typo or a
        // truncated paste through would surface as Task 15's
        // man-in-the-middle mismatch wording instead of a plain rejection
        // here, while the developer can still see and fix what they typed.
        assert!(
            Cli::try_parse_from([
                "riabuild",
                "remote",
                "build-01",
                "--accept-host-key",
                "qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y",
            ])
            .is_err()
        );
    }
}

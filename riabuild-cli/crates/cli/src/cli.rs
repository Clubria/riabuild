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

    /// How many setup tasks to run at the same time. Left out, as many as the
    /// dependency graph allows.
    ///
    /// riabuild runs the tasks that do not depend on each other together —
    /// four tool downloads with their sockets open at once, rather than four
    /// downloads one after another — and reports them one at a time in the
    /// order it always did. `--jobs 1` runs them one at a time as well, which
    /// is the escape hatch if a machine behaves differently under load, and
    /// the way to tell a concurrency problem apart from a task's own.
    #[arg(long, global = true, value_name = "N", value_parser = at_least_one)]
    pub jobs: Option<usize>,
}

/// Rejects `--jobs 0`, which otherwise reads as "run nothing" and silently
/// means "run everything": `wave::steps` clamps it, and a flag whose stated
/// value is not the one used is worse than one that refuses.
fn at_least_one(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(0) => Err("--jobs must be at least 1".to_string()),
        Ok(jobs) => Ok(jobs),
        Err(_) => Err(format!("`{value}` is not a whole number of jobs")),
    }
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
    /// Print the config directory riabuild points each tool at.
    ///
    /// One line per Claude Code account, Codex profile and Grok Build profile,
    /// with the variable that addresses it — `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
    /// `GROK_HOME` — and riabuild's own tree beneath them. Each account is a
    /// uuid riabuild chose, so this is the only way to find out which directory
    /// holds which login without reading a generated launcher.
    Paths,
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
    /// All three open, always. riabuild installs all three, and the two that
    /// answer one turn per process start no process until they are spoken to —
    /// so there is nothing for a developer to enable, and asking them which to
    /// enable would be a decision riabuild made them make for no benefit.
    ///
    /// Every session is started with that harness's approvals turned off, which
    /// is what riabuild's own launchers already do — see `Kind::bypass` in
    /// `riabuild-harness` for the three spellings.
    Agents {
        /// The first thing to say, asked of all three at once.
        #[arg(long, value_name = "TEXT")]
        prompt: Option<String>,
    },
}

/// Which harness `internal launch` is being asked for.
///
/// A parser-side mirror of `riabuild_tasks::shims::Harness`, because only the
/// binary may see a clap type — the rule `CLAUDE.md` states as "a library that
/// matches on a command enum has to be compiled with the parser". The two are
/// kept honest by `every_generated_launcher_parses_back_into_the_plan_that_wrote_it`,
/// which feeds real generated launchers to the real parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LaunchHarness {
    Claude,
    Codex,
    Grok,
}

impl From<LaunchHarness> for riabuild_tasks::shims::Harness {
    fn from(harness: LaunchHarness) -> Self {
        match harness {
            LaunchHarness::Claude => Self::Claude,
            LaunchHarness::Codex => Self::Codex,
            LaunchHarness::Grok => Self::Grok,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum InternalAction {
    /// Read a GitHub token on stdin and hand it to `gh`.
    SeedGithub,
    /// Remove what a session that died without cleaning up left behind.
    GhSweep,
    /// Print the team's ngrok authtoken on stdout.
    ///
    /// Hidden: it was what the generated `~/.riabuild/bin/ngrok` read on every
    /// invocation, and its stdout is the token itself, which is why — like
    /// `askpass` — it is one of the commands riabuild does not print anything
    /// else during.
    ///
    /// The shim reaches for `Ngrok` below now, which fetches the token in the
    /// process that goes on to *become* ngrok rather than handing it back
    /// through a pipe. This stays because a shim written by an older riabuild
    /// is still on disk until the next provisioning run rewrites it, and that
    /// run is not guaranteed to happen before the developer's next `ngrok`.
    NgrokToken,

    /// Run infisical with a credential brokered for this one command.
    ///
    /// Hidden: run by the generated `~/.riabuild/bin/infisical`, which is what
    /// the developer's shell finds when they type `infisical`. Everything after
    /// the subcommand is infisical's, untouched.
    ///
    /// `trailing_var_arg` and `allow_hyphen_values` for `askpass`'s reason,
    /// with the roles reversed: these arguments are not riabuild's to parse at
    /// all. `infisical export --env=dev` has to reach infisical exactly as
    /// typed, and a clap that took an interest in `--env` would fail the
    /// invocation over a flag riabuild has no opinion about.
    Infisical {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Run ngrok with the team's authtoken in its environment.
    ///
    /// What `~/.riabuild/bin/ngrok` execs. The token is fetched here, put in
    /// this process's own environment, and then this process *becomes* ngrok —
    /// so it is never in an argument list, never on a pipe, and never in a
    /// shell variable.
    Ngrok {
        /// The ngrok riabuild installed, by absolute path.
        ///
        /// Named by the shim rather than resolved here, so the shim on disk
        /// still says which ngrok it runs and `owned_tool`'s check can compare
        /// it against the version riabuild would install now.
        #[arg(long, value_name = "PATH")]
        binary: String,

        /// The developer's own arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Run one harness launcher: `claude`, `codex` or `grok`.
    ///
    /// What every launcher in `~/.riabuild/bin` execs, and the reason none of
    /// them is a shell script any more. The flags carry exactly what riabuild
    /// resolved when that launcher was written; the decisions made from them
    /// live in `riabuild_tasks::shims::launch`.
    ///
    /// Dispatched before a `Ctx` exists, like `askpass` and `channel`: this
    /// runs on every `claude` a developer types, so it must not check the
    /// machine, read the org's settings, or talk to the API.
    Launch {
        /// Which harness this launcher is for.
        harness: LaunchHarness,

        /// `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `GROK_HOME` for this profile.
        #[arg(long, value_name = "PATH")]
        home: String,

        /// The harness binary riabuild installed, as recorded when the
        /// launcher was written.
        #[arg(long, value_name = "PATH")]
        binary: String,

        /// `~/.riabuild/bin`, which is what comes off `PATH` when the recorded
        /// binary has moved.
        #[arg(long = "bin-dir", value_name = "PATH")]
        bin_dir: String,

        /// `~/.riabuild/org-settings.json`. Claude Code only.
        #[arg(long, value_name = "PATH")]
        settings: Option<String>,

        /// One per checkout this machine knows a path for, longest first.
        /// Claude Code only, and only for the agents view.
        #[arg(long = "checkout", value_name = "PATH")]
        checkouts: Vec<String>,

        /// What the agents view opens on when the working directory is under
        /// none of the checkouts. Claude Code only.
        #[arg(long = "default-checkout", value_name = "PATH")]
        default_checkout: Option<String>,

        /// Where this account's status line writes usage samples. Claude Code
        /// only, and written only for an account `riabuild claude track` has
        /// marked — its absence is what stops collection.
        #[arg(long = "usage-spool", value_name = "PATH")]
        usage_spool: Option<String>,

        /// The developer's own arguments, verbatim.
        ///
        /// `trailing_var_arg` and `allow_hyphen_values` for the reason
        /// `Askpass` has them, and more sharply: a launcher must be able to
        /// pass on any flag at all — `claude --resume`, `codex -a on-request`,
        /// `grok --permission-mode plan` — including ones riabuild has never
        /// heard of. The generated script always writes `--` ahead of them.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Send the usage samples the Claude status line has spooled.
    ///
    /// Started detached by `claude-statusline.js`, at most once a minute, and
    /// never by a person. It takes the flush lock non-blocking and gives up
    /// rather than queueing — three windows on one laptop reach for it in the
    /// same second and the winner's work makes the others unnecessary — and it
    /// fails silently, because nothing reads its output and it runs beside an
    /// interactive session that did not ask for it.
    UsageFlush,

    /// Run one agent turn.
    ///
    /// Started detached by `riabuild agents`, never by a person. It is a
    /// riabuild process rather than the harness itself because three things have
    /// to happen around the harness and none of them can be asked of a
    /// third-party binary: the session's lock has to be *held* for as long as
    /// the turn runs, its stdout has to be appended to the session's spool, and
    /// the thread id it announces has to be written down or the next turn starts
    /// a new conversation.
    AgentTurn {
        /// The session, by store id.
        #[arg(long, value_name = "ID")]
        session: String,

        /// The file holding this turn's prompt.
        ///
        /// A file rather than an argument: argv is world-readable through `ps`,
        /// and on a shared server `ps` shows other developers' processes.
        #[arg(long = "prompt-file", value_name = "PATH")]
        prompt_file: String,
    },
    /// Serve Codex to Claude Code as a subagent, over MCP on stdio.
    ///
    /// Started by Claude Code, never by a person: `claude_codex_mcp` writes the
    /// entry that names it into each account's config, and Claude Code spawns
    /// one of these per session and talks JSON-RPC to it over a pipe.
    ///
    /// **Nothing it runs may print.** Its stdout is the wire, and one line of
    /// `riabuild-ui` on it is a parse error in Claude Code with nothing to say
    /// where it came from — which is why this returns before `connect` and
    /// touches no part of riabuild that reports progress.
    McpCodex {
        /// Which Codex sign-in the subagent runs under, 1-based.
        ///
        /// The same number the launcher carries: 3 is `codex-3`. Nine profiles
        /// exist from the first run, and the first is the one a developer who
        /// has never made a second has.
        #[arg(long, value_name = "N", default_value_t = 1)]
        profile: usize,
    },
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
    /// Bind a UDP port in mosh's range, print it, and echo what arrives.
    ///
    /// The server's half of "will a mosh session work from this network". Its
    /// stdout carries one protocol line and nothing else, which is why — like
    /// `askpass` — it is dispatched before the banner and the API client
    /// exist.
    #[command(name = riabuild_remote::mosh::UDP_ECHO)]
    UdpEcho,
    /// Carry a mosh session's datagrams between this process's stdio and a
    /// local `mosh-server`.
    ///
    /// The server end of the tunnel riabuild opens when UDP cannot reach this
    /// machine. After one ready line its **stdout is the wire**, so nothing
    /// else may ever print on it.
    ///
    /// Named from the constant the laptop builds the command out of, because
    /// clap's own kebab-casing of `MoshTcp2Udp` is `mosh-tcp2-udp` — it breaks
    /// before the `Udp`, which no human spelling of this ever did. The laptop
    /// asks for `mosh-tcp2udp`, so the derived name meant every tunnel got
    /// clap's "unrecognized subcommand" on stderr, an immediately closed
    /// stdout, and a silent fall back to `ssh`.
    #[command(name = riabuild_remote::mosh::TCP2UDP)]
    MoshTcp2Udp {
        /// The loopback UDP port `mosh-server` is listening on, from the
        /// `MOSH CONNECT` line the laptop already read.
        port: u16,
    },

    /// Print the shell completion script for one shell, on stdout.
    ///
    /// Run at *packaging* time, not on a developer's machine: the Homebrew
    /// formula and `packaging/build-packages.sh` each call this once and
    /// install what it prints where bash, zsh and fish already look. Nothing a
    /// developer types ever reaches it, and nothing they have to type ever
    /// will — that is the point. riabuild does not write to anybody's
    /// `.bashrc`, `.zshrc` or `config.fish`, for the reason `CLAUDE.md` gives
    /// about `x.ai/cli/install.sh`, so a completion that needed a line adding
    /// to a rcfile would be a completion nobody has.
    ///
    /// Hidden, and under `internal`, because it is plumbing for the packages
    /// rather than a command: a developer who ran it by hand would get a shell
    /// script on their terminal and nothing to do with it.
    ///
    /// Its stdout is a payload like `askpass` and `mosh-tcp2udp` above — a
    /// script the shell sources — so it is dispatched before a `Ctx`, a
    /// banner, or the API client exists. It reads nothing about the machine,
    /// which is also what makes it safe to run inside Homebrew's build
    /// sandbox.
    Completions {
        /// Which shell to render for.
        ///
        /// `clap_complete::Shell` rather than a mirror of
        /// `riabuild_tasks::shell::Shell`: that enum answers "which shell is
        /// this developer in", has an `Other(String)` arm for the ones riabuild
        /// launches generically, and knows nothing about completion syntax.
        /// This one is the set clap can actually generate, which is the only
        /// question being asked here.
        shell: clap_complete::Shell,
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
    /// Report this account's usage to the team dashboard.
    ///
    /// Off for every account until it is asked for. A developer's accounts
    /// include personal subscriptions, and collecting from one nobody marked
    /// would ship a person's private usage to their employer.
    Track {
        /// Which account, as shown by `riabuild claude`.
        #[arg(value_name = "NUMBER")]
        number: usize,
    },
    /// Stop reporting this account's usage.
    Untrack {
        #[arg(value_name = "NUMBER")]
        number: usize,
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

    /// The server has to answer the exact subcommand the laptop sends, and
    /// nothing else in this repository was checking that it did.
    ///
    /// clap's kebab-casing of `MoshTcp2Udp` is `mosh-tcp2-udp`, so the tunnel
    /// spent its whole life being answered with "unrecognized subcommand" on
    /// stderr and a closed stdout — read by the laptop as a server that cannot
    /// run the far end, which is a silent fall back to `ssh` by design. Both
    /// names now come from one constant in `riabuild_remote::mosh`; this is the
    /// test that clap agrees, which asserting the constants against each other
    /// could not do.
    #[test]
    fn the_server_answers_the_subcommands_the_laptop_sends() {
        let cli = Cli::parse_from([
            "riabuild",
            "internal",
            riabuild_remote::mosh::TCP2UDP,
            "60001",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::Internal {
                action: InternalAction::MoshTcp2Udp { port: 60001 }
            })
        ));

        let cli = Cli::parse_from(["riabuild", "internal", riabuild_remote::mosh::UDP_ECHO]);
        assert!(matches!(
            cli.command,
            Some(Command::Internal {
                action: InternalAction::UdpEcho
            })
        ));
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

    /// The developer's own infisical command reaches infisical exactly as
    /// typed.
    ///
    /// Every one of these would be a parse error, or worse a silently rewritten
    /// invocation, if the arguments were riabuild's to read: `--env` is not a
    /// riabuild flag, `--` is meaningful to `infisical run`, and `--project` is
    /// a riabuild global that must not be claimed out of the middle of somebody
    /// else's command line.
    #[test]
    fn an_infisical_invocation_is_passed_through_untouched() {
        for typed in [
            vec!["export", "--env=dev"],
            vec!["run", "--", "pnpm", "dev", "--project", "web"],
            vec!["--version"],
        ] {
            let mut argv = vec!["riabuild", "internal", "infisical"];
            argv.extend(typed.iter().copied());
            let parsed = Cli::parse_from(argv);
            let Some(Command::Internal {
                action: InternalAction::Infisical { args },
            }) = parsed.command
            else {
                panic!("{typed:?} did not parse as an infisical passthrough");
            };
            assert_eq!(args, typed, "{typed:?}");
        }
    }

    /// And a bare `infisical`, which is the shim run with no arguments at all.
    #[test]
    fn an_infisical_invocation_with_no_arguments_parses() {
        let parsed = Cli::parse_from(["riabuild", "internal", "infisical"]);
        let Some(Command::Internal {
            action: InternalAction::Infisical { args },
        }) = parsed.command
        else {
            panic!("a bare infisical did not parse");
        };
        assert!(args.is_empty(), "{args:?}");
    }

    /// The seam nothing else can see: a launcher is written by
    /// `riabuild-tasks`, which has no parser, and read back by this parser,
    /// which has no generator. A flag renamed on one side and not the other
    /// compiles perfectly and fails on a developer's laptop the next time they
    /// type `claude` — with an error from clap, about a launcher they did not
    /// write.
    ///
    /// So this takes the real generated file, splits its `exec` line the way
    /// `/bin/sh` would, and feeds the result to the real `Cli`. It also covers
    /// the quoting: a path with a space in it survives the round trip or this
    /// fails.
    #[test]
    fn every_generated_launcher_parses_back_into_the_plan_that_wrote_it() {
        use riabuild_tasks::shims;
        use std::path::{Path, PathBuf};

        // A home directory with a space in it is an ordinary macOS home, not a
        // hypothetical one — and it is what the old launchers' `"{path}"`
        // interpolation was one careless edit away from splitting in two.
        let riabuild = Path::new("/opt/riabuild/2026.08.27/riabuild");
        let checkout = PathBuf::from("/Users/Ada Smith/Clubria/ai-builders-hub");
        let bin = Path::new("/Users/Ada Smith/.riabuild/bin");

        let cases = [
            (
                LaunchHarness::Claude,
                shims::claude::launcher_script(
                    riabuild,
                    Path::new("/Users/Ada Smith/.riabuild/claude/abc"),
                    "/Users/Ada Smith/.riabuild/node/22.23.1/bin/claude",
                    Path::new("/Users/Ada Smith/.riabuild/org-settings.json"),
                    bin,
                    shims::Checkouts {
                        all: std::slice::from_ref(&checkout),
                        default: Some(&checkout),
                    },
                    // A tracked account, so `--usage-spool` crosses the seam
                    // too — and with a space in it, like every other path here.
                    Some(Path::new("/Users/Ada Smith/.riabuild/usage/abc.ndjson")),
                ),
            ),
            (
                LaunchHarness::Codex,
                shims::codex::launcher_script(
                    riabuild,
                    Path::new("/Users/Ada Smith/.riabuild/codex/1"),
                    "/Users/Ada Smith/.riabuild/node/22.23.1/bin/codex",
                    bin,
                ),
            ),
            (
                LaunchHarness::Grok,
                shims::grok::launcher_script(
                    riabuild,
                    Path::new("/Users/Ada Smith/.riabuild/grok/1"),
                    "/Users/Ada Smith/.riabuild/grok/1.0.5/grok",
                    bin,
                ),
            ),
        ];

        for (expected, script) in cases {
            // What the developer typed, appended by the shell's `"$@"`.
            let mut argv = split_exec_line(&script);
            argv.extend(["--resume".to_string(), "a prompt with spaces".to_string()]);

            let parsed = Cli::parse_from(&argv);
            let Some(Command::Internal {
                action:
                    InternalAction::Launch {
                        harness,
                        home,
                        binary,
                        bin_dir,
                        args,
                        usage_spool,
                        ..
                    },
            }) = parsed.command
            else {
                panic!("{argv:?} did not parse as a launch");
            };
            assert_eq!(harness, expected);

            // Claude Code is the only harness that writes one today, so the
            // other two prove the flag is genuinely optional rather than
            // silently defaulted.
            let expected_spool = match expected {
                LaunchHarness::Claude => {
                    Some("/Users/Ada Smith/.riabuild/usage/abc.ndjson".to_string())
                }
                _ => None,
            };
            assert_eq!(usage_spool, expected_spool, "{argv:?}");
            assert!(home.contains("Ada Smith"), "{home}");
            assert!(binary.contains("Ada Smith"), "{binary}");
            assert_eq!(bin_dir, bin.to_string_lossy());
            // The developer's own arguments arrive whole, hyphens and spaces
            // included — which is the whole reason for `--`,
            // `trailing_var_arg` and `allow_hyphen_values`.
            assert_eq!(args, vec!["--resume", "a prompt with spaces"]);
        }
    }

    /// The `exec` line of a generated script, split into argv the way `/bin/sh`
    /// would.
    ///
    /// Only single-quoted, double-quoted and bare words, because that is all
    /// the generator ever emits — `shell_quote` wraps every value in single
    /// quotes and escapes any of its own, and the one double-quoted token is
    /// the trailing `"$@"`. That one is dropped: it is the shell appending the
    /// developer's arguments, which the caller adds itself.
    fn split_exec_line(script: &str) -> Vec<String> {
        let line = script
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("exec "))
            .unwrap_or_else(|| panic!("no exec line in:\n{script}"));

        let mut words = Vec::new();
        let mut current = String::new();
        let mut quoted = false;
        let mut started = false;
        for character in line.chars() {
            match character {
                '\'' | '"' => {
                    quoted = !quoted;
                    started = true;
                }
                ' ' if !quoted => {
                    if started {
                        words.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                other => {
                    current.push(other);
                    started = true;
                }
            }
        }
        if started {
            words.push(current);
        }
        assert!(!quoted, "unbalanced quote in: {line}");
        assert_eq!(words.first().map(String::as_str), Some("exec"));
        words.retain(|word| word != "exec" && word != "$@");
        words
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

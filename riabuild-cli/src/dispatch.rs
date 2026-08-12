//! Where the command line meets the library.
//!
//! Every function here is one `match` from a clap enum onto calls that know
//! nothing about clap. That is the whole job, and keeping it in one place is
//! what lets `channel/`, `accounts/` and `remote/` be libraries rather than
//! command handlers: a module that matches on `ChannelAction` has to be
//! compiled with the parser, and a module compiled with the parser can read
//! any flag it likes rather than the ones its caller chose to pass.
//!
//! Each of these started life inside the module it dispatches into, which is
//! the direction this drifts if nobody is watching — a handler grows one
//! `RealPaths::new()` at a time until the library it lives in cannot be built
//! without the binary.

use crate::cli::{ChannelAction, ClaudeAction, Cli, RemoteAction};
use crate::paths::{Paths, RealPaths};
use crate::runner::{CommandRunner, RealRunner};
use crate::tasks::Ctx;
use crate::ui::Ui;
use crate::{accounts, channel, remote, shims};
use anyhow::Result;
use std::sync::Arc;

/// `riabuild channel …`
///
/// Handled before the setup flow: the shim runs on every Ctrl+V and must not
/// check the machine or talk to the API.
pub async fn channel(action: &ChannelAction, quiet: bool) -> Result<i32> {
    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);

    match action {
        ChannelAction::Shim { tool, args } => {
            let Some(tool) = shims::clipboard::Tool::from_name(tool) else {
                // Not a tool riabuild shadows. The shell's own code for it,
                // rather than a silent success.
                return Ok(127);
            };
            let bin = RealPaths::new()?.bin_dir();
            Ok(
                shims::clipboard::run(tool, args, Some(channel::socket_path()), &bin, &runner)
                    .await,
            )
        }

        ChannelAction::Agent { socket } => {
            // The creating side, so the checked resolver: this is the one call
            // that can still refuse a socket belonging to somebody else, before
            // `serve` unlinks whatever is in the way and binds.
            let socket = channel::socket_path_for_create(socket.as_deref()).await?;
            let bin = RealPaths::new()?.bin_dir();
            channel::laptop_agent(runner, &bin)?.serve(&socket).await?;
            Ok(0)
        }

        ChannelAction::Open { args } => {
            Ok(shims::browser::run(args, Some(channel::socket_path())).await)
        }

        ChannelAction::Status => {
            let ui = Ui::new(quiet);
            let socket = channel::socket_path();
            match channel::client::request(&socket, &channel::protocol::Request::ChannelPing).await
            {
                Ok(_) => {
                    ui.info(&format!(
                        "Clipboard channel — connected ({})",
                        socket.display()
                    ));
                    Ok(0)
                }
                Err(error) => {
                    ui.warn(&format!("Clipboard channel — down: {error}"));
                    ui.info(
                        "Paste will not work until the laptop reconnects. Nothing else is affected.",
                    );
                    Ok(1)
                }
            }
        }
    }
}

/// `riabuild claude …`, which defaults to `list`.
pub async fn claude(ctx: &mut Ctx, action: Option<ClaudeAction>) -> Result<i32> {
    match action.unwrap_or(ClaudeAction::List) {
        ClaudeAction::List => accounts::command::list(ctx).await,
        ClaudeAction::New => accounts::command::new(ctx).await,
        ClaudeAction::Delete { number, yes } => accounts::command::delete(ctx, number, yes).await,
        ClaudeAction::Primary { number } => accounts::command::primary(ctx, number).await,
    }
}

/// `riabuild remote …`
///
/// `list` and `forget` are whole commands rather than variations on the
/// default flow — neither connects to a server — so they are named calls here
/// rather than an `Option<RemoteAction>` threaded down into `flow`.
pub async fn remote(
    ctx: &mut Ctx,
    action: Option<RemoteAction>,
    request: remote::Request,
) -> Result<i32> {
    match action {
        Some(RemoteAction::List) => remote::list(ctx).await,
        Some(RemoteAction::Forget { name }) => remote::forget_server(ctx, &name).await,
        None => remote::run(ctx, request).await,
    }
}

/// The `remote` request this invocation describes.
///
/// `--accept-host-key` is scoped to the `remote` subcommand rather than being a
/// global flag (R13 in `.superpowers/sdd/2026-08-06-remote-mode/decisions.md`),
/// so it arrives already destructured from `Command::Remote`. That is the whole
/// reason this can be a plain function: `remote/` used to reach back into
/// `cli.command` and match on it, because from down there the flag is not a
/// field of `Cli` and there was nowhere else to find it.
pub fn remote_request(
    cli: &Cli,
    target: Option<String>,
    accept_host_key: Option<String>,
) -> remote::Request {
    remote::Request {
        target,
        accept_host_key,
        check: cli.check,
        quiet: cli.quiet,
        no_shell: cli.no_shell,
        project: cli.project.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Command;
    use clap::Parser;

    const GOOD_FINGERPRINT: &str = "SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y";

    /// Builds the request the way `main` does, from a parsed command line.
    fn request_from(argv: &[&str]) -> remote::Request {
        let cli = Cli::parse_from(argv);
        let Some(Command::Remote {
            target,
            accept_host_key,
            ..
        }) = cli.command.clone()
        else {
            panic!("{argv:?} is not a `remote` invocation");
        };
        remote_request(&cli, target, accept_host_key)
    }

    #[test]
    fn accept_host_key_reaches_the_request_from_the_remote_subcommand() {
        // R13: the flag is scoped to `remote`, not global. This used to be
        // asserted against a helper inside `remote/flow.rs` that matched on
        // `cli.command` to find it — the coupling this function exists to end.
        let request = request_from(&[
            "riabuild",
            "remote",
            "build-01",
            "--accept-host-key",
            GOOD_FINGERPRINT,
        ]);
        assert_eq!(request.accept_host_key.as_deref(), Some(GOOD_FINGERPRINT));
        assert_eq!(request.target.as_deref(), Some("build-01"));

        let without = request_from(&["riabuild", "remote", "build-01"]);
        assert_eq!(without.accept_host_key, None);
    }

    #[test]
    fn the_global_flags_a_remote_run_honours_reach_the_request() {
        // `--check`, `--quiet`, `--no-shell` and `--project` are global, and
        // `flow/connect.rs` reads all four. Handing them over by name is what
        // stops it reading any other flag it likes.
        let request = request_from(&[
            "riabuild",
            "--check",
            "--quiet",
            "--no-shell",
            "--project",
            "/srv/checkout",
            "remote",
            "build-01",
        ]);
        assert!(request.check);
        assert!(request.quiet);
        assert!(request.no_shell);
        assert_eq!(request.project.as_deref(), Some("/srv/checkout"));
    }
}

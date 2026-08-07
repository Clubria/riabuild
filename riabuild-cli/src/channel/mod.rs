//! The laptop channel: a request path from a remote server back to the
//! developer's laptop.
//!
//! The server asks and the laptop decides. The operation set is compiled into
//! the binary, so a server can request only what the laptop already implements
//! — it cannot push work, extend the operation set, or execute anything. That
//! asymmetry is what makes a reverse tunnel defensible at all, and it is the
//! architecture rule "the server ships data, never logic" applied to the one
//! direction remote mode had not opened.
//!
//! The channel is strictly optional. Its absence degrades to "no clipboard"
//! and never to "environment broken": a laptop that closes its lid leaves a
//! session that still runs setup, still re-pulls rotated secrets, and still
//! opens a shell. Only paste stops.

pub mod agent;
pub mod client;
pub mod clipboard;
pub mod mime;
pub mod opener;
pub mod protocol;
pub mod resize;
pub mod supervisor;

use std::path::PathBuf;

/// The environment variable the shim reads to find the channel.
///
/// Set by remote mode in the environment shell. Its absence is how a local
/// session — where the clipboard is already the developer's own — leaves the
/// real tools alone.
pub const SOCKET_ENV: &str = "RIABUILD_CHANNEL_SOCKET";

/// Where the shim records why paste stopped working.
pub const LOG_ENV: &str = "RIABUILD_CHANNEL_LOG";

/// Where the shim should look for the channel.
///
/// Explicit configuration wins. Otherwise the runtime directory, resolved the
/// way remote mode already resolves it: `$XDG_RUNTIME_DIR`, then `$TMPDIR`,
/// then `/tmp`.
///
/// When remote mode lands, this should defer to its runtime-directory helper,
/// which additionally enforces the 0700, ownership, and symlink rules. Until
/// then it computes the same path without those checks — safe here because this
/// function only ever *reads* a path the supervisor created.
pub fn socket_path() -> PathBuf {
    if let Ok(explicit) = std::env::var(SOCKET_ENV)
        && !explicit.is_empty()
    {
        return PathBuf::from(explicit);
    }

    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .or_else(|| std::env::var("TMPDIR").ok().filter(|dir| !dir.is_empty()))
        .unwrap_or_else(|| "/tmp".to_string());

    PathBuf::from(runtime).join("riabuild").join("channel.sock")
}

use crate::cli::ChannelAction;
// `bin_dir` is a trait method, so the trait has to be in scope even though only
// the concrete `RealPaths` is named below.
use crate::paths::{Paths, RealPaths};
use crate::runner::{CommandRunner, RealRunner};
use crate::shims::clipboard::Tool;
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::sync::Arc;

/// Handled before the setup flow: the shim runs on every Ctrl+V and must not
/// check the machine or talk to the API.
pub async fn dispatch(action: &ChannelAction, quiet: bool) -> Result<i32> {
    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);

    match action {
        ChannelAction::Shim { tool, args } => {
            let Some(tool) = Tool::from_name(tool) else {
                // Not a tool riabuild shadows. The shell's own code for it,
                // rather than a silent success.
                return Ok(127);
            };
            let bin = RealPaths::new()?.bin_dir();
            Ok(crate::shims::clipboard::run(tool, args, Some(socket_path()), &bin, &runner).await)
        }

        ChannelAction::Agent { socket } => {
            let socket = socket
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(socket_path);

            // The only platform decision here is supplying the real value; the
            // decision itself lives in `clipboard::detect`, which takes the OS
            // as a parameter and is tested for every platform. Same shape as
            // `paths::default_project_dir` wrapping `default_project_dir_on`.
            let os = std::env::consts::OS;
            let wayland = std::env::var("WAYLAND_DISPLAY").ok();

            let Some(session) = clipboard::detect(&runner, os, wayland.as_deref()) else {
                return Err(Failure::new(
                    "This laptop has no clipboard tool riabuild can read",
                    clipboard::install_hint(wayland.is_some()),
                )
                .into());
            };

            let bin = RealPaths::new()?.bin_dir();
            let agent = Arc::new(agent::Agent::new(
                clipboard::backend(runner.clone(), session),
                Box::new(opener::SystemOpener::new(runner, os, &bin)),
            ));
            agent.serve(&socket).await?;
            Ok(0)
        }

        ChannelAction::Open { args } => {
            Ok(crate::shims::browser::run(args, Some(socket_path())).await)
        }

        ChannelAction::Status => {
            let ui = Ui::new(quiet);
            let socket = socket_path();
            match client::request(&socket, &protocol::Request::ChannelPing).await {
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

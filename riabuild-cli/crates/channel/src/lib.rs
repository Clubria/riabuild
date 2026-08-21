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

// The panic lints are denied workspace-wide. In tests a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture there
// is correct and this keeps the deny from forcing ceremony into every test
// module. The exemption is `test` and nothing wider — see the workspace
// manifest for what an `any(test, feature = "testing")` spelling of it costs.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

pub mod agent;
pub mod client;
pub mod clipboard;
pub mod line;
pub mod mime;
pub mod mux;
pub mod opener;
pub mod protocol;
pub mod pump;
pub mod resize;
pub mod socket;
pub mod supervisor;

/// Where the shim records why paste stopped working.
pub const LOG_ENV: &str = "RIABUILD_CHANNEL_LOG";

/// The socket's name and owner, from `socket`. Re-exported so callers name the
/// channel rather than a file inside it — and so the two answers stay one
/// import apart, since which of them a caller wants is the whole distinction
/// that module exists to draw.
pub use socket::{SOCKET_ENV, socket_path, socket_path_for_create};

use anyhow::Result;
use riabuild_runner::CommandRunner;
use riabuild_ui::Failure;
use std::path::Path;
use std::sync::Arc;

/// What this laptop can answer with: its own clipboard, and its own browser.
///
/// One construction, shared by `riabuild channel agent` below and by remote
/// mode, which serves the same agent in-process beside the shell it opened
/// (`remote::channel`). Two constructions would be two answers to "what can
/// this laptop do", and the one that drifted would be the one no developer
/// ever runs by hand.
///
/// Fails only when the laptop has no clipboard tool at all. Remote mode turns
/// that into a warning and carries on without a channel; `agent` reports it,
/// because a developer who asked for the agent asked for exactly this.
pub fn laptop_agent(runner: Arc<dyn CommandRunner>, bin: &Path) -> Result<Arc<agent::Agent>> {
    // The only platform decision here is supplying the real value; the
    // decision itself lives in `clipboard::detect`, which takes the OS as a
    // parameter and is tested for every platform. Same shape as
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

    Ok(Arc::new(agent::Agent::new(
        clipboard::backend(runner.clone(), session),
        Box::new(opener::SystemOpener::new(runner, os, bin)),
    )))
}

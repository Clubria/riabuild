//! Reading the laptop's clipboard.
//!
//! Every backend goes through `CommandRunner`, without exception. That is what
//! makes the whole channel testable with no server and no second machine
//! anywhere — a scripted `xclip` is indistinguishable from a real one.
//!
//! `read` separates "no content of that type" (`Ok(None)`) from "the tool
//! could not be run" (`Err`). An empty clipboard is the normal case and must
//! never be reported as a broken channel.
//!
//! One file per platform: X11 and Wayland share `linux`, macOS needs
//! AppleScript and gets its own.

pub mod linux;
pub mod macos;

use crate::runner::CommandRunner;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub use linux::{WaylandClipboard, X11Clipboard};
pub use macos::MacOsClipboard;

#[async_trait]
pub trait Clipboard: Send + Sync {
    /// The types currently on the clipboard, normalised, filtered and ordered.
    async fn targets(&self) -> Result<Vec<String>>;

    /// The bytes for one type, or `None` if the clipboard has no such content.
    async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>>;

    /// Puts `bytes` on the laptop's clipboard under `mime`, replacing what was
    /// there.
    ///
    /// `Ok(false)` when this laptop's clipboard has no name for that type — the
    /// same clean refusal `read` makes with `Ok(None)`, rather than a fault.
    /// `Err` is reserved for a tool that could not be run or would not take the
    /// selection.
    async fn write(&self, mime: &str, bytes: &[u8]) -> Result<bool>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    X11,
    Wayland,
    MacOs,
}

/// Which clipboard backend this laptop needs, if any.
///
/// The OS and the Wayland display are parameters rather than `cfg!` reads:
/// `cfg!` compiles every branch but one out of the test binary, so only the
/// runner's own platform could ever be asserted. `paths::default_project_dir_on`
/// is the same pattern.
pub fn detect(
    runner: &Arc<dyn CommandRunner>,
    os: &str,
    wayland_display: Option<&str>,
) -> Option<Session> {
    if os == "macos" {
        // osascript ships with every macOS and needs no check.
        return Some(Session::MacOs);
    }

    let wayland = wayland_display.is_some_and(|display| !display.is_empty());
    if wayland && runner.which("wl-paste").is_some() {
        return Some(Session::Wayland);
    }
    // A Wayland laptop can still drive xclip through XWayland, so this is
    // tried either way before giving up.
    if runner.which("xclip").is_some() {
        return Some(Session::X11);
    }
    None
}

pub fn backend(runner: Arc<dyn CommandRunner>, session: Session) -> Box<dyn Clipboard> {
    match session {
        Session::X11 => Box::new(X11Clipboard::new(runner)),
        Session::Wayland => Box::new(WaylandClipboard::new(runner)),
        Session::MacOs => Box::new(MacOsClipboard::new(runner)),
    }
}

/// The install command for a Linux laptop that has neither tool.
///
/// Named rather than described, the way `mosh-server` already is: "no
/// clipboard tool" is not something a developer can act on.
pub fn install_hint(wayland: bool) -> &'static str {
    if wayland {
        "Install wl-clipboard on this laptop: `apt install wl-clipboard`, or `dnf install wl-clipboard`."
    } else {
        "Install xclip on this laptop: `apt install xclip`, or `dnf install xclip`."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    fn arc(runner: FakeRunner) -> Arc<dyn CommandRunner> {
        Arc::new(runner)
    }

    #[test]
    fn a_wayland_session_is_detected_by_its_display_variable() {
        let runner = arc(FakeRunner::new().with("wl-paste --version", 0, "2.2.1", ""));
        assert_eq!(
            detect(&runner, "linux", Some("wayland-0")),
            Some(Session::Wayland)
        );
    }

    #[test]
    fn an_x11_session_falls_back_to_xclip() {
        let runner = arc(FakeRunner::new().with("xclip -version", 0, "xclip 0.13", ""));
        assert_eq!(detect(&runner, "linux", None), Some(Session::X11));
    }

    /// A Wayland compositor with only xclip installed still works through
    /// XWayland, so the X11 branch is tried rather than giving up.
    #[test]
    fn a_wayland_laptop_with_only_xclip_uses_xclip() {
        let runner = arc(FakeRunner::new().with("xclip -version", 0, "xclip 0.13", ""));
        assert_eq!(
            detect(&runner, "linux", Some("wayland-0")),
            Some(Session::X11)
        );
    }

    /// Naming the missing tool is the whole value of this branch: the failure
    /// otherwise reads as "paste does not work" with nothing to act on.
    #[test]
    fn a_linux_laptop_with_no_clipboard_tool_has_no_session() {
        let runner = arc(FakeRunner::new());
        assert_eq!(detect(&runner, "linux", Some("wayland-0")), None);
        assert_eq!(detect(&runner, "linux", None), None);
    }

    #[test]
    fn macos_needs_no_tool_check_because_osascript_always_exists() {
        let runner = arc(FakeRunner::new());
        assert_eq!(detect(&runner, "macos", None), Some(Session::MacOs));
    }

    #[test]
    fn the_install_hint_names_a_command_rather_than_a_problem() {
        assert!(install_hint(true).contains("wl-clipboard"));
        assert!(install_hint(false).contains("xclip"));
    }
}

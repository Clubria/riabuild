//! Opening a link on the laptop, and the record that is its audit trail.
//!
//! The one operation that reaches past the laptop into whatever application
//! claims the scheme — which is why `decode_request` settled the scheme before
//! anything here ran, and why this is the only operation that writes to the
//! channel log.

use super::{Agent, wedged, within};
use crate::protocol::{ErrorCode, Response};

impl Agent {
    /// Opens a link on the laptop.
    ///
    /// No prompt, by decision: `clipboard.read` already hands the server the
    /// contents of this laptop's clipboard without asking, and a confirmation
    /// per URL turns a device-code login into a two-machine dance. The log line
    /// is the audit trail, and it is written *before* the opener runs so a URL
    /// that hangs a browser is still recorded.
    ///
    /// The scheme was settled in `decode_request`; by here the URL is http or
    /// https and nothing else.
    pub(super) async fn open(&self, url: &str) -> (Response, Option<Vec<u8>>) {
        self.note(&format!("opening {url}"));
        let Some(opened) = within(self.opener.open(url)).await else {
            self.warn(&format!("the browser did not answer while opening {url}"));
            return (wedged("open the link"), None);
        };
        match opened {
            Ok(()) => (Response::Opened, None),
            Err(error) => {
                self.warn(&format!("could not open {url}: {error:#}"));
                (
                    Response::Error {
                        code: ErrorCode::Unavailable,
                        message: format!("this laptop could not open the link: {error}"),
                    },
                    None,
                )
            }
        }
    }

    /// Something the laptop is doing on the server's behalf.
    fn note(&self, message: &str) {
        record(message);
        if self.bar.enabled() {
            self.bar.flash(&spoken(message));
        } else {
            eprintln!("riabuild: {message}");
        }
    }

    /// Something that went wrong doing it.
    ///
    /// Passing rather than standing, like the sentence itself: a link this
    /// laptop's browser refused says nothing about the next one, and the shim
    /// on the server has already exited non-zero, which is what makes the
    /// program that asked print the URL for the developer to open by hand.
    fn warn(&self, message: &str) {
        record(message);
        if self.bar.enabled() {
            self.bar.flash_warning(&spoken(message));
        } else {
            eprintln!("riabuild: {message}");
        }
    }
}

/// The same sentence, addressed to a developer rather than to a log.
///
/// Named for the facility the banner already named — a developer who read
/// "Clipboard channel — connected" at the top of the run is the one reading
/// this — rather than for the half of it that opens links. Two names for one
/// channel is a worse cost on one line than a name that covers more than the
/// clipboard.
fn spoken(message: &str) -> String {
    format!("Clipboard channel — {message}")
}

/// The laptop's record of what the server asked it to do.
///
/// Only `browser.open` writes here. Clipboard traffic is high-volume and its
/// content is the developer's own, so logging it would be both noisy and a
/// place secrets accumulate; opening a link is rare, consequential, and the
/// operation the developer agreed to have happen without a prompt. That trade
/// is the reason there is no confirmation.
fn record(message: &str) {
    if let Ok(path) = std::env::var(crate::LOG_ENV) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "agent: {message}");
        }
    }
}

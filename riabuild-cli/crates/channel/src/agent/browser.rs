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
        note(&format!("opening {url}"));
        let Some(opened) = within(self.opener.open(url)).await else {
            note(&format!("the browser did not answer while opening {url}"));
            return (wedged("open the link"), None);
        };
        match opened {
            Ok(()) => (Response::Opened, None),
            Err(error) => {
                note(&format!("could not open {url}: {error:#}"));
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
}

/// The laptop's record of what the server asked it to do.
///
/// Only `browser.open` writes here. Clipboard traffic is high-volume and its
/// content is the developer's own, so logging it would be both noisy and a
/// place secrets accumulate; opening a link is rare, consequential, and the
/// operation the developer agreed to have happen without a prompt. That trade
/// is the reason there is no confirmation.
fn note(message: &str) {
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
    eprintln!("riabuild: {message}");
}

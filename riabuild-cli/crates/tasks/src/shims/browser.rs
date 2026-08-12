//! The `xdg-open` shim: a link on the server, a browser on the laptop.
//!
//! Reached two ways, because one is not enough. `gh auth login` and anything
//! else that opens a link finds this on `PATH`. Claude Code does not — it
//! decides whether to open anything *before* it resolves a command:
//!
//! ```js
//! let browser = attacherCaps()?.browser ?? env.BROWSER
//! if (!browser && isHeadlessLinux()) return { ok: false, reason: "no_display" }
//! return classify(await spawn(browser || "xdg-open", [url]))
//! ```
//!
//! On a headless server with `BROWSER` unset that returns `no_display` and
//! never execs anything, so `shell::environment` points `BROWSER` at this same
//! script. One implementation, two entry points.
//!
//! **This shim never falls through to the real `xdg-open`.** That is the one
//! place it diverges from the clipboard shim, which hands unhandled
//! invocations to the real binary. `xdg-open` on a headless box resolves
//! through `/etc/mailcap` to w3m or lynx, which then renders *inside the
//! session's own TTY* over the top of Claude Code. That is the failure this
//! whole feature exists to prevent, so a channel that cannot serve the request
//! fails instead of passing it on.

use riabuild_channel::client;
use riabuild_channel::protocol::{Request, Response, is_openable};
use std::path::PathBuf;

/// The URL out of an `xdg-open`/`$BROWSER` argv.
///
/// Both callers pass exactly one operand. Options are skipped rather than
/// rejected so that a caller adding a flag riabuild has never seen still gets
/// its link opened.
pub fn url_from(args: &[String]) -> Option<&String> {
    args.iter().find(|arg| !arg.starts_with('-'))
}

/// Runs the shim. Returns the exit code the real tool would have used.
///
/// Every failure is non-zero and silent on stdout. Claude Code *captures* the
/// browser command's stdout rather than letting it reach the terminal, so a
/// printed "open this yourself: <url>" would be swallowed and the developer
/// would see nothing at all. The exit code is the only signal that crosses:
/// Claude Code maps non-zero to `{ ok: false }` and its own caller surfaces the
/// URL. `gh`, whose stdout is the terminal, gets the same treatment because one
/// behaviour that works for both beats two that each work for one.
pub async fn run(args: &[String], socket: Option<PathBuf>) -> i32 {
    let Some(url) = url_from(args) else {
        log("no link was given to open");
        return 1;
    };

    // The laptop enforces this too, at decode time, and that copy is the one
    // that decides. This one exists so a refused link costs a message rather
    // than a round trip.
    if !is_openable(url) {
        log(&format!(
            "the channel opens http and https links only, and `{url}` is neither"
        ));
        return 1;
    }

    let Some(socket) = socket else {
        log("no laptop channel is configured for this session");
        return 1;
    };

    let request = Request::OpenUrl { url: url.clone() };
    match client::request(&socket, &request).await {
        Ok(reply) => match reply.response {
            Response::Opened => 0,
            Response::Error { code, message } => {
                log(&format!("{}: {message}", code.as_str()));
                1
            }
            other => {
                log(&format!("unexpected reply: {other:?}"));
                1
            }
        },
        Err(error) => {
            log(&format!("{error:#}"));
            1
        }
    }
}

/// Where a diagnosis survives.
///
/// stderr is best-effort — Claude Code discards it on some paths — so the
/// channel log is the copy that can be relied on.
fn log(message: &str) {
    if let Ok(path) = std::env::var(riabuild_channel::LOG_ENV) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "browser: {message}");
        }
    }
    eprintln!("riabuild: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn the_url_is_the_first_operand() {
        let argv = args(["https://example.com"].as_ref());
        assert_eq!(
            url_from(&argv).map(String::as_str),
            Some("https://example.com")
        );
    }

    /// A flag riabuild has never seen must not consume the link.
    #[test]
    fn options_are_skipped_rather_than_treated_as_the_link() {
        let argv = args(["--new-window", "https://example.com"].as_ref());
        assert_eq!(
            url_from(&argv).map(String::as_str),
            Some("https://example.com")
        );
    }

    #[test]
    fn an_argv_with_no_operand_has_no_url() {
        assert!(url_from(&args(["--help"].as_ref())).is_none());
        assert!(url_from(&[]).is_none());
    }

    #[tokio::test]
    async fn a_missing_channel_fails_rather_than_pretending_it_opened() {
        let code = run(&args(["https://example.com"].as_ref()), None).await;
        assert_eq!(code, 1);
    }

    /// The scheme rule, server-side. Whether the check runs before the socket
    /// lookup is not observable from here — a dead socket fails either way —
    /// and it does not need to be: `protocol::is_openable` is the rule, and the
    /// laptop's own `decode_request` is what enforces it. This pins that the
    /// shim consults it at all.
    #[tokio::test]
    async fn a_link_the_channel_does_not_carry_is_refused_locally() {
        for url in ["file:///etc/passwd", "vscode://x", "javascript:alert(1)"] {
            let code = run(
                &args([url].as_ref()),
                Some(PathBuf::from("/nonexistent.sock")),
            )
            .await;
            assert_eq!(code, 1, "{url}");
        }
    }

    #[tokio::test]
    async fn an_empty_argv_fails() {
        assert_eq!(run(&[], None).await, 1);
    }
}

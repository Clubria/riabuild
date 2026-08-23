//! Whether to open a browser at all, which one, and what to print instead.
//!
//! The environment arrives as a *parameter* rather than being read here, so
//! the decision is testable without rewriting the process environment
//! underneath a test — and so is the opener each platform uses.

use riabuild_runner::{CommandRunner, RunOptions};

use super::reply::DeviceStart;

/// What riabuild has to know to decide whether opening a browser is worth it.
///
/// Passed in rather than read here so the decision is testable without
/// rewriting the process environment underneath a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserEnv {
    pub over_ssh: bool,
    pub macos: bool,
    pub has_display: bool,
}

/// Whether to try to open a browser at all.
///
/// Over SSH the answer is always no: the terminal is on a server and the
/// browser that matters is on the laptop in front of the developer, so spawning
/// anything here at best opens a window nobody is looking at.
pub fn browser_available(env: BrowserEnv) -> bool {
    if env.over_ssh {
        return false;
    }
    env.macos || env.has_display
}

pub(super) fn current_browser_env() -> BrowserEnv {
    let set = |key: &str| std::env::var(key).is_ok_and(|value| !value.is_empty());
    BrowserEnv {
        over_ssh: set("SSH_CONNECTION") || set("SSH_TTY") || set("SSH_CLIENT"),
        macos: cfg!(target_os = "macos"),
        has_display: set("DISPLAY") || set("WAYLAND_DISPLAY"),
    }
}

/// A label for this machine, from its hostname. `pub`: `tasks::login` calls
/// this for a laptop's own login; `remote::session::ensure` passes the
/// server's hostname instead, so the dashboard lists each session correctly.
pub async fn device_label(runner: &dyn CommandRunner) -> String {
    let hostname = runner
        .run("hostname", &[], &RunOptions::default())
        .await
        .ok()
        .filter(|output| output.ok())
        .map(|output| output.trimmed().to_string())
        .filter(|name| !name.is_empty());
    hostname.unwrap_or_else(|| "this machine".to_string())
}

/// Opens the developer's browser, and says whether it worked.
///
/// `is_macos` is a parameter for the reason `current_browser_env` twenty lines
/// above already takes the same fact as one: `riabuild-api` is not on the list
/// of crates allowed to know which platform it is running on, and a `cfg!`
/// here compiles every branch but one out of the test binary — so `open`
/// versus `xdg-open` could only ever be asserted on the host that happened to
/// be running the suite, which for this repository is a Mac on `e2e.yml` and
/// Linux everywhere a unit test runs.
pub(super) async fn open_browser(is_macos: bool, runner: &dyn CommandRunner, url: &str) -> bool {
    let opener = if is_macos { "open" } else { "xdg-open" };
    runner
        .run(opener, &[url], &RunOptions::default())
        .await
        .map(|output| output.ok())
        .unwrap_or(false)
}

/// The one link a developer is given, whether they click it or copy it.
///
/// `verificationUriComplete` when the server sent one: the same `/cli` page
/// with `?code=` on it, which fills the box in on arrival. The browser opened
/// locally already used that link; printing something *else* left the
/// developer signing in over SSH — where nothing is opened for them and the
/// link is carried to a browser on another machine by hand — as the only one
/// who had to copy the code separately, which is exactly backwards.
///
/// Prefilling is not approving. The page fills the field and stops: it still
/// names the machine asking and still waits for a click, because a URL that
/// approved on sight would sign in whoever got a developer to follow it. That
/// is why the code keeps a line of its own — it is what the developer checks
/// the browser against, not merely something to type.
///
/// Falls back to the bare URI rather than appending `?code=` here. What the
/// dashboard reads out of a query string is the dashboard's to decide, and a
/// riabuild-web that offers no complete link is one this side has no business
/// guessing the shape of; the bare link and the code line still sign the
/// machine in.
pub(super) fn verification_link(start: &DeviceStart) -> &str {
    start
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&start.verification_uri)
}
#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    #[test]
    fn the_link_a_developer_is_shown_carries_the_code() {
        // The whole point: a developer who copies this line out of an SSH
        // session lands on a page with the box already filled, instead of
        // going back to the terminal for the code as a second copy-paste.
        let start: DeviceStart = serde_json::from_str(
            r#"{"deviceCode":"dc_1","userCode":"WXZB-CDFG",
                "verificationUri":"https://riabuild.clubria.com/cli",
                "verificationUriComplete":"https://riabuild.clubria.com/cli?code=WXZB-CDFG"}"#,
        )
        .unwrap();
        assert_eq!(
            verification_link(&start),
            "https://riabuild.clubria.com/cli?code=WXZB-CDFG"
        );
    }

    #[test]
    fn a_dashboard_that_offers_no_prefill_still_gets_a_link_printed() {
        // Losing the prefill costs a developer one typed code. Printing no
        // link, or one this side invented a query string for, costs them the
        // login.
        let start: DeviceStart = serde_json::from_str(
            r#"{"deviceCode":"dc_1","userCode":"WXZB-CDFG",
                "verificationUri":"https://riabuild.clubria.com/cli"}"#,
        )
        .unwrap();
        assert_eq!(
            verification_link(&start),
            "https://riabuild.clubria.com/cli"
        );
    }

    #[test]
    fn ssh_never_opens_a_browser() {
        // The whole reason this flow exists: over SSH the browser that matters
        // is on the laptop, and anything opened here is on the wrong machine.
        for env in [
            BrowserEnv {
                over_ssh: true,
                macos: true,
                has_display: true,
            },
            BrowserEnv {
                over_ssh: true,
                macos: false,
                has_display: false,
            },
        ] {
            assert!(!browser_available(env), "{env:?}");
        }
    }

    #[test]
    fn a_desktop_session_still_gets_its_browser_opened() {
        assert!(browser_available(BrowserEnv {
            over_ssh: false,
            macos: true,
            has_display: false,
        }));
        assert!(browser_available(BrowserEnv {
            over_ssh: false,
            macos: false,
            has_display: true,
        }));
    }

    #[test]
    fn a_linux_box_with_no_display_is_not_offered_a_browser() {
        // A headless server someone is sitting at physically, or a container.
        assert!(!browser_available(BrowserEnv {
            over_ssh: false,
            macos: false,
            has_display: false,
        }));
    }

    /// Both branches, on whichever host is running the suite.
    ///
    /// This is the assertion `cfg!(target_os = "macos")` made impossible: with
    /// it inline, one of these two arms is compiled out of the test binary, so
    /// a Linux runner could only ever pin `xdg-open` and the `open` branch
    /// shipped covered by nothing. `ci.yml` does run the suite on macOS, which
    /// would have covered the other arm and neither at once.
    #[tokio::test]
    async fn the_opener_is_the_one_the_platform_uses_and_both_are_asserted() {
        for (is_macos, expected) in [(true, "open"), (false, "xdg-open")] {
            let runner = FakeRunner::new().with(
                &format!("{expected} https://riabuild.clubria.com/cli"),
                0,
                "",
                "",
            );
            assert!(
                open_browser(is_macos, &runner, "https://riabuild.clubria.com/cli").await,
                "is_macos={is_macos}"
            );
            assert_eq!(
                runner.calls(),
                vec![format!("{expected} https://riabuild.clubria.com/cli")],
            );
        }
    }

    /// A browser that will not open is a note, never a failure — the link and
    /// the code are already on screen and the developer can carry them.
    #[tokio::test]
    async fn a_browser_that_refuses_to_open_is_reported_as_such() {
        let runner = FakeRunner::new().with("xdg-open https://example.test", 1, "", "no display");
        assert!(!open_browser(false, &runner, "https://example.test").await);
    }
}

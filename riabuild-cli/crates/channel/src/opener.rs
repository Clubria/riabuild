//! Opening a URL in the laptop's own browser.
//!
//! The mirror of `clipboard`: a trait so the agent can be tested without a
//! browser anywhere, and a `CommandRunner` underneath so a scripted `open` is
//! indistinguishable from a real one.
//!
//! Nothing here validates the URL. `protocol::is_openable` has already refused
//! everything but http and https, and it did so before the `Request` existed —
//! a second check here would suggest the first one is optional.

use anyhow::{Result, bail};
use async_trait::async_trait;
use riabuild_runner::{CommandRunner, RunOptions};
use std::path::Path;
use std::sync::Arc;

#[async_trait]
pub trait Opener: Send + Sync {
    /// Hands `url` to the laptop's browser.
    ///
    /// `Ok(())` means the opener accepted it, which is as much as any of these
    /// tools reports — neither `open` nor `xdg-open` waits to find out whether
    /// a page loaded.
    async fn open(&self, url: &str) -> Result<()>;
}

/// The command each platform uses. A parameter rather than a `cfg!` read, for
/// the reason `clipboard::detect` gives: `cfg!` compiles every branch but one
/// out of the test binary, so only the runner's own platform could be asserted.
pub fn command_for(os: &str) -> &'static str {
    match os {
        "macos" => "open",
        _ => "xdg-open",
    }
}

pub struct SystemOpener {
    runner: Arc<dyn CommandRunner>,
    command: &'static str,
    bin_dir: std::path::PathBuf,
}

impl SystemOpener {
    pub fn new(runner: Arc<dyn CommandRunner>, os: &str, bin_dir: &Path) -> Self {
        Self {
            runner,
            command: command_for(os),
            bin_dir: bin_dir.to_path_buf(),
        }
    }
}

#[async_trait]
impl Opener for SystemOpener {
    async fn open(&self, url: &str) -> Result<()> {
        // The same recursion the clipboard shim guards against, in the one
        // place it is easy to miss. A laptop that riabuild provisioned has its
        // own `~/.riabuild/bin/xdg-open`, and the agent is started from inside
        // the riabuild shell where that directory leads PATH. Without this the
        // laptop's opener finds riabuild's shim, which asks the channel to open
        // the URL, which arrives back here.
        let path = std::env::var("PATH").unwrap_or_default();
        let options = RunOptions {
            env: vec![(
                "PATH".into(),
                riabuild_paths::path_without(&path, &self.bin_dir),
            )],
            ..Default::default()
        };

        let output = self.runner.run(self.command, &[url], &options).await?;
        if !output.ok() {
            let detail = output.stderr.trim();
            let detail = if detail.is_empty() {
                match output.code {
                    Some(code) => format!("exit {code}"),
                    // Killed by a signal: no status to report, and saying
                    // "exit None" to a developer helps nobody.
                    None => "it was killed before it finished".to_string(),
                }
            } else {
                detail.to_string()
            };
            bail!("{} could not open the link: {detail}", self.command);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    #[test]
    fn each_platform_uses_its_own_opener() {
        assert_eq!(command_for("macos"), "open");
        assert_eq!(command_for("linux"), "xdg-open");
    }

    #[tokio::test]
    async fn a_url_reaches_the_platform_opener_untouched() {
        let fake =
            Arc::new(FakeRunner::new().with("open https://github.com/login/device", 0, "", ""));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let opener = SystemOpener::new(runner, "macos", Path::new("/Users/ada/.riabuild/bin"));

        opener
            .open("https://github.com/login/device")
            .await
            .unwrap();
        assert_eq!(fake.calls(), vec!["open https://github.com/login/device"]);
    }

    #[tokio::test]
    async fn an_opener_that_fails_says_what_it_printed() {
        let fake = Arc::new(FakeRunner::new().with(
            "xdg-open https://example.com",
            3,
            "",
            "no method available for opening",
        ));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let opener = SystemOpener::new(runner, "linux", Path::new("/home/ada/.riabuild/bin"));

        let error = opener.open("https://example.com").await.unwrap_err();
        assert!(error.to_string().contains("no method available"), "{error}");
    }
}

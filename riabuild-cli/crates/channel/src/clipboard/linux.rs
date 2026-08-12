//! The X11 and Wayland clipboards, read through `xclip` and `wl-paste`.

use super::Clipboard;
use crate::mime::{self, Vocabulary};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use riabuild_runner::{CommandRunner, RunOptions};
use std::sync::Arc;

/// The shell's code for "no such command".
///
/// This is the one exit status that is a genuine fault rather than an empty
/// clipboard: the tool riabuild was told to use is not installed, and every
/// read will fail the same way until someone installs it. Reported with the
/// tool's own stderr, because "paste does not work" is not actionable.
const NOT_FOUND: i32 = 127;

fn fault(tool: &str, stderr: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "`{tool}` is not installed on this laptop: {}",
        stderr.trim()
    )
}

pub struct X11Clipboard {
    runner: Arc<dyn CommandRunner>,
}

impl X11Clipboard {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl Clipboard for X11Clipboard {
    async fn targets(&self) -> Result<Vec<String>> {
        let output = self
            .runner
            .run(
                "xclip",
                &["-selection", "clipboard", "-t", "TARGETS", "-o"],
                &RunOptions::default(),
            )
            .await
            .context("could not ask xclip what is on the clipboard")?;

        if output.code == Some(NOT_FOUND) {
            bail!(fault("xclip", &output.stderr));
        }
        // Any other non-zero exit is an empty clipboard. That is not a fault.
        if !output.ok() {
            return Ok(Vec::new());
        }

        let atoms: Vec<String> = output.stdout.lines().map(|line| line.to_string()).collect();
        Ok(mime::normalise_targets(Vocabulary::X11, &atoms))
    }

    async fn read(&self, mime_type: &str) -> Result<Option<Vec<u8>>> {
        let Some(atom) = mime::from_mime(Vocabulary::X11, mime_type) else {
            return Ok(None);
        };

        let output = self
            .runner
            .run_bytes(
                "xclip",
                &["-selection", "clipboard", "-t", atom, "-o"],
                &RunOptions::default(),
            )
            .await
            .context("could not read the clipboard with xclip")?;

        if output.code == Some(NOT_FOUND) {
            bail!(fault("xclip", &output.stderr));
        }
        if !output.ok() || output.stdout.is_empty() {
            return Ok(None);
        }
        Ok(Some(output.stdout))
    }

    async fn write(&self, mime_type: &str, bytes: &[u8]) -> Result<bool> {
        let Some(atom) = mime::from_mime(Vocabulary::X11, mime_type) else {
            return Ok(false);
        };

        let code = self
            .runner
            .run_forking(
                "xclip",
                &["-selection", "clipboard", "-t", atom, "-i"],
                &RunOptions {
                    stdin: Some(bytes.to_vec()),
                    ..Default::default()
                },
            )
            .await
            .context("could not write the clipboard with xclip")?;

        if code != 0 {
            bail!(write_failed("xclip", code));
        }
        Ok(true)
    }
}

/// A write has no stderr to quote — the fork holds that pipe — so the exit
/// status is the whole diagnostic and the message has to carry it.
fn write_failed(tool: &str, code: i32) -> anyhow::Error {
    anyhow::anyhow!("`{tool}` could not take the laptop's clipboard (exit {code})")
}

pub struct WaylandClipboard {
    runner: Arc<dyn CommandRunner>,
}

impl WaylandClipboard {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl Clipboard for WaylandClipboard {
    async fn targets(&self) -> Result<Vec<String>> {
        let output = self
            .runner
            .run("wl-paste", &["-l"], &RunOptions::default())
            .await
            .context("could not ask wl-paste what is on the clipboard")?;

        if output.code == Some(NOT_FOUND) {
            bail!(fault("wl-paste", &output.stderr));
        }
        if !output.ok() {
            return Ok(Vec::new());
        }

        let types: Vec<String> = output.stdout.lines().map(|line| line.to_string()).collect();
        Ok(mime::normalise_targets(Vocabulary::Wayland, &types))
    }

    async fn read(&self, mime_type: &str) -> Result<Option<Vec<u8>>> {
        let Some(native) = mime::from_mime(Vocabulary::Wayland, mime_type) else {
            return Ok(None);
        };

        // `-n` matters for every type, not just images: without it wl-paste
        // appends a newline, which corrupts a PNG and silently changes a
        // string.
        let output = self
            .runner
            .run_bytes("wl-paste", &["-n", "-t", native], &RunOptions::default())
            .await
            .context("could not read the clipboard with wl-paste")?;

        if output.code == Some(NOT_FOUND) {
            bail!(fault("wl-paste", &output.stderr));
        }
        if !output.ok() || output.stdout.is_empty() {
            return Ok(None);
        }
        Ok(Some(output.stdout))
    }

    /// Writes go to `wl-copy`; only reads go to `wl-paste`. The pair is one
    /// package and one session, so a laptop that can paste can also copy.
    async fn write(&self, mime_type: &str, bytes: &[u8]) -> Result<bool> {
        let Some(native) = mime::from_mime(Vocabulary::Wayland, mime_type) else {
            return Ok(false);
        };

        let code = self
            .runner
            .run_forking(
                "wl-copy",
                &["--type", native],
                &RunOptions {
                    stdin: Some(bytes.to_vec()),
                    ..Default::default()
                },
            )
            .await
            .context("could not write the clipboard with wl-copy")?;

        if code != 0 {
            bail!(write_failed("wl-copy", code));
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime::{PNG, TEXT};
    use riabuild_runner::FakeRunner;

    fn arc(runner: FakeRunner) -> Arc<dyn CommandRunner> {
        Arc::new(runner)
    }

    #[tokio::test]
    async fn x11_targets_are_normalised_from_the_atom_list() {
        let runner = arc(FakeRunner::new().with(
            "xclip -selection clipboard -t TARGETS -o",
            0,
            "TARGETS\nTIMESTAMP\nimage/png\nimage/tiff\ntext/uri-list\n",
            "",
        ));
        let clipboard = X11Clipboard::new(runner);
        // TARGETS and TIMESTAMP are not content, TIFF is redundant beside PNG,
        // and a file reference never crosses.
        assert_eq!(clipboard.targets().await.unwrap(), vec![PNG]);
    }

    #[tokio::test]
    async fn x11_reads_bytes_for_the_requested_type() {
        let png = [0x89u8, b'P', b'N', b'G', 0xFF];
        let runner = arc(FakeRunner::new().with_bytes(
            "xclip -selection clipboard -t image/png -o",
            0,
            &png,
            "",
        ));
        let clipboard = X11Clipboard::new(runner);
        assert_eq!(clipboard.read(PNG).await.unwrap(), Some(png.to_vec()));
    }

    /// The channel speaks MIME; xclip speaks atoms. A read for canonical UTF-8
    /// text has to reach xclip as `UTF8_STRING` or it returns nothing.
    #[tokio::test]
    async fn a_text_read_is_translated_into_the_x11_atom() {
        let runner = Arc::new(FakeRunner::new().with_bytes(
            "xclip -selection clipboard -t UTF8_STRING -o",
            0,
            b"hello",
            "",
        ));
        let clipboard = X11Clipboard::new(runner.clone());
        assert_eq!(clipboard.read(TEXT).await.unwrap(), Some(b"hello".to_vec()));
        assert!(
            runner.calls().iter().any(|c| c.contains("UTF8_STRING")),
            "{:?}",
            runner.calls()
        );
    }

    /// An empty clipboard is not a fault. xclip exits non-zero with nothing on
    /// stdout, and that has to stay distinguishable from "the tool is missing"
    /// or the agent reports a broken channel every time nothing is copied.
    #[tokio::test]
    async fn an_empty_clipboard_reads_as_no_content_rather_than_an_error() {
        let runner = arc(FakeRunner::new().with(
            "xclip -selection clipboard -t image/png -o",
            1,
            "",
            "Error: target image/png not available",
        ));
        let clipboard = X11Clipboard::new(runner);
        assert_eq!(clipboard.read(PNG).await.unwrap(), None);
    }

    /// The one exit status that is a genuine fault. Left as an empty clipboard,
    /// a laptop with no xclip installed reports "nothing copied" forever and
    /// nobody ever finds out why paste does not work.
    #[tokio::test]
    async fn a_missing_tool_is_a_fault_rather_than_an_empty_clipboard() {
        let runner = arc(FakeRunner::new().with(
            "xclip -selection clipboard -t TARGETS -o",
            127,
            "",
            "xclip: command not found",
        ));
        let clipboard = X11Clipboard::new(runner);
        let error = clipboard.targets().await.expect_err("should be a fault");
        assert!(error.to_string().contains("not installed"), "{error}");
        assert!(error.to_string().contains("command not found"), "{error}");
    }

    #[tokio::test]
    async fn an_empty_target_list_is_an_empty_clipboard_not_an_error() {
        let runner = arc(FakeRunner::new().with(
            "xclip -selection clipboard -t TARGETS -o",
            1,
            "",
            "Error: target TARGETS not available",
        ));
        let clipboard = X11Clipboard::new(runner);
        assert!(clipboard.targets().await.unwrap().is_empty());
    }

    /// A type the channel does not carry is refused before a subprocess runs.
    #[tokio::test]
    async fn a_type_outside_the_table_is_never_shelled_out_for() {
        let runner = Arc::new(FakeRunner::new());
        let clipboard = X11Clipboard::new(runner.clone());
        assert_eq!(clipboard.read("application/pdf").await.unwrap(), None);
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    /// The bytes reach xclip on stdin, not in the argument list — the whole
    /// point of piping a write rather than passing it.
    #[tokio::test]
    async fn x11_writes_the_bytes_through_stdin() {
        let png = [0x89u8, b'P', b'N', b'G', 0xFF];
        let runner = Arc::new(FakeRunner::new().with(
            "xclip -selection clipboard -t image/png -i",
            0,
            "",
            "",
        ));
        let clipboard = X11Clipboard::new(runner.clone());

        assert!(clipboard.write(PNG, &png).await.unwrap());
        assert_eq!(runner.input_for("xclip"), Some(png.to_vec()));
    }

    /// The channel speaks MIME; xclip speaks atoms, on the way in as well as
    /// out. Written as `text/plain;charset=utf-8` the selection is one no X11
    /// application asks for.
    #[tokio::test]
    async fn a_text_write_is_translated_into_the_x11_atom() {
        let runner = Arc::new(FakeRunner::new().with(
            "xclip -selection clipboard -t UTF8_STRING -i",
            0,
            "",
            "",
        ));
        let clipboard = X11Clipboard::new(runner.clone());

        assert!(clipboard.write(TEXT, b"hello").await.unwrap());
        assert!(
            runner.calls().iter().any(|c| c.contains("UTF8_STRING")),
            "{:?}",
            runner.calls()
        );
    }

    /// A write must never land on PRIMARY, which changes on every mouse drag.
    #[tokio::test]
    async fn a_write_names_the_clipboard_selection_explicitly() {
        let runner = Arc::new(FakeRunner::new().with(
            "xclip -selection clipboard -t UTF8_STRING -i",
            0,
            "",
            "",
        ));
        let clipboard = X11Clipboard::new(runner.clone());
        clipboard.write(TEXT, b"hi").await.unwrap();

        let call = runner.calls().first().cloned().unwrap_or_default();
        assert!(call.contains("-selection clipboard"), "{call}");
    }

    /// A type outside the table is refused before a subprocess runs, the same
    /// way a read of one is.
    #[tokio::test]
    async fn a_write_of_an_uncarried_type_never_shells_out() {
        let runner = Arc::new(FakeRunner::new());
        let clipboard = X11Clipboard::new(runner.clone());
        assert!(!clipboard.write("application/pdf", b"%PDF").await.unwrap());
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    /// A write has no stderr to quote, so the exit status has to be in the
    /// message or the failure is unattributable.
    #[tokio::test]
    async fn a_failed_write_names_its_exit_status() {
        let runner =
            arc(FakeRunner::new().with("xclip -selection clipboard -t UTF8_STRING -i", 1, "", ""));
        let clipboard = X11Clipboard::new(runner);
        let error = clipboard
            .write(TEXT, b"hi")
            .await
            .expect_err("should be a fault");
        assert!(error.to_string().contains("exit 1"), "{error}");
    }

    /// Writes go to wl-copy; only reads go to wl-paste.
    #[tokio::test]
    async fn wayland_writes_through_wl_copy() {
        let runner = Arc::new(FakeRunner::new().with("wl-copy --type image/png", 0, "", ""));
        let clipboard = WaylandClipboard::new(runner.clone());

        assert!(clipboard.write(PNG, b"\x89PNG").await.unwrap());
        assert_eq!(runner.input_for("wl-copy"), Some(b"\x89PNG".to_vec()));
    }

    #[tokio::test]
    async fn wayland_targets_come_from_the_type_listing() {
        let runner = arc(FakeRunner::new().with(
            "wl-paste -l",
            0,
            "text/html\ntext/plain;charset=utf-8\ntext/plain\n",
            "",
        ));
        let clipboard = WaylandClipboard::new(runner);
        // Text leads, and the spellings of it collapse to one entry.
        assert_eq!(clipboard.targets().await.unwrap(), vec![TEXT, "text/html"]);
    }

    #[tokio::test]
    async fn wayland_reads_without_a_trailing_newline() {
        let runner =
            Arc::new(FakeRunner::new().with_bytes("wl-paste -n -t image/png", 0, b"\x89PNG", ""));
        let clipboard = WaylandClipboard::new(runner.clone());
        assert_eq!(
            clipboard.read(PNG).await.unwrap(),
            Some(b"\x89PNG".to_vec())
        );
        // Without -n, wl-paste appends a newline, which corrupts every image.
        assert!(
            runner.calls().iter().any(|c| c.contains("-n")),
            "{:?}",
            runner.calls()
        );
    }
}

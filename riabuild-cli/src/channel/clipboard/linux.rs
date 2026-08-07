//! The X11 and Wayland clipboards, read through `xclip` and `wl-paste`.

use super::Clipboard;
use crate::channel::mime::{self, Vocabulary};
use crate::runner::{CommandRunner, RunOptions};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;

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

        // xclip exits non-zero on an empty clipboard. That is not a fault.
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

        if !output.ok() || output.stdout.is_empty() {
            return Ok(None);
        }
        Ok(Some(output.stdout))
    }
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

        if !output.ok() || output.stdout.is_empty() {
            return Ok(None);
        }
        Ok(Some(output.stdout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::mime::{PNG, TEXT};
    use crate::runner::FakeRunner;

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

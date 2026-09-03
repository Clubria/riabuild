//! What Ctrl-V puts in the box.
//!
//! An agent reads *files*. A clipboard holds *bytes with no name*, and none of
//! the three harnesses is given a way to be handed anonymous bytes: a turn is
//! one detached child started with argv and no stdin anybody is left holding,
//! which is the same constraint that made `--input-format stream-json`
//! unusable in `riabuild-harness`. So a pasted image is **written to a file
//! and named in the prompt**, and the path is what the agent reads.
//!
//! That the path lands in the compose line rather than behind a `[image #1]`
//! placeholder is the whole design, not a shortcut. The developer sees exactly
//! what the agent will be given, editing the line is editing text, and there is
//! no second copy of "which attachment is this" to keep in step with a line
//! they can backspace through.
//!
//! # It works on a server because it does not know it is on one
//!
//! Nothing here names a platform or a tool. It asks a [`Clipboard`] what it
//! holds and reads one type, and `riabuild-channel` decides what that is:
//! `osascript` on a Mac, `xclip` or `wl-paste` on Linux — and on a server the
//! `xclip` on `PATH` is riabuild's own shim, so the clipboard being read is the
//! laptop's. The remote case needed no code; it needed this code to not care.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use riabuild_channel::clipboard::Clipboard;
use riabuild_channel::mime;

/// What one press of Ctrl-V found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pasted {
    /// An image, written here. The path is what goes in the prompt.
    Image(PathBuf),
    /// Text, already flattened to one line.
    Text(String),
    /// The clipboard holds nothing this window can use. Not a failure — an
    /// empty clipboard is the ordinary case and must never read as a broken
    /// channel.
    Nothing,
}

/// The image types this reads, in the order it prefers them.
///
/// PNG first because it is lossless and a tenth of the size; TIFF only where it
/// is the *only* image, which is what `mime::normalise_targets` already
/// guarantees by dropping it whenever PNG is present. A macOS screenshot is on
/// the pasteboard as both.
const IMAGES: &[(&str, &str)] = &[(mime::PNG, "png"), (mime::TIFF, "tiff")];

/// Reads the clipboard and says what to put in the box.
///
/// An image wins over text wherever both are present. That is a decision about
/// what this key is *for*: text is the thing a developer can also type, and
/// Ctrl-V exists here because an image is the thing they cannot.
pub async fn read(clipboard: &dyn Clipboard, images_dir: &Path) -> Result<Pasted> {
    let targets = clipboard
        .targets()
        .await
        .context("could not ask this machine what is on the clipboard")?;

    for (mime_type, extension) in IMAGES {
        if !targets.iter().any(|held| held == mime_type) {
            continue;
        }
        // A type that was advertised and then read as nothing is a clipboard
        // that changed under us, not a fault. Fall through to the next.
        let Some(bytes) = clipboard.read(mime_type).await? else {
            continue;
        };
        let path = write_image(images_dir, extension, &bytes).await?;
        return Ok(Pasted::Image(path));
    }

    if targets.iter().any(|held| held == mime::TEXT)
        && let Some(bytes) = clipboard.read(mime::TEXT).await?
    {
        let text = flatten(&String::from_utf8_lossy(&bytes));
        if !text.is_empty() {
            return Ok(Pasted::Text(text));
        }
    }

    Ok(Pasted::Nothing)
}

/// Writes the bytes where the compose line can name them.
///
/// Under riabuild's own tree and never in the checkout: an image dropped into
/// the repository would arrive in the developer's `git status` and, sooner or
/// later, in a commit.
async fn write_image(dir: &Path, extension: &str, bytes: &[u8]) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("could not create {}", dir.display()))?;
    let path = dir.join(format!("{}.{extension}", crate::store::stamped_name()));
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("could not write the pasted image to {}", path.display()))?;
    Ok(path)
}

/// Clipboard text as one line.
///
/// The box is a single-line editor, so a newline has no spelling in it — and
/// the alternative to collapsing them is a `String` holding characters the
/// caret arithmetic in `compose` cannot land on. Runs of whitespace collapse
/// with them, because a pasted stack trace is mostly indentation and a line of
/// it padded out to the terminal's width is unreadable.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_channel::clipboard::{CliClipboard, MacOsClipboard};
    use riabuild_runner::{CommandRunner, FakeRunner};
    use std::sync::Arc;

    fn arc(runner: FakeRunner) -> Arc<dyn CommandRunner> {
        Arc::new(runner)
    }

    /// Five bytes that are recognisably a PNG and nothing else.
    const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D];

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A Linux laptop, and — byte for byte the same path — a server, where the
    /// `xclip` being run is riabuild's own shim and the clipboard on the far
    /// end of it is the developer's Mac. That the two are one test is the
    /// claim: nothing in this module can tell them apart.
    #[tokio::test]
    async fn an_image_on_an_x11_clipboard_is_written_and_named() {
        let dir = temp();
        let runner = arc(FakeRunner::new()
            .with(
                "xclip -selection clipboard -t TARGETS -o",
                0,
                "TARGETS\nimage/png\n",
                "",
            )
            .with_bytes(
                "xclip -selection clipboard -t image/png -o",
                0,
                PNG_BYTES,
                "",
            ));
        let clipboard = CliClipboard::x11(runner);

        let Pasted::Image(path) = read(&clipboard, dir.path()).await.unwrap() else {
            panic!("an image on the clipboard should paste as one");
        };
        assert_eq!(path.extension().unwrap(), "png");
        assert_eq!(tokio::fs::read(&path).await.unwrap(), PNG_BYTES);
        // Under the directory it was handed, so the caller decides where images
        // live and this decides nothing.
        assert!(path.starts_with(dir.path()), "{}", path.display());
    }

    /// The other laptop. `osascript` rather than `xclip`, a hex envelope rather
    /// than raw bytes, and the same three lines of this module.
    #[tokio::test]
    async fn an_image_on_a_macos_pasteboard_is_written_and_named() {
        let dir = temp();
        let hex: String = PNG_BYTES.iter().map(|byte| format!("{byte:02X}")).collect();
        let runner = arc(FakeRunner::new()
            .with("osascript -e clipboard info", 0, "«class PNGf», 5", "")
            .with(
                "osascript -e the clipboard as «class PNGf»",
                0,
                &format!("«data PNGf{hex}»\n"),
                "",
            ));
        let clipboard = MacOsClipboard::new(runner);

        let Pasted::Image(path) = read(&clipboard, dir.path()).await.unwrap() else {
            panic!("a pasteboard image should paste as one");
        };
        assert_eq!(tokio::fs::read(&path).await.unwrap(), PNG_BYTES);
    }

    /// macOS puts a screenshot on the pasteboard as PNG *and* as uncompressed
    /// TIFF, which for the same pixels is ten times the size — over the
    /// channel, ten times the bytes to carry back from the laptop.
    #[tokio::test]
    async fn png_is_preferred_and_tiff_is_never_read_beside_it() {
        let dir = temp();
        let runner = arc(FakeRunner::new()
            .with(
                "xclip -selection clipboard -t TARGETS -o",
                0,
                "image/tiff\nimage/png\n",
                "",
            )
            .with_bytes(
                "xclip -selection clipboard -t image/png -o",
                0,
                PNG_BYTES,
                "",
            ));
        // No `image/tiff` stub anywhere: reading it would be an unscripted
        // call, so this fails rather than quietly costing ten times the bytes.
        let clipboard = CliClipboard::x11(runner);

        let Pasted::Image(path) = read(&clipboard, dir.path()).await.unwrap() else {
            panic!("png should win");
        };
        assert_eq!(path.extension().unwrap(), "png");
    }

    #[tokio::test]
    async fn tiff_is_read_when_it_is_the_only_image() {
        let dir = temp();
        let runner = arc(FakeRunner::new()
            .with(
                "xclip -selection clipboard -t TARGETS -o",
                0,
                "image/tiff\n",
                "",
            )
            .with_bytes(
                "xclip -selection clipboard -t image/tiff -o",
                0,
                &[0x4D, 0x4D, 0x00, 0x2A],
                "",
            ));
        let clipboard = CliClipboard::x11(runner);

        let Pasted::Image(path) = read(&clipboard, dir.path()).await.unwrap() else {
            panic!("tiff on its own is the image");
        };
        assert_eq!(path.extension().unwrap(), "tiff");
    }

    /// An image beats text wherever both are on the clipboard. Copying a
    /// picture out of a browser leaves a caption or a URL beside it, and the
    /// picture is what the developer pressed this key for.
    #[tokio::test]
    async fn an_image_wins_over_text_that_came_with_it() {
        let dir = temp();
        let runner = arc(FakeRunner::new()
            .with(
                "xclip -selection clipboard -t TARGETS -o",
                0,
                "UTF8_STRING\nimage/png\n",
                "",
            )
            .with_bytes(
                "xclip -selection clipboard -t image/png -o",
                0,
                PNG_BYTES,
                "",
            ));
        let clipboard = CliClipboard::x11(runner);
        assert!(matches!(
            read(&clipboard, dir.path()).await.unwrap(),
            Pasted::Image(_)
        ));
    }

    /// Ctrl-V reaching this window at all means the terminal did not paste it
    /// — Ctrl-Shift-V and Cmd-V are the terminal's own bindings — so text has
    /// to be handled here or the key does nothing on the commonest clipboard
    /// there is.
    #[tokio::test]
    async fn text_is_pasted_as_one_line() {
        let dir = temp();
        let runner = arc(FakeRunner::new()
            .with(
                "xclip -selection clipboard -t TARGETS -o",
                0,
                "UTF8_STRING\n",
                "",
            )
            .with_bytes(
                "xclip -selection clipboard -t UTF8_STRING -o",
                0,
                b"thread 'main' panicked at\n    src/lib.rs:12\n",
                "",
            ));
        let clipboard = CliClipboard::x11(runner);
        assert_eq!(
            read(&clipboard, dir.path()).await.unwrap(),
            Pasted::Text("thread 'main' panicked at src/lib.rs:12".into())
        );
        // and nothing was written: text is not an attachment
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        assert!(entries.next_entry().await.unwrap().is_none());
    }

    /// An empty clipboard is the ordinary case, and reporting it as a fault is
    /// the bug `clipboard::read` separates `Ok(None)` from `Err` to prevent.
    #[tokio::test]
    async fn an_empty_clipboard_is_nothing_rather_than_an_error() {
        let dir = temp();
        let runner =
            arc(FakeRunner::new().with("xclip -selection clipboard -t TARGETS -o", 1, "", ""));
        let clipboard = CliClipboard::x11(runner);
        assert_eq!(read(&clipboard, dir.path()).await.unwrap(), Pasted::Nothing);
    }

    /// A clipboard holding only a copied *file* pastes nothing. The type is
    /// dropped by `mime::normalise_targets` before it is ever advertised —
    /// asserted from this end too, because a laptop path carried to a server is
    /// the one payload that is syntactically valid and semantically false, and
    /// exactly the sort of thing an agent will confidently try to read.
    #[tokio::test]
    async fn a_copied_file_reference_pastes_nothing() {
        let dir = temp();
        let runner = arc(FakeRunner::new().with(
            "xclip -selection clipboard -t TARGETS -o",
            0,
            "text/uri-list\nFILE_NAME\n",
            "",
        ));
        let clipboard = CliClipboard::x11(runner);
        assert_eq!(read(&clipboard, dir.path()).await.unwrap(), Pasted::Nothing);
    }

    /// The tool is not installed, which is the one non-zero exit that is a real
    /// fault: every read will fail the same way until somebody installs it, and
    /// "nothing was copied" is not something a developer can act on.
    #[tokio::test]
    async fn a_missing_clipboard_tool_is_reported_rather_than_read_as_empty() {
        let dir = temp();
        let runner = arc(FakeRunner::new().with(
            "xclip -selection clipboard -t TARGETS -o",
            127,
            "",
            "xclip: not found",
        ));
        let clipboard = CliClipboard::x11(runner);
        let error = read(&clipboard, dir.path()).await.unwrap_err();
        assert!(format!("{error:#}").contains("xclip"), "{error:#}");
    }

    /// Two pastes in the same second are two files. The name carries the clock
    /// *and* chance for the reason a session id does.
    #[tokio::test]
    async fn two_pastes_do_not_overwrite_each_other() {
        let dir = temp();
        let runner = || {
            arc(FakeRunner::new()
                .with(
                    "xclip -selection clipboard -t TARGETS -o",
                    0,
                    "image/png\n",
                    "",
                )
                .with_bytes(
                    "xclip -selection clipboard -t image/png -o",
                    0,
                    PNG_BYTES,
                    "",
                ))
        };
        let first = read(&CliClipboard::x11(runner()), dir.path())
            .await
            .unwrap();
        let second = read(&CliClipboard::x11(runner()), dir.path())
            .await
            .unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn flattening_collapses_every_run_of_whitespace() {
        assert_eq!(flatten("  a\n\n\tb  c "), "a b c");
        assert_eq!(flatten("\n\n"), "");
    }
}

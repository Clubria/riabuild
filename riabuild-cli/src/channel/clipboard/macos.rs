//! The macOS pasteboard, read through AppleScript.
//!
//! `pbpaste` cannot enumerate pasteboard types and cannot emit binary for an
//! arbitrary one, so it is not sufficient on its own. AppleScript can do both:
//! `clipboard info` lists `{«class PNGf», 184320}` pairs, and `the clipboard as
//! «class PNGf»` returns `«data PNGf89504E47…»`, a hex envelope that decodes to
//! the original bytes and travels safely as text through `CommandRunner`.
//!
//! Text still goes through `pbpaste`, which is exact. AppleScript's string
//! handling rewrites line endings and mangles anything non-ASCII.

use super::Clipboard;
use crate::channel::mime::{self, Vocabulary};
use crate::runner::{CommandRunner, RunOptions};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;

pub struct MacOsClipboard {
    runner: Arc<dyn CommandRunner>,
}

impl MacOsClipboard {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    async fn osascript(&self, script: &str) -> Result<Option<String>> {
        let output = self
            .runner
            .run("osascript", &["-e", script], &RunOptions::default())
            .await
            .context("could not read the pasteboard with osascript")?;
        Ok(output.ok().then(|| output.stdout.clone()))
    }
}

/// MIME → the four-character pasteboard class AppleScript names it by.
fn class_for(mime_type: &str) -> Option<&'static str> {
    match mime::from_mime(Vocabulary::MacOs, mime_type) {
        Some("public.png") => Some("PNGf"),
        Some("public.tiff") => Some("TIFF"),
        Some("public.html") => Some("HTML"),
        Some("public.utf8-plain-text") => Some("utf8"),
        _ => None,
    }
}

/// Class → the UTI the MIME table knows, for reading `clipboard info` back.
fn uti_for_class(class: &str) -> Option<&'static str> {
    match class {
        "PNGf" => Some("public.png"),
        "TIFF" => Some("public.tiff"),
        "HTML" => Some("public.html"),
        "utf8" | "ut16" | "TEXT" => Some("public.utf8-plain-text"),
        _ => None,
    }
}

/// `«data PNGf89504E47»` → the bytes.
///
/// The class tag is checked rather than skipped: AppleScript answers a request
/// for a class the pasteboard lacks by coercing to something else, and decoding
/// that as if it were the requested type produces a corrupt file rather than a
/// clean miss.
fn decode_applescript_data(raw: &str, class: &str) -> Option<Vec<u8>> {
    let body = raw.trim().strip_prefix("«data ")?.strip_suffix('»')?;
    let hex = body.strip_prefix(class)?;
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The bytes → `«data PNGf89504E47»`, the literal AppleScript reads back.
///
/// The exact inverse of `decode_applescript_data`, and it keeps writes off the
/// filesystem: the alternative is a temp file for `read (POSIX file …)`, which
/// would put a copy of every image the developer pastes into `/tmp`.
fn encode_applescript_data(class: &str, bytes: &[u8]) -> String {
    let mut literal = String::with_capacity(bytes.len() * 2 + class.len() + 8);
    literal.push_str("«data ");
    literal.push_str(class);
    for byte in bytes {
        literal.push_str(&format!("{byte:02X}"));
    }
    literal.push('»');
    literal
}

#[async_trait]
impl Clipboard for MacOsClipboard {
    async fn targets(&self) -> Result<Vec<String>> {
        let Some(info) = self.osascript("clipboard info").await? else {
            // An empty pasteboard makes osascript exit non-zero. Not a fault.
            return Ok(Vec::new());
        };

        // `«class PNGf», 184320, «class utf8», 11` — take the class tokens and
        // ignore the sizes.
        let utis: Vec<String> = info
            .split("«class ")
            .skip(1)
            .filter_map(|chunk| chunk.split('»').next())
            .filter_map(|class| uti_for_class(class.trim()))
            .map(|uti| uti.to_string())
            .collect();

        Ok(mime::normalise_targets(Vocabulary::MacOs, &utis))
    }

    async fn read(&self, mime_type: &str) -> Result<Option<Vec<u8>>> {
        if mime_type.eq_ignore_ascii_case(mime::TEXT) {
            let output = self
                .runner
                .run_bytes("pbpaste", &[], &RunOptions::default())
                .await
                .context("could not read the pasteboard with pbpaste")?;
            if !output.ok() || output.stdout.is_empty() {
                return Ok(None);
            }
            return Ok(Some(output.stdout));
        }

        let Some(class) = class_for(mime_type) else {
            return Ok(None);
        };

        let script = format!("the clipboard as «class {class}»");
        let Some(raw) = self.osascript(&script).await? else {
            return Ok(None);
        };
        Ok(decode_applescript_data(&raw, class))
    }

    async fn write(&self, mime_type: &str, bytes: &[u8]) -> Result<bool> {
        // pbcopy is exact, and it is the only way to put text on the pasteboard
        // without AppleScript's string handling rewriting line endings.
        if mime_type.eq_ignore_ascii_case(mime::TEXT) {
            let output = self
                .runner
                .run(
                    "pbcopy",
                    &[],
                    &RunOptions {
                        stdin: Some(bytes.to_vec()),
                        ..Default::default()
                    },
                )
                .await
                .context("could not write the pasteboard with pbcopy")?;
            if !output.ok() {
                anyhow::bail!("`pbcopy` failed: {}", output.stderr.trim());
            }
            return Ok(true);
        }

        let Some(class) = class_for(mime_type) else {
            return Ok(false);
        };

        // The script goes in on stdin rather than after `-e`. A 12 MB
        // screenshot is 24 MB of hex, which is well past what an argument list
        // can hold, and `osascript -` has no such limit.
        let script = format!(
            "set the clipboard to {}",
            encode_applescript_data(class, bytes)
        );
        let output = self
            .runner
            .run(
                "osascript",
                &["-"],
                &RunOptions {
                    stdin: Some(script.into_bytes()),
                    ..Default::default()
                },
            )
            .await
            .context("could not write the pasteboard with osascript")?;

        if !output.ok() {
            anyhow::bail!(
                "osascript could not set the pasteboard: {}",
                output.stderr.trim()
            );
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::mime::{PNG, TEXT};
    use crate::runner::FakeRunner;

    const CLIPBOARD_INFO: &str = "osascript -e clipboard info";

    fn arc(runner: FakeRunner) -> Arc<dyn CommandRunner> {
        Arc::new(runner)
    }

    #[tokio::test]
    async fn macos_targets_come_from_the_pasteboard_class_list() {
        let runner = arc(FakeRunner::new().with(
            CLIPBOARD_INFO,
            0,
            "«class PNGf», 184320, «class TIFF», 4194304, «class utf8», 11",
            "",
        ));
        let clipboard = MacOsClipboard::new(runner);
        // Text leads; the TIFF beside a PNG is the 4 MB redundancy the filter
        // exists to drop.
        assert_eq!(clipboard.targets().await.unwrap(), vec![TEXT, PNG]);
    }

    #[tokio::test]
    async fn macos_reads_an_image_as_hex_and_decodes_it() {
        let runner = arc(FakeRunner::new()
            .with(CLIPBOARD_INFO, 0, "«class PNGf», 4", "")
            .with(
                "osascript -e the clipboard as «class PNGf»",
                0,
                "«data PNGf89504E47»\n",
                "",
            ));
        let clipboard = MacOsClipboard::new(runner);
        assert_eq!(
            clipboard.read(PNG).await.unwrap(),
            Some(vec![0x89, 0x50, 0x4E, 0x47])
        );
    }

    /// pbpaste is exact for text and avoids AppleScript's string handling,
    /// which rewrites line endings and mangles anything non-ASCII.
    #[tokio::test]
    async fn macos_reads_text_through_pbpaste() {
        let runner = Arc::new(FakeRunner::new().with_bytes("pbpaste", 0, "héllo".as_bytes(), ""));
        let clipboard = MacOsClipboard::new(runner.clone());
        assert_eq!(
            clipboard.read(TEXT).await.unwrap(),
            Some("héllo".as_bytes().to_vec())
        );
        assert!(runner.calls().iter().any(|c| c.starts_with("pbpaste")));
    }

    #[tokio::test]
    async fn an_empty_macos_pasteboard_has_no_targets() {
        let runner = arc(FakeRunner::new().with(CLIPBOARD_INFO, 1, "", "execution error"));
        let clipboard = MacOsClipboard::new(runner);
        assert!(clipboard.targets().await.unwrap().is_empty());
    }

    #[test]
    fn the_applescript_hex_envelope_is_decoded_and_its_class_tag_stripped() {
        assert_eq!(
            decode_applescript_data("«data PNGf89504E47»", "PNGf"),
            Some(vec![0x89, 0x50, 0x4E, 0x47])
        );
        // Real osascript output is wrapped and newline-terminated.
        assert_eq!(
            decode_applescript_data("  «data utf8686921»  \n", "utf8"),
            Some(b"hi!".to_vec())
        );
    }

    /// AppleScript answers a request for a class the pasteboard lacks by
    /// coercing to something else. Decoding that as the requested type would
    /// produce a corrupt file rather than a clean miss.
    #[test]
    fn a_reply_that_is_not_a_data_envelope_decodes_to_nothing() {
        for raw in ["", "missing value", "«data PNGf8950", "«data TIFF89504E47»"] {
            assert_eq!(decode_applescript_data(raw, "PNGf"), None, "{raw:?}");
        }
    }

    /// pbcopy is exact. AppleScript's string handling rewrites line endings and
    /// mangles anything non-ASCII, which for a write means silently corrupting
    /// what the developer copied.
    #[tokio::test]
    async fn macos_writes_text_through_pbcopy() {
        let runner = Arc::new(FakeRunner::new().with("pbcopy", 0, "", ""));
        let clipboard = MacOsClipboard::new(runner.clone());

        assert!(clipboard.write(TEXT, "héllo".as_bytes()).await.unwrap());
        assert_eq!(
            runner.input_for("pbcopy"),
            Some("héllo".as_bytes().to_vec())
        );
    }

    /// The script goes in on stdin, not after `-e`: a screenshot's worth of hex
    /// is far past what an argument list can hold.
    #[tokio::test]
    async fn macos_writes_an_image_as_a_data_literal_on_stdin() {
        let runner = Arc::new(FakeRunner::new().with("osascript -", 0, "", ""));
        let clipboard = MacOsClipboard::new(runner.clone());

        assert!(
            clipboard
                .write(PNG, &[0x89, 0x50, 0x4E, 0x47])
                .await
                .unwrap()
        );

        let script = runner.input_for("osascript").expect("script on stdin");
        let script = String::from_utf8(script).expect("utf-8 script");
        assert_eq!(script, "set the clipboard to «data PNGf89504E47»");
    }

    #[tokio::test]
    async fn a_macos_write_of_an_uncarried_type_never_shells_out() {
        let runner = Arc::new(FakeRunner::new());
        let clipboard = MacOsClipboard::new(runner.clone());
        assert!(!clipboard.write("application/pdf", b"%PDF").await.unwrap());
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    /// The envelope has to be the exact inverse of the one `read` decodes, or
    /// a round trip through the channel corrupts what it carried.
    #[test]
    fn the_data_envelope_round_trips_through_its_own_decoder() {
        let bytes = vec![0x00, 0x89, 0x50, 0xFF, 0x0A];
        let literal = encode_applescript_data("PNGf", &bytes);
        assert_eq!(decode_applescript_data(&literal, "PNGf"), Some(bytes));
    }

    #[test]
    fn every_carried_mime_type_has_a_pasteboard_class() {
        assert_eq!(class_for(PNG), Some("PNGf"));
        assert_eq!(class_for(TEXT), Some("utf8"));
        assert_eq!(class_for("application/pdf"), None);
    }

    /// Pins the AppleScript pasteboard format against a real macOS.
    ///
    /// Ignored by default: it needs a Mac with something on the pasteboard.
    /// Run with `cargo test -- --ignored` on one before changing this backend.
    #[tokio::test]
    #[ignore = "requires macOS with content on the pasteboard"]
    async fn macos_pasteboard_smoke() {
        use crate::runner::RealRunner;
        let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
        let clipboard = MacOsClipboard::new(runner);
        let targets = clipboard.targets().await.expect("clipboard info");
        assert!(!targets.is_empty(), "copy something first");
        for target in &targets {
            let bytes = clipboard.read(target).await.expect("read");
            assert!(bytes.is_some(), "{target} was advertised but read nothing");
        }
    }

    /// Pins the write half against a real pasteboard.
    ///
    /// Ignored by default, and separate from the read smoke test because it is
    /// destructive: it replaces the pasteboard, so it restores the text that was
    /// there before rather than leaving a developer's copy consumed by a test.
    ///
    /// This is the one path in the backend that cannot be proved by a scripted
    /// runner. `«data …»` on `osascript -`'s stdin either sets the pasteboard or
    /// it does not, and only a Mac can say which.
    #[tokio::test]
    #[ignore = "requires macOS; replaces and then restores the pasteboard"]
    async fn macos_pasteboard_write_smoke() {
        use crate::runner::RealRunner;
        let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
        let clipboard = MacOsClipboard::new(runner);

        let restore = clipboard.read(TEXT).await.expect("read the current text");

        clipboard
            .write(TEXT, "riabuild write smoke".as_bytes())
            .await
            .expect("write text");
        assert_eq!(
            clipboard.read(TEXT).await.expect("read back"),
            Some("riabuild write smoke".as_bytes().to_vec())
        );

        // A one-pixel PNG, to exercise the data-literal path rather than pbcopy.
        let png: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        clipboard.write(PNG, &png).await.expect("write png");
        let read_back = clipboard.read(PNG).await.expect("read the png back");
        assert_eq!(
            read_back,
            Some(png),
            "the pasteboard did not return the bytes it was given"
        );

        if let Some(text) = restore {
            clipboard.write(TEXT, &text).await.expect("restore");
        }
    }
}

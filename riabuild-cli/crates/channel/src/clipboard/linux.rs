//! The X11 and Wayland clipboards, read through `xclip` and `wl-paste`.
//!
//! **One backend, two invocation tables.** These were two structs of roughly
//! seventy structurally identical lines that differed in a program name and an
//! argument order and in nothing else — same three methods, same vocabulary
//! lookup, same "127 is a fault and every other non-zero exit is an empty
//! clipboard" rule, same context sentences with a different tool spliced into
//! them. Two copies of one behaviour is a fix applied to one of them, and the
//! shape made that invisible: a reviewer reading either half sees a complete
//! and correct backend.
//!
//! What actually varies is the argv. [`CliClipboard`] takes it as data — the
//! vocabulary the tool speaks, the command that lists what is on the clipboard,
//! and the two commands that have a type name in the middle of them — so
//! adding a third session (a `termux-clipboard`, a `wsl` bridge) is a table
//! rather than a file, and a change to how a fault is told from an empty
//! clipboard lands on every one of them at once.

use super::Clipboard;
use crate::clipboard::{NOT_FOUND, missing, write_failed};
use crate::mime::{self, Vocabulary};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use riabuild_runner::{CommandRunner, RunOptions};
use std::sync::Arc;

/// An invocation with no type name in it: the one that lists the clipboard.
struct Listing {
    program: &'static str,
    argv: &'static [&'static str],
}

/// An invocation whose argv carries a type name somewhere in the middle.
///
/// Split into `before` and `after` rather than a format string because the
/// pieces reach `CommandRunner` as separate arguments, and a tool's own
/// vocabulary — `UTF8_STRING`, `text/plain;charset=utf-8` — is exactly the sort
/// of value a shell would have to quote.
struct Typed {
    program: &'static str,
    before: &'static [&'static str],
    after: &'static [&'static str],
}

impl Typed {
    fn argv<'a>(&self, native: &'a str) -> Vec<&'a str> {
        let mut argv: Vec<&'a str> = Vec::with_capacity(self.before.len() + self.after.len() + 1);
        argv.extend_from_slice(self.before);
        argv.push(native);
        argv.extend_from_slice(self.after);
        argv
    }
}

/// A clipboard reached through a command-line tool.
pub struct CliClipboard {
    runner: Arc<dyn CommandRunner>,
    /// What this tool calls the types the channel carries — X11 atoms, or the
    /// MIME spellings Wayland uses.
    vocabulary: Vocabulary,
    /// How this tool is asked what is on the clipboard. Its `program` is also
    /// the name spliced into the context sentence, because "could not ask
    /// wl-paste what is on the clipboard" is the line a developer reads.
    listing: Listing,
    reading: Typed,
    /// Writes go to a *different program* under Wayland — `wl-copy`, not
    /// `wl-paste` — which is the one asymmetry between the two sessions and the
    /// reason this is a whole `Typed` rather than a flag on `reading`.
    writing: Typed,
}

impl CliClipboard {
    /// `xclip`, which does all three through one program and one selection.
    ///
    /// `-selection clipboard` on every call, never `PRIMARY`: PRIMARY changes
    /// on every mouse drag, so a paste would carry whatever the developer last
    /// happened to highlight.
    pub fn x11(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            vocabulary: Vocabulary::X11,
            listing: Listing {
                program: "xclip",
                argv: &["-selection", "clipboard", "-t", "TARGETS", "-o"],
            },
            reading: Typed {
                program: "xclip",
                before: &["-selection", "clipboard", "-t"],
                after: &["-o"],
            },
            writing: Typed {
                program: "xclip",
                before: &["-selection", "clipboard", "-t"],
                after: &["-i"],
            },
        }
    }

    /// `wl-paste` for reads and `wl-copy` for writes. The pair is one package
    /// and one session, so a laptop that can paste can also copy.
    ///
    /// `-n` matters for every type and not just for images: without it
    /// `wl-paste` appends a newline, which corrupts a PNG and silently changes
    /// a string.
    pub fn wayland(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            vocabulary: Vocabulary::Wayland,
            listing: Listing {
                program: "wl-paste",
                argv: &["-l"],
            },
            reading: Typed {
                program: "wl-paste",
                before: &["-n", "-t"],
                after: &[],
            },
            writing: Typed {
                program: "wl-copy",
                before: &["--type"],
                after: &[],
            },
        }
    }
}

#[async_trait]
impl Clipboard for CliClipboard {
    async fn targets(&self) -> Result<Vec<String>> {
        let program = self.listing.program;
        let output = self
            .runner
            .run(program, self.listing.argv, &RunOptions::default())
            .await
            .with_context(|| format!("could not ask {program} what is on the clipboard"))?;

        if output.code == Some(NOT_FOUND) {
            bail!(missing(program, &output.stderr));
        }
        // Any other non-zero exit is an empty clipboard. That is not a fault.
        if !output.ok() {
            return Ok(Vec::new());
        }

        let native: Vec<String> = output.stdout.lines().map(|line| line.to_string()).collect();
        Ok(mime::normalise_targets(self.vocabulary, &native))
    }

    async fn read(&self, mime_type: &str) -> Result<Option<Vec<u8>>> {
        let Some(native) = mime::from_mime(self.vocabulary, mime_type) else {
            return Ok(None);
        };

        let program = self.reading.program;
        let output = self
            .runner
            .run_bytes(program, &self.reading.argv(native), &RunOptions::default())
            .await
            .with_context(|| format!("could not read the clipboard with {program}"))?;

        if output.code == Some(NOT_FOUND) {
            bail!(missing(program, &output.stderr));
        }
        if !output.ok() || output.stdout.is_empty() {
            return Ok(None);
        }
        Ok(Some(output.stdout))
    }

    async fn write(&self, mime_type: &str, bytes: &[u8]) -> Result<bool> {
        let Some(native) = mime::from_mime(self.vocabulary, mime_type) else {
            return Ok(false);
        };

        let program = self.writing.program;
        let code = self
            .runner
            .run_forking(
                program,
                &self.writing.argv(native),
                &RunOptions {
                    stdin: Some(bytes.to_vec()),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("could not write the clipboard with {program}"))?;

        if code != 0 {
            bail!(write_failed(program, code));
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
        let clipboard = CliClipboard::x11(runner);
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
        let clipboard = CliClipboard::x11(runner);
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
        let clipboard = CliClipboard::x11(runner.clone());
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
        let clipboard = CliClipboard::x11(runner);
        assert_eq!(clipboard.read(PNG).await.unwrap(), None);
    }

    /// The one exit status that is a genuine fault. Left as an empty clipboard,
    /// a laptop with no xclip installed reports "nothing copied" forever and
    /// nobody ever finds out why paste does not work.
    ///
    /// Asserted for **both** sessions rather than for xclip alone, which is the
    /// point of there being one backend: the Wayland copy of this rule used to
    /// be a separate seventy lines that nothing tied to this one.
    #[tokio::test]
    async fn a_missing_tool_is_a_fault_rather_than_an_empty_clipboard() {
        type Build = fn(Arc<dyn CommandRunner>) -> CliClipboard;
        let cases: [(&str, Build, &str); 2] = [
            (
                "xclip -selection clipboard -t TARGETS -o",
                CliClipboard::x11,
                "xclip",
            ),
            ("wl-paste -l", CliClipboard::wayland, "wl-paste"),
        ];
        for (invocation, build, tool) in cases {
            let runner = arc(FakeRunner::new().with(
                invocation,
                127,
                "",
                &format!("{tool}: command not found"),
            ));
            let error = build(runner).targets().await.unwrap_err().to_string();
            assert!(error.contains("not installed"), "{error}");
            assert!(error.contains(tool), "{error}");
        }
    }

    #[tokio::test]
    async fn an_empty_target_list_is_an_empty_clipboard_not_an_error() {
        let runner = arc(FakeRunner::new().with(
            "xclip -selection clipboard -t TARGETS -o",
            1,
            "",
            "Error: target TARGETS not available",
        ));
        let clipboard = CliClipboard::x11(runner);
        assert!(clipboard.targets().await.unwrap().is_empty());
    }

    /// A type the channel does not carry is refused before a subprocess runs.
    #[tokio::test]
    async fn a_type_outside_the_table_is_never_shelled_out_for() {
        let runner = Arc::new(FakeRunner::new());
        let clipboard = CliClipboard::x11(runner.clone());
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
        let clipboard = CliClipboard::x11(runner.clone());

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
        let clipboard = CliClipboard::x11(runner.clone());

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
        let clipboard = CliClipboard::x11(runner.clone());
        clipboard.write(TEXT, b"hi").await.unwrap();

        let call = runner.calls().first().cloned().unwrap_or_default();
        assert!(call.contains("-selection clipboard"), "{call}");
    }

    /// A type outside the table is refused before a subprocess runs, the same
    /// way a read of one is.
    #[tokio::test]
    async fn a_write_of_an_uncarried_type_never_shells_out() {
        let runner = Arc::new(FakeRunner::new());
        let clipboard = CliClipboard::x11(runner.clone());
        assert!(!clipboard.write("application/pdf", b"%PDF").await.unwrap());
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    /// A write has no stderr to quote, so the exit status has to be in the
    /// message or the failure is unattributable.
    #[tokio::test]
    async fn a_failed_write_names_its_exit_status() {
        let runner =
            arc(FakeRunner::new().with("xclip -selection clipboard -t UTF8_STRING -i", 1, "", ""));
        let clipboard = CliClipboard::x11(runner);
        let error = clipboard
            .write(TEXT, b"hi")
            .await
            .expect_err("should be a fault");
        assert!(error.to_string().contains("exit 1"), "{error}");
    }

    /// Writes go to wl-copy; only reads go to wl-paste. The one place the two
    /// sessions genuinely differ, and the reason the write invocation is its
    /// own table rather than a flag on the read.
    #[tokio::test]
    async fn wayland_writes_through_wl_copy() {
        let runner = Arc::new(FakeRunner::new().with("wl-copy --type image/png", 0, "", ""));
        let clipboard = CliClipboard::wayland(runner.clone());

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
        let clipboard = CliClipboard::wayland(runner);
        // Text leads, and the spellings of it collapse to one entry.
        assert_eq!(clipboard.targets().await.unwrap(), vec![TEXT, "text/html"]);
    }

    #[tokio::test]
    async fn wayland_reads_without_a_trailing_newline() {
        let runner =
            Arc::new(FakeRunner::new().with_bytes("wl-paste -n -t image/png", 0, b"\x89PNG", ""));
        let clipboard = CliClipboard::wayland(runner.clone());
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

    /// The argv is data now, so what each session actually runs is worth
    /// pinning in one place: the type name goes in the middle, and every
    /// argument around it survives.
    #[tokio::test]
    async fn each_session_runs_the_invocation_it_names() {
        let runner = Arc::new(FakeRunner::new());
        CliClipboard::wayland(runner.clone())
            .read(PNG)
            .await
            .unwrap_or_default();
        CliClipboard::wayland(runner.clone())
            .write(TEXT, b"hi")
            .await
            .unwrap_or_default();

        let calls = runner.calls();
        assert!(
            calls.iter().any(|c| c == "wl-paste -n -t image/png"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c == "wl-copy --type text/plain;charset=utf-8"),
            "{calls:?}"
        );
    }
}

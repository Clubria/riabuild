//! What riabuild prints as it works.
//!
//! Every method here is a line the developer reads while a run is in progress
//! — the banner, a task starting, a task finished, a note, a warning. They
//! share one obligation: a status line already on the row has to be *covered*
//! before anything replaces it, because a shorter line printed over a longer
//! one leaves the tail of the old one behind.

use riabuild_theme::{Role, Theme};
use std::io::Write;
use std::sync::atomic::Ordering;

use crate::Ui;
use crate::wrap::Detail;

/// Spaces needed for `line` to cover a status line `previous` columns wide.
fn cover(previous: usize, line: &str) -> usize {
    previous.saturating_sub(line.chars().count())
}

/// A note whose tail is the part the developer has to act on, painted as two
/// spans rather than one.
///
/// The two cannot be nested. `Theme::paint` closes with `\x1b[0m`, which resets
/// every attribute rather than the one it opened, so a `Strong` value formatted
/// *into* a `Muted` line would end the dim at the value and leave everything
/// after it undimmed. Painting the prose and the value separately is what makes
/// the emphasis local to the value.
///
/// Pure, and taking its `Theme`, so a test can assert what each rung of the
/// ladder receives without owning a terminal — the same reason `depth_for` is
/// split out of `Theme::detect`.
fn value_line(theme: Theme, text: &str, value: &str) -> String {
    format!(
        "    {} {}",
        theme.paint(Role::Muted, text),
        theme.paint(Role::Strong, value)
    )
}

impl Ui {
    /// The mark, the wordmark, and what this invocation is about to work on.
    pub fn banner(&self, org: &str) {
        if self.quiet {
            return;
        }
        println!();
        for line in crate::art::banner(
            self.theme,
            crate::art::glyphs_render(),
            org,
            riabuild_version::VERSION,
        ) {
            println!("{line}");
        }
        println!();
    }

    pub fn heading(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("\n{}", self.paint(Role::Strong, text));
    }

    /// A task that needed nothing.
    pub fn satisfied(&self, title: &str) {
        if self.quiet {
            return;
        }
        println!(
            "  {} {}",
            self.paint(Role::Ok, "●"),
            self.paint(Role::Muted, title)
        );
    }

    /// A task about to run, with the reason it is running.
    pub fn working(&self, title: &str, reason: &str) {
        if self.quiet {
            return;
        }
        // Measured without the colour escapes, which occupy no columns.
        self.pending.store(
            format!("  ◐ {title} — {reason}").chars().count(),
            Ordering::Relaxed,
        );
        print!(
            "  {} {} {}",
            self.paint(Role::Busy, "◐"),
            title,
            self.paint(Role::Muted, &format!("— {reason}")),
        );
        let _ = std::io::stdout().flush();
    }

    pub fn applied(&self, title: &str) {
        if self.quiet {
            return;
        }
        // This line is written over the status line, which is longer: it also
        // carried the reason the task ran. Padding has to cover whatever was
        // there, not a fixed guess — a fixed ten spaces left the tail of
        // "— first run" behind, so finished tasks read "● GitHub CLI    un".
        let line = format!("  ● {title}");
        let padding = " ".repeat(cover(self.take_pending(), &line));
        println!("\r  {} {}{}", self.paint(Role::Ok, "●"), title, padding);
    }

    /// One deliberate blank line, at a handoff.
    ///
    /// Spacing across a handoff is nobody's by default, and that is the bug it
    /// exists to fix: `ssh` prints `Connection to … closed.` the moment the
    /// remote command ends, `mosh` prints `[mosh is exiting.]` when it lets go,
    /// and the environment shell prints an accounts box the second it starts.
    /// None of them know what riabuild printed a line earlier, so the rule is
    /// riabuild's to keep: **one blank line before handing the terminal to a
    /// child that prints its own lines, and one after taking it back**.
    ///
    /// The other half of the rule is that whatever runs on the far side prints
    /// none of its own — see `provision::open_shell`, where a laptop separates
    /// its own shell and a server does not, because the laptop that opened the
    /// connection already did.
    ///
    /// Counted rather than recorded in tests: what a spacing regression looks
    /// like is a blank line missing or doubled, never one with the wrong text
    /// in it.
    pub fn blank(&self) {
        if self.quiet {
            return;
        }
        // A blank line ends the status line, exactly as a note does.
        self.take_pending();
        #[cfg(any(test, feature = "testing"))]
        self.blanks.fetch_add(1, Ordering::Relaxed);
        println!();
    }

    /// How many blank lines this `Ui` printed.
    #[cfg(any(test, feature = "testing"))]
    pub fn blanks(&self) -> usize {
        self.blanks.load(Ordering::Relaxed)
    }

    pub fn note(&self, text: &str) {
        if self.quiet {
            return;
        }
        // A note is written on the end of the status line and ends it, so
        // there is nothing left for `applied` to cover.
        self.take_pending();
        #[cfg(any(test, feature = "testing"))]
        crate::recorded(&self.noted).push(text.to_string());
        // Recorded whole and printed folded: a test asserting what the
        // developer was told should not have to know where the terminal
        // happened to break the sentence.
        for line in crate::wrap::fold(text, self.width.saturating_sub(crate::wrap::INDENT.len())) {
            println!("{}{}", crate::wrap::INDENT, self.paint(Role::Muted, &line));
        }
    }

    /// A note ending in something the developer has to read off the screen and
    /// type somewhere else: a device code, a one-time value.
    ///
    /// The prose stays `Muted` like every other note; the value does not. A
    /// device code is transcribed by hand into a browser on another machine,
    /// and printing it dim makes the one line that has to be legible the least
    /// legible thing on the screen — under `Signing … in to riabuild` there are
    /// three dim lines and no way to tell which one is the work.
    ///
    /// `Strong` rather than a hue: this is emphasis, not a status. `Brand` and
    /// `Danger` share `1;31` on a sixteen-colour terminal, so a brand-coloured
    /// code would read as an error message on exactly the terminals — an old
    /// server over SSH — where this flow is the entire interface.
    pub fn note_value(&self, text: &str, value: &str) {
        if self.quiet {
            return;
        }
        // A note ends the status line; see `note`.
        self.take_pending();
        #[cfg(any(test, feature = "testing"))]
        crate::recorded(&self.noted).push(format!("{text} {value}"));
        println!("{}", value_line(self.theme, text, value));
    }

    pub fn warn(&self, text: &str) {
        // Deliberately not gated on `quiet`, and on stderr: a warning is what
        // riabuild says in place of stopping, so it is the one line a run
        // asked to be silent still has to produce.
        #[cfg(any(test, feature = "testing"))]
        crate::recorded(&self.warned).push(text.to_string());
        self.end_status_line();
        for (index, line) in
            crate::wrap::fold(text, self.width.saturating_sub(crate::wrap::INDENT.len()))
                .iter()
                .enumerate()
        {
            if index == 0 {
                eprintln!("  {} {line}", self.paint(Role::Warn, "▲"));
            } else {
                // Under the first word, never under the mark: a hanging indent
                // is what keeps the block reading as one warning.
                eprintln!("{}{line}", crate::wrap::INDENT);
            }
        }
    }

    /// A task that could not be finished, and that did not stop the run.
    ///
    /// The `▲` counterpart of [`Ui::applied`]. It covers the busy line the same
    /// way, so the task resolves instead of sitting at `◐` for the rest of the
    /// run, and it carries the outcome where the reason for running was. The
    /// explanation follows beneath it, folded and dimmed — one mark for the
    /// whole block, because a second `▲` under the first says nothing the first
    /// did not.
    ///
    /// Two streams, on purpose. The mark and the outcome belong to the task
    /// ladder, which is on stdout and is what the busy line has to be covered
    /// on; the explanation is a warning and joins the rest of them on stderr. A
    /// run asked to be quiet printed no ladder to cover, so there it goes to
    /// stderr with the explanation — a warning is the one thing `--quiet` does
    /// not silence.
    pub fn unresolved(&self, title: &str, outcome: &str, detail: &[Detail]) {
        // Recorded as one warning, not as a title and some lines: a test
        // asserting that a downgraded path told the developer what happened
        // should not have to know how the block was split up to print it.
        #[cfg(any(test, feature = "testing"))]
        crate::recorded(&self.warned).push(
            std::iter::once(format!("{title} — {outcome}"))
                .chain(detail.iter().map(|line| line.text().to_string()))
                .collect::<Vec<_>>()
                .join(" "),
        );
        // Measured without the colour escapes, exactly as `applied` does.
        let plain = format!("  ▲ {title} — {outcome}");
        let padding = " ".repeat(cover(self.take_pending(), &plain));
        let painted = format!(
            "  {} {} {}",
            self.paint(Role::Warn, "▲"),
            title,
            self.paint(Role::Muted, &format!("— {outcome}"))
        );
        if self.quiet {
            eprintln!("{painted}");
        } else {
            println!("\r{painted}{padding}");
            let _ = std::io::stdout().flush();
        }
        for line in crate::wrap::detail_lines(self.theme, self.width, detail) {
            eprintln!("{line}");
        }
    }

    pub fn info(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_finished_task_covers_the_status_line_it_replaces() {
        // "  ◐ GitHub CLI — first run" is 26 columns and "  ● GitHub CLI" is
        // 14. The ten fixed spaces this used to print reached column 24 and
        // left the last two behind, so a satisfied task rendered as
        // "● GitHub CLI          un".
        assert_eq!("  ◐ GitHub CLI — first run".chars().count(), 26);
        assert!(cover(26, "  ● GitHub CLI") > 10);
        assert_eq!(cover(26, "  ● GitHub CLI"), 12);
    }

    #[test]
    fn a_longer_finished_line_is_not_padded_backwards() {
        assert_eq!(cover(8, "  ● Node and pnpm"), 0);
    }

    #[test]
    fn a_note_ends_the_status_line_so_nothing_is_left_to_cover() {
        let ui = Ui::new(false);
        ui.working("Infisical CLI", "first run");
        ui.note("Installing infisical with Homebrew…");
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn a_device_code_is_not_dimmed_along_with_the_words_in_front_of_it() {
        use riabuild_theme::Depth;
        // The dim run closes before the code opens. Written the obvious way —
        // a `Strong` value formatted into a `Muted` line — the inner reset
        // would end the dim at the code and leave the rest of the line
        // undimmed, emphasising everything except the thing to emphasise.
        let line = value_line(
            Theme::with_depth(Depth::TrueColor),
            "Enter code",
            "DHNT-ZSDM",
        );
        assert_eq!(line, "    \x1b[2mEnter code\x1b[0m \x1b[1mDHNT-ZSDM\x1b[0m");
    }

    #[test]
    fn a_highlighted_value_still_reads_as_a_sentence_without_colour() {
        // NO_COLOR, a pipe, a CI log: the emphasis is gone and the line has to
        // survive being nothing but its words.
        let line = value_line(Theme::plain(), "Enter code", "DHNT-ZSDM");
        assert_eq!(line, "    Enter code DHNT-ZSDM");
        assert!(!line.contains('\x1b'));
    }

    #[test]
    fn a_highlighted_value_is_recorded_as_one_note() {
        // Painted as two spans, recorded as one sentence: a test asserting on
        // what the developer was told should not have to know the line was
        // split to colour it.
        let ui = Ui::new(false);
        ui.note_value("Enter code", "DHNT-ZSDM");
        assert_eq!(ui.noted(), vec!["Enter code DHNT-ZSDM"]);
    }

    #[test]
    fn a_warning_ends_the_status_line_it_interrupts() {
        // The reported bug: `warn` writes to stderr, so it cannot carry the
        // `\r` that covers stdout, and it left the busy line unterminated —
        // "◐ Authorised — installing the key  ▲ riabuild's key is already…"
        // on one line, with the task never resolving.
        let ui = Ui::new(false);
        ui.working("Authorised", "installing the key");
        ui.warn("something to say about it");
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn an_unresolved_task_covers_the_busy_line_like_a_finished_one() {
        let ui = Ui::new(false);
        ui.working("Authorised", "installing the key");
        ui.unresolved("Authorised", "the server refuses it", &[]);
        assert_eq!(ui.take_pending(), 0);
    }

    #[test]
    fn an_unresolved_task_is_recorded_as_one_warning() {
        // `Ok(())` on its own is indistinguishable from a step that silently
        // did nothing, so the recorder is what a test asserts a downgraded
        // path actually spoke up — and it should not have to know the block
        // was split into a title and three paragraphs to print it.
        let ui = Ui::new(false);
        ui.unresolved(
            "Authorised",
            "the server refuses it",
            &[
                Detail::Prose("It is already in the file."),
                Detail::Verbatim("ssh-ed25519 AAAA riabuild"),
            ],
        );
        assert_eq!(
            ui.warned(),
            vec![
                "Authorised — the server refuses it It is already in the file. ssh-ed25519 AAAA riabuild"
            ]
        );
    }

    #[test]
    fn a_status_line_is_only_covered_once() {
        let ui = Ui::new(false);
        ui.working("GitHub CLI", "first run");
        ui.applied("GitHub CLI");
        assert_eq!(ui.take_pending(), 0);
    }
}

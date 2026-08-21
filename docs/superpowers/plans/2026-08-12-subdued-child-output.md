# Subdued child output — Implementation Plan

> **Completed — historical record, do not execute.** Shipped in #48, 2026-08-12. The
> unchecked `- [ ]` boxes below are how the plan was written and not work outstanding, and
> the instruction to an agentic worker to implement it task-by-task that stood here has
> been removed: acting on it would rebuild something that already ships. See
> [`README.md`](README.md) for the index, and the design spec for what the code does now.

**Goal:** Run riabuild's provisioning subprocesses under a pty riabuild owns, discard every escape sequence they emit, and print what remains one dimmed line at a time.

**Architecture:** A new `RunOptions.subdued: Option<Theme>` field, honoured only by `run_interactive`. When set and a real terminal is present, `runner/pty.rs` allocates a pty with `libc::openpty`, gives the child a controlling terminal, and pumps both directions; `runner/subdue.rs` is a pure byte-to-line filter with no IO in it. Absent a terminal the field is ignored and the call is today's plain fd inherit.

**Tech Stack:** Rust 2024, tokio current-thread (`AsyncFd`, `signal`), `libc` (already a direct dependency), `theme.rs` roles.

**Spec:** `docs/superpowers/specs/2026-08-12-subdued-child-output-design.md`

## Global Constraints

- Every external process goes through `CommandRunner`. No `std::process::Command` outside `runner/`.
- All IO is async on a current-thread runtime. No blocking read or write on the runtime thread — `AsyncFd`, never `spawn_blocking` for anything unkillable.
- No `unwrap()` in production code: `unwrap_used = "deny"`. Tests are exempt via the `cfg_attr` in `main.rs`.
- Colour is chosen by **role** from `theme.rs`, never by writing an escape code at a call site. Child lines are `Role::Muted`.
- `cfg!(target_os)` / `std::env::consts::OS` only in `paths.rs`, `keychain/`, `tools.rs`, `download/`, `update.rs`. `#[cfg(unix)]` for a platform *capability* is fine and already present in `runner/mod.rs`.
- ~300 lines of production code per file; `#[cfg(test)]` modules do not count.
- Verification gate for every task: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- All work on branch `worktree-feat+subdued-child-output`, PR at the end. Do not push to `main`.

---

### Task 1: The filter — `runner/subdue.rs`

Pure bytes-to-lines. No terminal, no theme, no IO. This is the whole of the "line discipline" guarantee and it is testable with no tty anywhere.

**Files:**
- Create: `riabuild-cli/src/runner/subdue.rs`
- Modify: `riabuild-cli/src/runner/mod.rs` (add `mod subdue;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub(super) struct Subdue { /* private */ }
  impl Subdue {
      pub(super) fn new() -> Self;
      /// Bytes in, completed lines out. A line completes only on `\n`.
      pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<String>;
      /// The unterminated line as it currently stands, if it has any content.
      pub(super) fn partial(&self) -> Option<String>;
  }
  ```

The line buffer is `Vec<u8>`, not `String`: a read can split a multi-byte character, and `\r` overwrites happen byte-wise. Conversion is `String::from_utf8_lossy` at emit time, which is what `CommandOutput` already does for captured stdout.

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/runner/subdue.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8]) -> Vec<String> {
        Subdue::new().feed(input)
    }

    #[test]
    fn colour_is_removed_and_the_words_survive() {
        assert_eq!(
            lines(b"\x1b[32mUnpacking riabuild\x1b[0m\n"),
            vec!["Unpacking riabuild"]
        );
    }

    #[test]
    fn a_progress_bar_collapses_to_its_final_frame() {
        // apt rewrites one line over and over. Fifty redraws are one line's
        // worth of information, and the information is the last frame.
        let out = lines(b"Progress: 20%\rProgress: 60%\rProgress: 100%\n");
        assert_eq!(out, vec!["Progress: 100%"]);
    }

    #[test]
    fn a_shorter_rewrite_does_not_leave_the_tail_of_the_longer_one() {
        // "100%" written over "Reading database... 45%" must not read
        // "100%ng database... 45%".
        let out = lines(b"Reading database... 45%\rDone\n");
        assert_eq!(out, vec!["Done"]);
    }

    #[test]
    fn a_prompt_with_no_newline_is_available_before_it_is_answered() {
        // sudo writes this and blocks. A filter that waited for `\n` would
        // show the developer a terminal that had gone silent.
        let mut subdue = Subdue::new();
        assert_eq!(subdue.feed(b"[sudo] password for ilya: "), Vec::<String>::new());
        assert_eq!(
            subdue.partial().as_deref(),
            Some("[sudo] password for ilya: ")
        );
    }

    #[test]
    fn a_partial_line_continues_rather_than_repeating() {
        let mut subdue = Subdue::new();
        subdue.feed(b"Fetching ");
        assert_eq!(subdue.feed(b"riabuild\n"), vec!["Fetching riabuild"]);
        assert_eq!(subdue.partial(), None);
    }

    #[test]
    fn a_window_title_cannot_be_set() {
        // OSC 0. Left through, the child renames the developer's terminal and
        // leaves it renamed after riabuild exits.
        assert_eq!(
            lines(b"\x1b]0;apt-get\x07installing\n"),
            vec!["installing"]
        );
    }

    #[test]
    fn an_osc_terminated_by_st_is_also_dropped() {
        assert_eq!(lines(b"\x1b]2;title\x1b\\kept\n"), vec!["kept"]);
    }

    #[test]
    fn the_alternate_screen_cannot_be_entered() {
        assert_eq!(lines(b"\x1b[?1049hhello\x1b[?1049l\n"), vec!["hello"]);
    }

    #[test]
    fn cursor_motion_is_dropped_without_eating_the_text_around_it() {
        assert_eq!(lines(b"a\x1b[2Ab\x1b[Kc\n"), vec!["abc"]);
    }

    #[test]
    fn an_escape_split_across_two_reads_is_still_one_escape() {
        let mut subdue = Subdue::new();
        assert_eq!(subdue.feed(b"one\x1b["), Vec::<String>::new());
        assert_eq!(subdue.feed(b"32mtwo\n"), vec!["onetwo"]);
    }

    #[test]
    fn backspace_erases_the_character_before_it() {
        assert_eq!(lines(b"abcd\x08\x08X\n"), vec!["abX"]);
    }

    #[test]
    fn a_bell_is_not_content() {
        assert_eq!(lines(b"done\x07\n"), vec!["done"]);
    }

    #[test]
    fn carriage_returns_do_not_emit_empty_lines() {
        // `\r\n` is one line ending, not a rewind followed by a blank line.
        assert_eq!(lines(b"first\r\nsecond\r\n"), vec!["first", "second"]);
    }

    #[test]
    fn trailing_whitespace_from_an_overwrite_is_trimmed() {
        assert_eq!(lines(b"longer text\rhi\n"), vec!["hi"]);
    }

    #[test]
    fn invalid_utf8_does_not_lose_the_line() {
        assert_eq!(lines(b"caf\xff\n"), vec!["caf\u{fffd}"]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test subdue`
Expected: FAIL to compile — `Subdue` not found.

- [ ] **Step 3: Write the filter**

Prepend to `riabuild-cli/src/runner/subdue.rs`:

```rust
//! What a subdued child is allowed to say.
//!
//! A pty hands back whatever the child drew: colour, cursor motion, an
//! alternate screen, a window title. riabuild prints a page it chose, and a
//! third-party program is not a co-author of it — so everything a child draws
//! *with* is dropped here, and only the text it drew survives.
//!
//! No terminal, no theme, no IO. Bytes in, lines out, which is what lets the
//! whole of the line discipline be tested against canned `apt` and `gh`
//! transcripts rather than against a machine in a particular state.

/// Where the escape parser is between bytes.
///
/// A read can end anywhere, including halfway through a sequence, so this is
/// state rather than a loop inside `feed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    Text,
    /// Seen `ESC`, waiting to learn what kind of sequence this is.
    Escape,
    /// Inside `ESC [ … final`, where final is 0x40..=0x7e.
    Csi,
    /// Inside a string sequence — OSC, DCS, APC, PM — until `BEL` or `ST`.
    String,
    /// Inside a string sequence, having just seen `ESC`: `\` makes it `ST`.
    StringEscape,
}

pub(super) struct Subdue {
    /// The line being assembled. Bytes rather than a `String` because a read
    /// can split a multi-byte character and because `\r` overwrites are
    /// positional.
    line: Vec<u8>,
    /// Where the next byte lands. `\r` moves it to 0 without clearing, which
    /// is what makes a redraw overwrite rather than append.
    column: usize,
    state: Scan,
}

impl Subdue {
    pub(super) fn new() -> Self {
        Self {
            line: Vec::new(),
            column: 0,
            state: Scan::Text,
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut done = Vec::new();
        for &byte in bytes {
            match self.state {
                Scan::Text => {
                    if let Some(line) = self.text(byte) {
                        done.push(line);
                    }
                }
                Scan::Escape => self.state = Self::after_escape(byte),
                // A CSI ends on its first byte in the final range; everything
                // before that is parameters and intermediates.
                Scan::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = Scan::Text;
                    }
                }
                Scan::String => match byte {
                    0x07 => self.state = Scan::Text,
                    0x1b => self.state = Scan::StringEscape,
                    _ => {}
                },
                Scan::StringEscape => {
                    self.state = if byte == b'\\' {
                        Scan::Text
                    } else {
                        Scan::String
                    }
                }
            }
        }
        done
    }

    pub(super) fn partial(&self) -> Option<String> {
        let text = Self::render(&self.line);
        (!text.is_empty()).then_some(text)
    }

    /// One byte outside any escape sequence.
    fn text(&mut self, byte: u8) -> Option<String> {
        match byte {
            b'\n' => {
                let line = Self::render(&self.line);
                self.line.clear();
                self.column = 0;
                Some(line)
            }
            // Rewind, do not clear: the bytes still there are what a shorter
            // redraw leaves behind, and `render` trims what it does not cover.
            b'\r' => {
                self.column = 0;
                None
            }
            0x08 => {
                self.column = self.column.saturating_sub(1);
                None
            }
            0x1b => {
                self.state = Scan::Escape;
                None
            }
            // Every other C0 control — bell, NUL, vertical tab — is a thing a
            // terminal does, not a thing the child said. Tab is content.
            byte if byte < 0x20 && byte != b'\t' => None,
            byte => {
                if self.column < self.line.len() {
                    self.line[self.column] = byte;
                } else {
                    self.line.push(byte);
                }
                self.column += 1;
                None
            }
        }
    }

    /// What `ESC` turned out to introduce.
    fn after_escape(byte: u8) -> Scan {
        match byte {
            b'[' => Scan::Csi,
            // OSC, DCS, SOS, PM, APC: all run until BEL or ST.
            b']' | b'P' | b'X' | b'^' | b'_' => Scan::String,
            // Anything else is a two-byte sequence, already consumed.
            _ => Scan::Text,
        }
    }

    /// Bytes to text, lossily, with the tail of a longer previous frame
    /// trimmed off.
    fn render(line: &[u8]) -> String {
        String::from_utf8_lossy(line).trim_end().to_string()
    }
}
```

Add to `riabuild-cli/src/runner/mod.rs`, beside `mod child;`:

```rust
mod subdue;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test subdue`
Expected: PASS, 14 tests.

Note: `a_shorter_rewrite_does_not_leave_the_tail_of_the_longer_one` passes because `render` trims trailing whitespace only — `"Done"` written over `"Reading database... 45%"` leaves `"Donee..."`-shaped bytes, so if this test fails, the fix is to truncate the buffer at `column` on `\n` when a `\r` has occurred. Implement the simpler version first and let the test decide.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add riabuild-cli/src/runner/subdue.rs riabuild-cli/src/runner/mod.rs
git commit -m "Add the line filter a subdued child's output goes through"
```

---

### Task 2: The seam — `RunOptions.subdued`

The field, the no-terminal degradation as a pure function, and the stub runner recording it. No pty yet: with the field set and no pty module, `run_interactive` still takes today's path, so this task is safe on its own.

**Files:**
- Modify: `riabuild-cli/src/runner/mod.rs` — `RunOptions`, `Recorded`, `record`, a `subdued_calls` accessor, `should_subdue`

**Interfaces:**
- Consumes: `crate::theme::Theme` (already `Copy + Debug + Clone + PartialEq`).
- Produces:
  ```rust
  pub struct RunOptions { /* … */ pub subdued: Option<Theme> }
  /// Split out from `run_interactive` so the ladder is testable without a terminal.
  pub fn should_subdue(is_terminal: bool, subdued: Option<Theme>) -> Option<Theme>;
  #[cfg(test)] impl StubRunner { pub fn subdued_calls(&self) -> Vec<String>; }
  ```

- [ ] **Step 1: Write the failing tests**

Add to the test module at the end of `riabuild-cli/src/runner/mod.rs`:

```rust
#[cfg(test)]
mod subdued_tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn no_terminal_means_no_subduing_whatever_the_caller_asked_for() {
        // CI, `cargo test`, a pipe. An unattended run must not take a
        // different code path from an attended one for a cosmetic reason.
        assert_eq!(should_subdue(false, Some(Theme::plain())), None);
    }

    #[test]
    fn a_terminal_and_a_theme_is_the_only_combination_that_subdues() {
        assert_eq!(should_subdue(true, Some(Theme::plain())), Some(Theme::plain()));
        assert_eq!(should_subdue(true, None), None);
        assert_eq!(should_subdue(false, None), None);
    }

    #[test]
    fn the_default_run_is_not_subdued() {
        // Every existing call site constructs this way; none of them changes
        // behaviour because of this feature.
        assert_eq!(RunOptions::default().subdued, None);
    }

    #[tokio::test]
    async fn the_stub_records_which_commands_were_subdued() {
        let runner = StubRunner::new();
        runner
            .run_interactive(
                "sudo",
                &["apt-get", "update"],
                &RunOptions {
                    subdued: Some(Theme::plain()),
                    ..Default::default()
                },
            )
            .await
            .expect("interactive run");
        runner
            .run_interactive("bash", &["-l"], &RunOptions::default())
            .await
            .expect("interactive run");

        assert_eq!(runner.subdued_calls(), vec!["sudo apt-get update"]);
    }

    #[tokio::test]
    async fn a_scope_carries_the_subdued_flag_through() {
        // `ScopedRunner::merge` clones the options; a field it forgot would be
        // silently dropped for every task that runs under a scope.
        let inner = Arc::new(StubRunner::new());
        let scoped = ScopedRunner::new(inner.clone(), vec![("K".into(), "V".into())]);
        scoped
            .run_interactive(
                "gh",
                &["auth", "login"],
                &RunOptions {
                    subdued: Some(Theme::plain()),
                    ..Default::default()
                },
            )
            .await
            .expect("interactive run");

        assert_eq!(inner.subdued_calls(), vec!["gh auth login"]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test subdued`
Expected: FAIL to compile — no field `subdued`, no `should_subdue`, no `subdued_calls`.

- [ ] **Step 3: Implement the seam**

In `riabuild-cli/src/runner/mod.rs`, add the import and the field:

```rust
use crate::theme::Theme;
```

```rust
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    /// Run this child under a pty riabuild owns, discard everything it draws
    /// with, and print what is left one dimmed line at a time.
    ///
    /// Honoured by `run_interactive` only — the capturing methods never reach
    /// a terminal, so there is nothing there to subdue.
    ///
    /// A `Theme` rather than a `bool` for the reason `CLAUDE.md` gives for
    /// generated rcfile text: the palette is resolved on the side that has a
    /// `Ui` and passed to the side that does not. `runner/` has no `Ui` and
    /// must not grow one. `Theme::plain()` is a legitimate value — line
    /// discipline with no dim — which is what a `NO_COLOR` run produces
    /// without a special case.
    pub subdued: Option<Theme>,
}

/// Whether this call actually gets a pty.
///
/// Split out from `run_interactive` so the ladder is testable without a
/// terminal, the same reason `theme::depth_for` is split out from
/// `Theme::detect`.
pub fn should_subdue(is_terminal: bool, subdued: Option<Theme>) -> Option<Theme> {
    is_terminal.then_some(subdued).flatten()
}
```

Add `subdued: bool` to the `#[cfg(test)] struct Recorded`, set it in `record`:

```rust
subdued: options.subdued.is_some(),
```

and add the accessor beside `calls()`:

```rust
/// The invocations that asked for a pty. `calls()` answers what ran; this
/// answers which of it riabuild took responsibility for the look of.
pub fn subdued_calls(&self) -> Vec<String> {
    self.recorded
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.subdued)
        .map(|call| call.invocation.clone())
        .collect()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS, whole suite. If any `RunOptions { … }` literal fails to compile for a missing field, add `..Default::default()` to it — do not spell the new field out at call sites that do not want it.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add -A riabuild-cli/src
git commit -m "Add the subdued seam to RunOptions, off everywhere"
```

---

### Task 3: The pty — `runner/pty.rs`

**Files:**
- Create: `riabuild-cli/src/runner/pty.rs`
- Modify: `riabuild-cli/src/runner/mod.rs` (`mod pty;`, the branch in `RealRunner::run_interactive`)
- Modify: `riabuild-cli/Cargo.toml` (tokio `signal` feature)

**Interfaces:**
- Consumes: `Subdue` from Task 1, `should_subdue` from Task 2.
- Produces:
  ```rust
  #[cfg(unix)]
  pub(super) async fn run(command: Command, theme: Theme, program: &str) -> Result<i32>;
  ```

Four pieces, in this order: a `Restore` guard, a `Painter`, the pty setup, the pump.

**The guard.** Raw mode and `O_NONBLOCK` on fd 0 are process-wide changes to the developer's own terminal. If riabuild returns early — a `?` above, a cancelled future — and does not put them back, the developer's shell is left with no echo. That outlives the process that caused it, which is the one class of failure a provisioner must not have.

**Why `O_NONBLOCK` on fd 0 at all.** `AsyncFd` needs it, and the alternatives are worse: `tokio::io::stdin()` reads on a blocking thread that cannot be cancelled, so a keystroke typed after the child exits would be swallowed by a read nobody is waiting for — and the next `ui::ask` would lose it. The flag is restored by the same guard as the termios.

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/runner/pty.rs` with the test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Depth, Theme};

    #[test]
    fn a_line_is_indented_to_note_depth_and_dimmed() {
        let mut painter = Painter::new(Theme::with_depth(Depth::Ansi16));
        assert_eq!(painter.line("Unpacking"), "\r    \x1b[2mUnpacking\x1b[0m\n");
    }

    #[test]
    fn a_plain_theme_still_indents_and_still_ends_the_line() {
        let mut painter = Painter::new(Theme::plain());
        assert_eq!(painter.line("Unpacking"), "\r    Unpacking\n");
    }

    #[test]
    fn a_partial_line_is_written_without_ending_it() {
        let mut painter = Painter::new(Theme::plain());
        assert_eq!(painter.partial("Password: "), "\r    Password: ");
    }

    #[test]
    fn a_redraw_covers_what_the_longer_frame_left_behind() {
        // Same idea as `ui::applied` covering the status line: padding has to
        // cover what was there, not a fixed guess.
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Reading database... 45%");
        assert_eq!(painter.partial("Done"), "\r    Done                   ");
    }

    #[test]
    fn a_line_after_a_partial_covers_it_too() {
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Progress: 100%");
        assert_eq!(painter.line("Done"), "\r    Done          \n");
    }

    #[test]
    fn repainting_the_same_partial_writes_nothing() {
        // The pump repaints after every read. A child that writes a prompt and
        // then waits must not have it reprinted on each wakeup.
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Password: ");
        assert_eq!(painter.partial("Password: "), "");
    }

    #[test]
    fn the_child_gets_the_terminal_width_less_the_indent() {
        // Otherwise the child wraps at the full width and the indent pushes
        // every wrapped line four columns past the right edge.
        assert_eq!(child_columns(80), 76);
        // Never zero or negative, whatever the terminal claims.
        assert_eq!(child_columns(4), 1);
        assert_eq!(child_columns(0), 1);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test pty`
Expected: FAIL to compile — `Painter` and `child_columns` not found.

- [ ] **Step 3: Write the painter and the width rule**

Prepend to `riabuild-cli/src/runner/pty.rs`:

```rust
//! A child that prints through riabuild rather than over it.
//!
//! `run_interactive` normally hands the terminal to the child and looks away —
//! the handoff `CLAUDE.md` exempts from the async-IO rule. A subdued child does
//! not get that. It gets a pty riabuild owns, its output goes through
//! `subdue::Subdue`, and what survives is printed as dimmed lines at the depth
//! of a note.
//!
//! Unix only, which is the whole supported surface. The IO riabuild performs
//! here is all through `AsyncFd` on the current-thread runtime; no blocking
//! read reaches the reactor thread.

use crate::runner::subdue::Subdue;
use crate::theme::{Role, Theme};

/// The indent child output is printed at — `ui::note`'s, because that is what
/// a line from a child is: a note under the task that started it.
const INDENT: &str = "    ";

/// Renders subdued lines, and remembers how much of the terminal the last
/// unterminated one occupied.
struct Painter {
    theme: Theme,
    /// Columns written by the last `partial`, still on screen. A shorter
    /// redraw has to cover them or the tail of the longer frame stays visible.
    open: usize,
    /// What that partial said, so an unchanged repaint writes nothing.
    last: String,
}

impl Painter {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            open: 0,
            last: String::new(),
        }
    }

    /// A finished line. Ends with a newline; nothing is left open.
    fn line(&mut self, text: &str) -> String {
        let out = self.draw(text);
        self.open = 0;
        self.last.clear();
        out + "\n"
    }

    /// The line as it currently stands, with the child still writing it.
    fn partial(&mut self, text: &str) -> String {
        if text == self.last {
            return String::new();
        }
        let out = self.draw(text);
        self.open = text.chars().count();
        self.last = text.to_string();
        out
    }

    fn draw(&self, text: &str) -> String {
        let padding = " ".repeat(self.open.saturating_sub(text.chars().count()));
        format!(
            "\r{INDENT}{}{padding}",
            self.theme.paint(Role::Muted, text)
        )
    }
}

/// How wide the child is told its terminal is.
///
/// The real width less the indent, so a child that wraps at the width it was
/// given does not push every wrapped line four columns past the right edge.
/// Never zero: a terminal of no width makes some children divide by it.
fn child_columns(terminal: u16) -> u16 {
    terminal.saturating_sub(INDENT.len() as u16).max(1)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test pty`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit the testable half**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add riabuild-cli/src/runner/pty.rs riabuild-cli/src/runner/mod.rs
git commit -m "Paint subdued child lines at note depth, covering redraws"
```

- [ ] **Step 6: Add the tokio signal feature**

In `riabuild-cli/Cargo.toml`, in the tokio dependency's feature list, after `"process",`:

```toml
    # SIGWINCH, forwarded to a subdued child's pty so a resized window reaches
    # the program drawing into it. `process` already pulls this in on unix, so
    # it changes nothing in the graph — it is named because the code depends on
    # it directly and a future trim of `process` would otherwise break it.
    "signal",
```

- [ ] **Step 7: Write the pty allocation and the pump**

Append to `riabuild-cli/src/runner/pty.rs`, above the test module:

```rust
use anyhow::{Context, Result};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use tokio::io::unix::AsyncFd;
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};

/// Whether a pty can be had at all: both ends of the developer's terminal.
pub(super) fn available() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The developer's terminal, put back the way it was found.
///
/// Raw mode and `O_NONBLOCK` on fd 0 are changes to a file description the
/// shell shares. Restoring them on a `?` or a cancelled future is not tidiness:
/// a terminal left in raw mode with no echo outlives the process that did it,
/// and the developer's next command is typed into a shell that shows nothing.
struct Restore {
    termios: libc::termios,
    flags: libc::c_int,
}

impl Drop for Restore {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.termios);
            libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, self.flags);
        }
    }
}

impl Restore {
    /// Puts the terminal into raw mode and fd 0 into non-blocking mode,
    /// returning the guard that undoes both.
    fn take() -> Result<Self> {
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut termios) != 0 {
                return Err(std::io::Error::last_os_error()).context("could not read the terminal");
            }
            let flags = libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL);
            if flags < 0 {
                return Err(std::io::Error::last_os_error()).context("could not read the terminal");
            }
            let guard = Self { termios, flags };

            // Raw, so keystrokes reach the child unbuffered and unechoed: the
            // pty's own line discipline is what echoes them, which is how
            // `sudo` turning ECHO off still hides a password. It also turns
            // ISIG off here, so Ctrl-C is forwarded as a byte and the *child's*
            // line discipline raises SIGINT — the signal reaches the process
            // the developer is looking at.
            let mut raw = termios;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return Err(std::io::Error::last_os_error()).context("could not set the terminal");
            }
            if libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error()).context("could not set the terminal");
            }
            Ok(guard)
        }
    }
}

/// A borrowed descriptor `AsyncFd` can register without owning.
///
/// fd 0 belongs to the shell. Wrapping it in anything that closes on drop
/// would close the developer's terminal out from under riabuild.
struct Borrowed(RawFd);

impl AsRawFd for Borrowed {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

fn winsize() -> libc::winsize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut size) };
    if size.ws_row == 0 {
        size.ws_row = 24;
    }
    size.ws_col = child_columns(if size.ws_col == 0 { 80 } else { size.ws_col });
    size
}

/// Runs `command` under a pty, printing what it says as subdued lines.
pub(super) async fn run(mut command: Command, theme: Theme, program: &str) -> Result<i32> {
    let (master, slave) = open(&winsize())?;

    // Each of the child's three descriptors needs its own handle: `Stdio`
    // takes ownership of what it is given.
    command.stdin(dup(&slave)?);
    command.stdout(dup(&slave)?);
    command.stderr(dup(&slave)?);
    // A subdued child never sees riabuild's own stdio, so nothing it inherits
    // can bypass the filter.
    let master_fd = master.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            // A new session, then claim the slave as its controlling terminal.
            // Without this `sudo` finds no controlling terminal and refuses to
            // prompt — the pty would be three ordinary descriptors.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The child holding the master end is what would keep a read on it
            // from ever returning EOF.
            libc::close(master_fd);
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("could not start `{program}`"))?;
    // The parent's copy of the slave, likewise: while it is open, the master
    // never reports EOF and the pump would wait for a child that had exited.
    drop(slave);

    let _restore = Restore::take()?;
    let code = pump(&mut child, master, theme).await;
    // Restored before anything else prints, so a failure message is not
    // written into a raw terminal.
    drop(_restore);
    code.with_context(|| format!("`{program}` did not finish"))
}

/// `openpty`, as an owned pair.
fn open(size: &libc::winsize) -> Result<(OwnedFd, OwnedFd)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let made = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            size,
        )
    };
    if made != 0 {
        return Err(std::io::Error::last_os_error()).context("could not open a pseudo-terminal");
    }
    // `AsyncFd` requires it, and the master is riabuild's own descriptor.
    unsafe { libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK) };
    Ok(unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) })
}

fn dup(fd: &OwnedFd) -> Result<std::process::Stdio> {
    Ok(std::process::Stdio::from(
        fd.try_clone().context("could not duplicate the terminal")?,
    ))
}

/// Both directions, until the child exits and its output has been drained.
async fn pump(child: &mut tokio::process::Child, master: OwnedFd, theme: Theme) -> Result<i32> {
    let master = AsyncFd::new(master).context("could not watch the pseudo-terminal")?;
    let input = AsyncFd::new(Borrowed(libc::STDIN_FILENO))
        .context("could not watch the terminal")?;
    let mut resized = signal(SignalKind::window_change()).context("could not watch for resizes")?;

    let mut filter = Subdue::new();
    let mut painter = Painter::new(theme);
    let mut code = None;

    loop {
        tokio::select! {
            // Output first: a child that exits having just printed something
            // must not lose it to the exit branch.
            biased;

            ready = master.readable() => {
                let mut guard = ready.context("could not read from the pseudo-terminal")?;
                match guard.try_io(|fd| read(fd.get_ref().as_raw_fd())) {
                    Ok(Ok(bytes)) if bytes.is_empty() => break,
                    Ok(Ok(bytes)) => show(&mut filter, &mut painter, &bytes),
                    // A read on the master after the child has closed the slave
                    // is EIO on Linux and zero bytes on macOS. Both are EOF.
                    Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => break,
                    Ok(Err(error)) => return Err(error).context("could not read from the pseudo-terminal"),
                    Err(_would_block) => continue,
                }
            }

            ready = input.readable() => {
                let mut guard = ready.context("could not read from the terminal")?;
                match guard.try_io(|fd| read(fd.get_ref().as_raw_fd())) {
                    // Forwarded verbatim: unbuffered, uninspected, retained
                    // nowhere. The filter is for the output direction only.
                    Ok(Ok(bytes)) if !bytes.is_empty() => write(master.get_ref().as_raw_fd(), &bytes),
                    // The developer's stdin ended. The child keeps running; it
                    // simply gets no more input.
                    Ok(Ok(_)) => continue,
                    Ok(Err(_)) => continue,
                    Err(_would_block) => continue,
                }
            }

            _ = resized.recv() => {
                let size = winsize();
                unsafe { libc::ioctl(master.get_ref().as_raw_fd(), libc::TIOCSWINSZ, &size) };
            }

            status = child.wait(), if code.is_none() => {
                code = Some(status.context("could not wait for the child")?.code().unwrap_or(1));
                // Not a break: whatever the child wrote just before exiting is
                // still in the pty buffer, and the read branch above drains it
                // to EOF.
            }
        }
    }

    if let Some(text) = filter.partial() {
        print!("{}", painter.line(&text));
    }
    let _ = std::io::stdout().flush();

    match code {
        Some(code) => Ok(code),
        None => Ok(child
            .wait()
            .await
            .context("could not wait for the child")?
            .code()
            .unwrap_or(1)),
    }
}

fn show(filter: &mut Subdue, painter: &mut Painter, bytes: &[u8]) {
    let mut out = String::new();
    for line in filter.feed(bytes) {
        out.push_str(&painter.line(&line));
    }
    if let Some(text) = filter.partial() {
        out.push_str(&painter.partial(&text));
    }
    print!("{out}");
    let _ = std::io::stdout().flush();
}

fn read(fd: RawFd) -> std::io::Result<Vec<u8>> {
    let mut buffer = [0u8; 4096];
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(buffer[..read as usize].to_vec())
}

fn write(fd: RawFd, bytes: &[u8]) {
    let mut written = 0;
    while written < bytes.len() {
        let wrote = unsafe {
            libc::write(
                fd,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };
        if wrote <= 0 {
            return;
        }
        written += wrote as usize;
    }
}
```

- [ ] **Step 8: Wire it into `run_interactive`**

In `riabuild-cli/src/runner/mod.rs`, add `mod pty;` beside `mod subdue;` and replace `RealRunner::run_interactive`:

```rust
    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        let command = RealRunner::build(program, args, options);
        // The handoff `CLAUDE.md` describes is still the default and still the
        // rule for every non-subdued site. Where riabuild does perform the IO,
        // it does so through `AsyncFd` on the current-thread runtime.
        #[cfg(unix)]
        if pty::available()
            && let Some(theme) = should_subdue(true, options.subdued)
        {
            return pty::run(command, theme, program).await;
        }
        let status = command
            .status()
            .await
            .with_context(|| format!("could not start `{program}`"))?;
        Ok(status.code().unwrap_or(1))
    }
```

Note `RealRunner::build` must return the `Command` by value for both branches — it already does. The `let … &&` chain is Rust 2024 let-chains, which this edition supports.

- [ ] **Step 9: Verify**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS. The pty path is not exercised by the suite — `cargo test` has no terminal, so `available()` is false — which is the degradation the spec asks for.

- [ ] **Step 10: Commit**

```bash
git add riabuild-cli/src/runner/pty.rs riabuild-cli/src/runner/mod.rs riabuild-cli/Cargo.toml
git commit -m "Run a subdued child under a pty riabuild owns"
```

---

### Task 4: Turn it on at the three call sites

**Files:**
- Modify: `riabuild-cli/src/update.rs:221`, `:227`, `:238`
- Modify: `riabuild-cli/src/tasks/github_cli/sign_in.rs:82`
- Modify: `riabuild-cli/src/remote/authorise.rs:311-314`

**Interfaces:**
- Consumes: `RunOptions.subdued` from Task 2.
- Produces: nothing new.

`update.rs`'s `run_upgrade` takes `&dyn CommandRunner` and has no `Ui`. It needs the theme passed in. Check its caller and thread a `Theme` parameter through — do not have it detect one, for the reason `CLAUDE.md` gives about `ctx.ui.theme()`.

- [ ] **Step 1: Write the failing tests**

In `riabuild-cli/src/tasks/github_cli/sign_in.rs`'s test module:

```rust
    #[tokio::test]
    async fn the_sign_in_prints_through_riabuild_rather_than_over_it() {
        let mut ctx = crate::testing::ctx();
        run_gh_auth(&mut ctx, &["auth", "login"], "Signing in")
            .await
            .expect("sign-in");
        assert_eq!(
            ctx.runner_stub().subdued_calls(),
            vec!["gh auth login"],
        );
    }
```

Adapt the harness call to whatever this module's existing tests use to reach the stub — read the neighbouring tests first and match them exactly.

In `riabuild-cli/src/update.rs`'s test module:

```rust
    #[tokio::test]
    async fn a_package_manager_upgrade_is_subdued() {
        let runner = StubRunner::new();
        run_upgrade(&runner, &Strategy::Apt, Theme::plain())
            .await
            .expect("upgrade");
        assert_eq!(
            runner.subdued_calls(),
            vec!["sudo apt-get update", "sudo apt-get install --only-upgrade -y riabuild"],
        );
    }

    #[tokio::test]
    async fn riabuild_re_execing_itself_is_not_subdued() {
        // The child is riabuild, whose output is already themed. Subduing it
        // would dim a whole second run and nest its indent under the first.
        let runner = StubRunner::new();
        reexec_for_test(&runner).await;
        assert_eq!(runner.subdued_calls(), Vec::<String>::new());
    }
```

If `reexec` calls `std::process::exit` and cannot be tested, drop that second test and instead assert the absence through the first: `subdued_calls()` returning exactly the two apt invocations already proves nothing else was subdued in that path.

- [ ] **Step 2: Run to verify they fail**

Run: `cd riabuild-cli && cargo test subdued`
Expected: FAIL — `subdued_calls()` is empty.

- [ ] **Step 3: Turn it on**

`update.rs` — thread the theme through `run_upgrade` and set it on the three package-manager calls:

```rust
async fn run_upgrade(runner: &dyn CommandRunner, strategy: &Strategy, theme: Theme) -> Result<bool> {
```

```rust
    let subdued = RunOptions {
        // apt and dnf print more, louder, and in their own colours than
        // anything riabuild says around them. This is the output riabuild
        // takes responsibility for the look of.
        subdued: Some(theme),
        ..Default::default()
    };
```

and pass `&subdued` in place of `&RunOptions::default()` at `:221`, `:227` and `:238`. Leave `:255`, the re-exec, alone. Update the caller of `run_upgrade` to pass `ui.theme()`.

`sign_in.rs:82`:

```rust
    let code = ctx
        .runner
        .run_interactive(
            &ctx.gh(),
            args,
            &RunOptions {
                // A device-code flow is text and a wait — it survives line
                // discipline intact. `gh`'s arrow-key selection prompt would
                // not, which is why only the two commands that reach here are
                // subdued and not every `gh` invocation.
                subdued: Some(ctx.ui.theme()),
                ..Default::default()
            },
        )
        .await?;
```

`authorise.rs` — replace the comment at `:310-312` and set the flag:

```rust
    // Subdued: `ssh-copy-id` prints through riabuild rather than over it. The
    // output direction is filtered; the input direction is not. Whatever the
    // developer types here — a password, a passphrase — is forwarded to the
    // real `ssh` verbatim, unbuffered, uninspected, and retained nowhere. This
    // is a pty, so riabuild copies those keystrokes rather than standing beside
    // them; nothing reads them, and nothing writes them down.
    let code = runner
        .run_interactive(
            "ssh-copy-id",
            &refs,
            &RunOptions {
                subdued: Some(ui.theme()),
                ..Default::default()
            },
        )
        .await?;
```

Confirm `ui` is in scope at that point; if not, thread a `Theme` into the function the same way `update.rs` does.

- [ ] **Step 4: Run to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS, whole suite.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add -A riabuild-cli/src
git commit -m "Subdue apt, dnf, gh auth login, and ssh-copy-id"
```

---

### Task 5: Amend the documented invariants

**Files:**
- Modify: `riabuild-cli/CLAUDE.md` — the stdio exception under "Invariants", the "Colour" section, the `src/` layout table

- [ ] **Step 1: Amend the stdio exception**

Replace the paragraph beginning "The exception is **stdio**" with:

```markdown
The exception is **stdio**. `ui.rs` writes with `println!`/`eprintln!`, and
`run_interactive` hands the terminal to a child process — that is a handoff, not IO
riabuild performs. Async stdout buys nothing for line-at-a-time terminal output.

**Except for a subdued child.** `RunOptions.subdued` runs a child under a pty riabuild
owns, so riabuild does perform that IO: it reads the child's output, drops every escape
sequence in it, and prints the rest as dimmed lines. That is why `runner/pty.rs` pumps
through `AsyncFd` and never a blocking read — a subdued `sudo apt-get` holds the runtime
for as long as the developer takes to type a password. The handoff remains the default
and the rule everywhere `subdued` is `None`, which is every site except apt, dnf,
`gh auth login`, and `ssh-copy-id`.
```

- [ ] **Step 2: Add child output to the Colour section**

Append to the "## Colour" section:

```markdown
Child output has a role too. A subdued child's lines are `Muted`, printed at `ui::note`'s
indent, and everything the child drew *with* — colour, cursor motion, alternate screen,
window title — is dropped before it reaches the terminal. riabuild does not trust a
third-party program to keep a tidy terminal, so under `RunOptions.subdued` it does not
have to. Design: `../docs/superpowers/specs/2026-08-12-subdued-child-output-design.md`.
```

- [ ] **Step 3: Add the two modules to the layout table**

In the `src/` block, extend the `runner.rs` line:

```
  runner/     CommandRunner — all subprocesses; `subdue.rs` is the line filter a
              subdued child's output goes through and `pty.rs` is the terminal it
              gets instead of riabuild's own
```

- [ ] **Step 4: Commit**

```bash
git add riabuild-cli/CLAUDE.md
git commit -m "Record that a subdued child is an exception to the stdio handoff"
```

---

### Task 6: Manual verification and the PR

The pty path is unreachable from `cargo test` by construction — that is the degradation, not a gap in the suite — so it is checked by hand.

- [ ] **Step 1: Build and check the filter against a real program**

```bash
cd riabuild-cli && cargo build
```

- [ ] **Step 2: Confirm the terminal survives an interrupt**

On a real terminal, start a subdued command and Ctrl-C it, then confirm the shell still echoes:

```bash
echo "typing here should be visible"
```

Expected: the child dies, riabuild continues or exits, and the shell echoes normally. A shell that shows nothing means the `Restore` guard did not run.

- [ ] **Step 3: Open the PR and wait for it**

```bash
git push -u origin worktree-feat+subdued-child-output
gh pr create --fill
gh pr checks --watch
```

Per the root `CLAUDE.md`: work is not finished until PR CI has completed. If CI fails, fixing it is part of this task.

---

## Self-Review

**Spec coverage.** The seam → Task 2. Where it applies → Task 4. Degradation → `should_subdue`, Task 2, plus the `available()` gate in Task 3. The pty (openpty, setsid/TIOCSCTTY, AsyncFd, winsize/SIGWINCH, raw mode guard, EIO) → Task 3. The filter (CSI, OSC, `\r`, `\x08`, partial lines, the four-space indent, purity) → Tasks 1 and 3. Stdin and the changed guarantee → the pump's input branch and the rewritten comment in Task 4. The invariant amendment → Task 5. Code layout and testing → Tasks 1–3 and 6.

**Placeholders.** None. Two steps say "match the neighbouring tests" for a harness this plan cannot see from here (`sign_in.rs`'s ctx helper, `update.rs`'s `run_upgrade` caller); both name exactly what to read and what the assertion must be.

**Type consistency.** `Subdue::{new, feed, partial}` are used in Task 3 exactly as defined in Task 1. `should_subdue(bool, Option<Theme>) -> Option<Theme>` is used in Task 3's `run_interactive` as defined in Task 2. `Painter::{new, line, partial}` and `child_columns(u16) -> u16` are defined and used within Task 3. `subdued_calls() -> Vec<String>` is defined in Task 2 and asserted against in Task 4.

**Deviation from the spec, deliberate.** The spec says partial lines are emitted "when the read drains". The plan repaints the partial after *every* read instead, and `Painter::partial` returns an empty string when nothing changed. This is strictly simpler — no drain detection — and gives the same result, because a repaint of an unchanged line writes nothing and a changed one covers what it replaces. The spec sentence is updated to match.

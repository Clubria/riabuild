# Laptop Channel and Clipboard Bridge — Implementation Plan

> **Completed — historical record, do not execute.** Written for #27 on 2026-08-07 and
> shipped in #35 on 2026-08-09; the transport it describes was then replaced on
> 2026-08-13, so this plan is two changes behind the code rather than one. The unchecked
> `- [ ]` boxes below are how the plan was written and not work outstanding, and the
> instruction to an agentic worker to implement it task-by-task that stood here has been
> removed: acting on it would rebuild something that already ships, in the shape it no
> longer has. See [`README.md`](README.md) for the index, and
> [`../specs/2026-08-13-exec-channel-transport-design.md`](../specs/2026-08-13-exec-channel-transport-design.md)
> for the transport that did land.

**Goal:** Build a general-purpose request channel from a remote server back to the developer's laptop, with the clipboard as its first consumer, so Ctrl+V in Claude Code over `riabuild remote` attaches the laptop's clipboard content.

**Architecture:** A laptop-side agent listens on a unix socket and answers a compiled-in allowlist of operations by reading the local clipboard through `CommandRunner`. `ssh -N -R` forwards that socket onto the server, where a PATH shim named `xclip`/`wl-paste` in `~/.riabuild/bin` translates the tool's own argv vocabulary into one request and prints the reply in the tool's own output format. The server asks; the laptop decides; the channel carries no credentials and its absence degrades to "no clipboard" only.

**Tech Stack:** Rust 2024, tokio (current-thread, `net` feature already enabled), `serde`/`serde_json`, `async-trait`, `anyhow`. One new dependency: `image` (PNG/TIFF decode, PNG encode) for the long-edge ceiling.

**Spec:** `docs/superpowers/specs/2026-08-07-clipboard-channel-design.md`

## Scope: what this plan builds, and what it defers

Remote mode (`docs/superpowers/specs/2026-08-06-remote-mode-design.md`) is unmerged. This plan builds everything that does not require `src/remote/` to exist, which is all of the protocol, the agent, the client, the shims, and the supervisor. Deferred to a follow-up once remote mode lands:

- Starting the supervisor as part of the `riabuild remote` flow (`src/remote/mod.rs` wiring).
- The `● Clipboard channel — connected` banner line, which is remote mode's banner.
- Refcounted lifetime via remote mode's `sessions/<pid>` markers and `kill -0` sweep. **Task 12 builds the refcount logic against a directory path** so the wiring is a one-line substitution, not a rewrite.
- The `e2e/remote` container tests: the degradation test and the end-to-end paste test.
- Writing the amendment paragraph into the remote-mode spec (that file is in a sibling worktree).

Clipboard **writes** were added after these thirteen tasks landed; see *Task 14* at the end.

## Global Constraints

Every task's requirements implicitly include these. The first five are from `riabuild-cli/CLAUDE.md` and are not style preferences.

- **Every external process goes through `CommandRunner`.** No `std::process::Command` or `tokio::process` outside `runner.rs`.
- **All IO is async.** `tokio::fs`, never `std::fs`. Exception: stdio in `ui.rs` and the shim's own stdout.
- **`cfg!(target_os)` and `std::env::consts::OS` are confined** to `paths.rs`, `keychain.rs`, `tools.rs`, `download.rs`, `update.rs`. Everywhere else, take the OS as a *parameter* and keep a thin wrapper that supplies the real one — `paths::default_project_dir_on` is the pattern.
- **`clippy::unwrap_used` is `deny`** for the binary target. Tests are exempt via the `cfg_attr` in `main.rs`.
- **One concern per file; none near 300 lines**, tests included.
- **No panics reach a developer.** Every reachable error becomes a `ui::Failure` carrying what was attempted, the command, the detail, and one next action.
- **Protocol version is `1`**; wire constant `PROTOCOL_VERSION: u8 = 1`.
- **Hard payload cap is exactly `32 * 1024 * 1024`** bytes, with a legible failure past it.
- **`MAX_LONG_EDGE: u32 = 2576`** — compiled in, never a config key, environment variable, or dashboard field.
- **The op allowlist is compiled in.** A server can request only what the laptop's binary already implements.
- **File-reference types never appear in any target list**, under any input.
- **Commands:** `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. All three must pass before every commit.

## File Structure

The spec's four-file layout is expanded to eight, because the 300-line rule bites: the MIME table with its table-driven tests, the clipboard backends with three platforms, and the resize path are each their own concern.

| File | Responsibility |
|---|---|
| `src/channel/mod.rs` | module declarations, `socket_path` resolution, `RIABUILD_CHANNEL_SOCKET` |
| `src/channel/mime.rs` | the three vocabularies, normalisation both directions, preference order, filtering |
| `src/channel/protocol.rs` | `Request`/`Response`, the compiled-in allowlist, line framing, the payload cap |
| `src/channel/clipboard.rs` | the `Clipboard` trait and the X11, Wayland, and macOS backends, all via `CommandRunner` |
| `src/channel/resize.rs` | the `MAX_LONG_EDGE` ceiling; pass-through below it |
| `src/channel/agent.rs` | laptop side: unix socket server, dispatch, the snapshot cache |
| `src/channel/client.rs` | server side: connect, one request, one response |
| `src/channel/supervisor.rs` | keeps `ssh -N -R` alive: ssh argv, backoff, ping, refcount, teardown |
| `src/shims/clipboard.rs` | argv parsing for `xclip`/`wl-paste`, output formatting, the PATH guard, pass-through |
| `src/runner.rs` (modify) | `run_bytes` — subprocess output that is not lossy UTF-8 |
| `src/cli.rs` (modify) | the `channel` subcommand |
| `src/main.rs` (modify) | `mod channel;` and dispatch |
| `src/paths.rs` (modify) | `channel_log_file` |

---

### Task 1: Bytes-safe subprocess output

`CommandOutput.stdout` is a `String` built with `String::from_utf8_lossy`. Every byte sequence in a PNG that is not valid UTF-8 becomes U+FFFD, so a screenshot read through today's `CommandRunner` arrives corrupted. Since the spec requires every subprocess to go through `CommandRunner`, the trait has to grow a bytes-returning method before any clipboard code can be correct.

`run_bytes` returns stderr as a `String` deliberately: stderr is diagnostics, always text, and every caller wants to put it in an error message.

**Files:**
- Modify: `riabuild-cli/src/runner.rs`

**Interfaces:**
- Produces:
  - `pub struct BytesOutput { pub code: Option<i32>, pub stdout: Vec<u8>, pub stderr: String }`
  - `impl BytesOutput { pub fn ok(&self) -> bool }`
  - `CommandRunner::run_bytes(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<BytesOutput>`
  - `FakeRunner::with_bytes(self, invocation: &str, code: i32, stdout: &[u8], stderr: &str) -> Self`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block at the end of `riabuild-cli/src/runner.rs`. If that file has no test module, create one with `mod tests { use super::*; ... }`.

```rust
#[cfg(test)]
mod bytes_tests {
    use super::*;

    /// A PNG is not valid UTF-8. Read through `run`, its bytes come back
    /// mangled into replacement characters; `run_bytes` is what makes the
    /// clipboard bridge possible at all.
    #[tokio::test]
    async fn binary_stdout_survives_the_runner() {
        // PNG magic, then a byte that is illegal as UTF-8 on its own.
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF];
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, &png, "");

        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();

        assert!(out.ok());
        assert_eq!(out.stdout, png);
    }

    #[tokio::test]
    async fn an_unstubbed_command_fails_the_same_way_as_run() {
        let runner = FakeRunner::new();
        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(out.code, Some(127));
        assert!(out.stdout.is_empty());
        assert!(out.stderr.contains("no stub"), "{}", out.stderr);
    }

    #[tokio::test]
    async fn bytes_calls_are_recorded_like_every_other_call() {
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, b"hi", "");
        runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(runner.calls(), vec!["xclip -o".to_string()]);
    }

    /// The real runner is exercised through a command every supported
    /// platform has, so this stays a unit test rather than a fixture.
    #[tokio::test]
    async fn the_real_runner_returns_raw_bytes() {
        let out = RealRunner
            .run_bytes(
                "printf",
                &[r"\x89PNG\xff"],
                &RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout, [0x89u8, b'P', b'N', b'G', 0xFF]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test bytes_tests`
Expected: FAIL — `no method named run_bytes`, `no function or associated item named with_bytes`, `cannot find type BytesOutput`.

- [ ] **Step 3: Add `BytesOutput` and the trait method**

In `riabuild-cli/src/runner.rs`, after the `CommandOutput` impl block:

```rust
/// Subprocess output whose stdout is not assumed to be text.
///
/// `CommandOutput` exists for the `--version` and status checks that make up
/// most of riabuild, and its lossy `String` conversion is right for those. The
/// clipboard channel moves PNGs, where a single replacement character is a
/// corrupt image, so it reads through here instead. stderr stays a `String`:
/// it is diagnostics, it is always text, and every caller puts it in a message.
#[derive(Debug, Clone)]
pub struct BytesOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl BytesOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}
```

Add to the `CommandRunner` trait, after `run`:

```rust
    /// Like `run`, but stdout is returned as raw bytes.
    ///
    /// Used by the clipboard channel, where stdout is a PNG rather than a
    /// version string.
    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput>;
```

- [ ] **Step 4: Implement it for `RealRunner`**

In `impl CommandRunner for RealRunner`, after `run`:

```rust
    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput> {
        let mut command = RealRunner::build(program, args, options);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        command.stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))?;

        if let Some(input) = &options.stdin {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().context("stdin was piped")?;
            stdin.write_all(input.as_bytes()).await?;
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .with_context(|| format!("`{program}` did not finish"))?;

        Ok(BytesOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
```

- [ ] **Step 5: Implement it for `FakeRunner`**

`FakeRunner` currently stores `HashMap<String, CommandOutput>`. Add a parallel map for byte stubs so a test can script exact bytes. Add the field to the struct:

```rust
    byte_responses: HashMap<String, Vec<u8>>,
```

Add the builder, after `with`:

```rust
    /// Scripts a command whose stdout is binary.
    ///
    /// Registers a text stub too, so `which` and the exit code resolve through
    /// the same path as `with`, and only stdout differs.
    pub fn with_bytes(mut self, invocation: &str, code: i32, stdout: &[u8], stderr: &str) -> Self {
        self.byte_responses
            .insert(invocation.to_string(), stdout.to_vec());
        self.with(invocation, code, "", stderr)
    }
```

Byte lookup needs the same longest-prefix rule as `stubbed`. Add beside it:

```rust
    fn stubbed_bytes(&self, invocation: &str) -> Option<Vec<u8>> {
        let mut best: Option<(&String, &Vec<u8>)> = None;
        for (key, value) in &self.byte_responses {
            if invocation == key || invocation.starts_with(&format!("{key} ")) {
                let better = best.map(|(k, _)| key.len() > k.len()).unwrap_or(true);
                if better {
                    best = Some((key, value));
                }
            }
        }
        best.map(|(_, bytes)| bytes.clone())
    }

    fn resolve_bytes(&self, program: &str, args: &[&str]) -> Option<Vec<u8>> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.stubbed_bytes(&full)
            .or_else(|| self.stubbed_bytes(&FakeRunner::stub_key(program, args)))
    }
```

And the trait method, in `impl CommandRunner for FakeRunner`:

```rust
    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        _options: &RunOptions,
    ) -> Result<BytesOutput> {
        let invocation = format!("{program} {}", args.join(" "));
        self.calls
            .lock()
            .unwrap()
            .push(invocation.trim_end().to_string());

        let text = self.lookup(program, args);
        let stdout = self
            .resolve_bytes(program, args)
            .unwrap_or_else(|| text.stdout.into_bytes());

        Ok(BytesOutput {
            code: text.code,
            stdout,
            stderr: text.stderr,
        })
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test bytes_tests`
Expected: PASS, 4 tests.

- [ ] **Step 7: Check the whole suite still passes**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green. Any other `impl CommandRunner` in the tree must gain `run_bytes` — if the compiler names one, implement it the same way.

- [ ] **Step 8: Commit**

```bash
git add riabuild-cli/src/runner.rs
git commit -m "feat(runner): read subprocess stdout as bytes

CommandOutput.stdout is built with from_utf8_lossy, which turns every
non-UTF-8 byte of a PNG into U+FFFD. The clipboard channel moves images
through CommandRunner, so it needs output that is not assumed to be text."
```

---

### Task 2: The MIME vocabulary table

Three platforms disagree on what a clipboard type is called, and `text/html` copied out of Safari exists under no name `xclip` recognises. This task is the translation table and nothing else — filtering and ordering are Task 3.

**Files:**
- Create: `riabuild-cli/src/channel/mod.rs`
- Create: `riabuild-cli/src/channel/mime.rs`
- Modify: `riabuild-cli/src/main.rs` (add `mod channel;`)

**Interfaces:**
- Produces:
  - `pub const TEXT: &str = "text/plain;charset=utf-8"`, `HTML`, `PNG`, `TIFF`
  - `pub enum Vocabulary { X11, Wayland, MacOs }`
  - `pub fn to_mime(vocab: Vocabulary, native: &str) -> Option<&'static str>`
  - `pub fn from_mime(vocab: Vocabulary, mime: &str) -> Option<&'static str>`
  - `pub fn is_file_reference(native: &str) -> bool`

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/channel/mime.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The table is asserted in both directions for every row, because a
    /// one-way entry is exactly the bug that makes paste work on one laptop
    /// and silently fail on another.
    #[test]
    fn every_vocabulary_round_trips_through_mime() {
        let rows: &[(Vocabulary, &str, &str)] = &[
            (Vocabulary::MacOs, "public.utf8-plain-text", TEXT),
            (Vocabulary::MacOs, "public.html", HTML),
            (Vocabulary::MacOs, "public.png", PNG),
            (Vocabulary::MacOs, "public.tiff", TIFF),
            (Vocabulary::X11, "UTF8_STRING", TEXT),
            (Vocabulary::X11, "text/html", HTML),
            (Vocabulary::X11, "image/png", PNG),
            (Vocabulary::X11, "image/tiff", TIFF),
            (Vocabulary::Wayland, "text/plain;charset=utf-8", TEXT),
            (Vocabulary::Wayland, "text/html", HTML),
            (Vocabulary::Wayland, "image/png", PNG),
            (Vocabulary::Wayland, "image/tiff", TIFF),
        ];

        for (vocab, native, mime) in rows {
            assert_eq!(to_mime(*vocab, native), Some(*mime), "to_mime {native}");
            assert_eq!(from_mime(*vocab, mime), Some(*native), "from_mime {mime}");
        }
    }

    /// X11 has three names for the same UTF-8 text and only one of them is
    /// canonical. All three must read as text; only the canonical one is ever
    /// written back.
    #[test]
    fn the_legacy_x11_text_atoms_all_read_as_utf8_text() {
        for atom in ["UTF8_STRING", "STRING", "TEXT", "text/plain"] {
            assert_eq!(to_mime(Vocabulary::X11, atom), Some(TEXT), "{atom}");
        }
        assert_eq!(from_mime(Vocabulary::X11, TEXT), Some("UTF8_STRING"));
    }

    /// Wayland reports the same text under two spellings.
    #[test]
    fn wayland_text_is_recognised_with_and_without_the_charset() {
        for native in ["text/plain;charset=utf-8", "text/plain"] {
            assert_eq!(to_mime(Vocabulary::Wayland, native), Some(TEXT), "{native}");
        }
    }

    /// An unknown type is dropped rather than guessed at. Bridging a type the
    /// far side cannot name produces a paste that fails with no explanation.
    #[test]
    fn an_unrecognised_type_has_no_mime() {
        assert_eq!(to_mime(Vocabulary::X11, "MULTIPLE"), None);
        assert_eq!(to_mime(Vocabulary::MacOs, "com.apple.webarchive"), None);
        assert_eq!(from_mime(Vocabulary::X11, "application/pdf"), None);
    }

    /// Copying a file in Finder puts a path on the pasteboard. Bridged
    /// verbatim the server receives a path that does not exist there — the one
    /// payload that is syntactically valid and semantically false on the far
    /// side.
    #[test]
    fn file_reference_types_are_recognised_in_every_vocabulary() {
        for native in [
            "text/uri-list",
            "public.file-url",
            "public.url",
            "x-special/gnome-copied-files",
            "x-special/nautilus-clipboard",
            "application/x-kde-cutselection",
            "FILE_NAME",
            "com.apple.finder.node",
        ] {
            assert!(is_file_reference(native), "{native} should be a file reference");
        }
    }

    #[test]
    fn ordinary_types_are_not_file_references() {
        for native in ["image/png", "text/html", "UTF8_STRING", "public.png"] {
            assert!(!is_file_reference(native), "{native}");
        }
    }

    /// Case is not significant on the wire: X11 atoms are conventionally
    /// upper case and MIME types conventionally lower, and a laptop that
    /// reports `Image/PNG` must not silently lose its clipboard.
    #[test]
    fn lookup_ignores_case() {
        assert_eq!(to_mime(Vocabulary::X11, "image/PNG"), Some(PNG));
        assert_eq!(to_mime(Vocabulary::MacOs, "PUBLIC.PNG"), Some(PNG));
        assert!(is_file_reference("Text/URI-List"));
    }
}
```

- [ ] **Step 2: Wire the module in so the tests compile**

Create `riabuild-cli/src/channel/mod.rs`:

```rust
//! The laptop channel: a request path from a remote server back to the
//! developer's laptop.
//!
//! The server asks and the laptop decides. The operation set is compiled into
//! the binary, so a server can request only what the laptop already implements
//! — it cannot push work, extend the operation set, or execute anything. That
//! asymmetry is what makes a reverse tunnel defensible at all, and it is the
//! architecture rule "the server ships data, never logic" applied to the one
//! direction remote mode had not opened.
//!
//! The channel is strictly optional. Its absence degrades to "no clipboard"
//! and never to "environment broken": a laptop that closes its lid leaves a
//! session that still runs setup, still re-pulls rotated secrets, and still
//! opens a shell.

pub mod mime;
```

In `riabuild-cli/src/main.rs`, add `mod channel;` to the module list, keeping it alphabetical — between `mod cli;` and `mod config;`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test channel::mime`
Expected: FAIL to compile — `cannot find value TEXT`, `cannot find type Vocabulary`, `cannot find function to_mime`.

- [ ] **Step 4: Write the table**

Prepend to `riabuild-cli/src/channel/mime.rs`, above the test module:

```rust
//! The three clipboard vocabularies, and the MIME types the channel speaks.
//!
//! macOS names pasteboard types with uniform type identifiers, X11 with
//! interned atoms that predate MIME, and Wayland with MIME strings that are
//! nearly but not quite the ones X11 uses. The agent normalises to the MIME
//! column; the shim translates back into its own tool's vocabulary. Without
//! this layer `text/html` copied out of Safari exists under no name `xclip`
//! recognises.
//!
//! The macOS column is a *laptop* platform, which is the primary case. macOS
//! as a server is out of scope.

/// UTF-8 plain text. The canonical spelling on the wire.
pub const TEXT: &str = "text/plain;charset=utf-8";
pub const HTML: &str = "text/html";
pub const PNG: &str = "image/png";
pub const TIFF: &str = "image/tiff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vocabulary {
    X11,
    Wayland,
    MacOs,
}

/// Native name → MIME. The first column may hold several spellings per row;
/// only the first is ever written back by `from_mime`.
const X11: &[(&str, &str)] = &[
    ("UTF8_STRING", TEXT),
    ("STRING", TEXT),
    ("TEXT", TEXT),
    ("text/plain;charset=utf-8", TEXT),
    ("text/plain", TEXT),
    ("text/html", HTML),
    ("image/png", PNG),
    ("image/tiff", TIFF),
];

const WAYLAND: &[(&str, &str)] = &[
    ("text/plain;charset=utf-8", TEXT),
    ("text/plain", TEXT),
    ("UTF8_STRING", TEXT),
    ("text/html", HTML),
    ("image/png", PNG),
    ("image/tiff", TIFF),
];

const MACOS: &[(&str, &str)] = &[
    ("public.utf8-plain-text", TEXT),
    ("public.plain-text", TEXT),
    ("NSStringPboardType", TEXT),
    ("public.html", HTML),
    ("public.png", PNG),
    ("public.tiff", TIFF),
];

fn table(vocab: Vocabulary) -> &'static [(&'static str, &'static str)] {
    match vocab {
        Vocabulary::X11 => X11,
        Vocabulary::Wayland => WAYLAND,
        Vocabulary::MacOs => MACOS,
    }
}

/// A native clipboard type name as this platform spells it → the MIME type the
/// channel uses. `None` for anything the channel does not carry.
pub fn to_mime(vocab: Vocabulary, native: &str) -> Option<&'static str> {
    table(vocab)
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(native))
        .map(|(_, mime)| *mime)
}

/// A MIME type → the name this platform's clipboard tool expects.
///
/// Where a platform has several spellings for one type, the first row wins:
/// `UTF8_STRING` rather than the legacy `STRING`, which is what a modern
/// `xclip` wants.
pub fn from_mime(vocab: Vocabulary, mime: &str) -> Option<&'static str> {
    table(vocab)
        .iter()
        .find(|(_, m)| m.eq_ignore_ascii_case(mime))
        .map(|(name, _)| *name)
}

/// Types that name a file rather than carry one.
///
/// These are dropped at the type level and never bridged. Copying a file in
/// Finder puts `file:///Users/ada/Desktop/report.pdf` on the pasteboard;
/// carried across verbatim, the server receives a path that does not exist
/// there, and it is exactly the kind of thing Claude will confidently try to
/// read.
///
/// The exclusion is type-level only. A laptop path copied as plain text is
/// byte-identical to any other string, and scanning text content for
/// path-shaped substrings would corrupt legitimate text to prevent a case the
/// developer chose deliberately.
const FILE_REFERENCE_TYPES: &[&str] = &[
    "text/uri-list",
    "public.file-url",
    "public.url",
    "x-special/gnome-copied-files",
    "x-special/nautilus-clipboard",
    "x-special/mate-copied-files",
    "application/x-kde-cutselection",
    "application/x-kde4-urilist",
    "FILE_NAME",
    "com.apple.finder.node",
    "com.apple.pasteboard.promised-file-url",
    "NSFilenamesPboardType",
];

pub fn is_file_reference(native: &str) -> bool {
    FILE_REFERENCE_TYPES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(native))
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test channel::mime`
Expected: PASS, 7 tests.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/src/channel/mod.rs riabuild-cli/src/channel/mime.rs riabuild-cli/src/main.rs
git commit -m "feat(channel): translate between the three clipboard vocabularies

macOS names pasteboard types with UTIs, X11 with atoms that predate MIME,
and Wayland with MIME strings that are nearly the ones X11 uses. Without a
translation layer, text/html copied out of Safari exists under no name
xclip recognises.

File-reference types are named here so they can be dropped: a pasteboard
path is the one payload that is syntactically valid and semantically false
on the far side."
```

---

### Task 3: Target list filtering and preference order

`TARGETS` is not a passthrough of what the laptop reports. Callers commonly take the first match, so order is a functional decision; and a caller that walks the whole list can still choose a 40 MB TIFF for pixels already available as PNG, so redundancy has to be dropped rather than deprioritised.

**Files:**
- Modify: `riabuild-cli/src/channel/mime.rs`

**Interfaces:**
- Consumes: `Vocabulary`, `to_mime`, `is_file_reference`, `TEXT`/`HTML`/`PNG`/`TIFF` (Task 2)
- Produces: `pub fn normalise_targets(vocab: Vocabulary, native: &[String]) -> Vec<String>`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `riabuild-cli/src/channel/mime.rs`:

```rust
    fn targets(vocab: Vocabulary, native: &[&str]) -> Vec<String> {
        normalise_targets(vocab, &native.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// macOS puts a screenshot on the pasteboard as both PNG and uncompressed
    /// TIFF, and the TIFF can be 40 MB for pixels already available
    /// losslessly. Ordering alone is insufficient — a caller that walks the
    /// whole list can still choose it — so it is omitted entirely.
    #[test]
    fn tiff_is_dropped_when_png_is_present() {
        let list = targets(Vocabulary::MacOs, &["public.tiff", "public.png"]);
        assert_eq!(list, vec![PNG]);
    }

    /// TIFF is only redundant when PNG exists. On its own it is the content.
    #[test]
    fn tiff_survives_when_it_is_the_only_image() {
        let list = targets(Vocabulary::MacOs, &["public.tiff"]);
        assert_eq!(list, vec![TIFF]);
    }

    /// The first entry is what a caller with no type preference gets, and the
    /// spec fixes that as the preferred text flavour whenever text is present.
    #[test]
    fn text_leads_when_text_is_present() {
        let list = targets(
            Vocabulary::X11,
            &["image/png", "text/html", "UTF8_STRING"],
        );
        assert_eq!(list, vec![TEXT, HTML, PNG]);
    }

    #[test]
    fn png_leads_when_the_clipboard_holds_only_an_image() {
        let list = targets(Vocabulary::X11, &["image/tiff", "image/png"]);
        assert_eq!(list, vec![PNG]);
    }

    /// The strong form of the rule: no input, however shaped, produces a
    /// file-reference type on the wire.
    #[test]
    fn no_file_reference_type_survives_any_input() {
        let list = targets(
            Vocabulary::X11,
            &[
                "text/uri-list",
                "x-special/gnome-copied-files",
                "FILE_NAME",
                "UTF8_STRING",
            ],
        );
        assert_eq!(list, vec![TEXT]);

        // And on its own it leaves nothing at all, rather than an empty-ish
        // list the caller would treat as a usable clipboard.
        assert!(targets(Vocabulary::MacOs, &["public.file-url"]).is_empty());
    }

    /// X11 reports three atoms for one text flavour. They must collapse, or
    /// TARGETS lists the same content three times and a caller reads it three
    /// times.
    #[test]
    fn duplicate_spellings_collapse_to_one_entry() {
        let list = targets(
            Vocabulary::X11,
            &["UTF8_STRING", "STRING", "TEXT", "text/plain"],
        );
        assert_eq!(list, vec![TEXT]);
    }

    /// X11 always reports these two, and they are not content.
    #[test]
    fn unknown_and_meta_targets_are_dropped() {
        let list = targets(
            Vocabulary::X11,
            &["TARGETS", "MULTIPLE", "TIMESTAMP", "image/png"],
        );
        assert_eq!(list, vec![PNG]);
    }

    #[test]
    fn an_empty_clipboard_produces_an_empty_list() {
        assert!(targets(Vocabulary::X11, &[]).is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test channel::mime`
Expected: FAIL — `cannot find function normalise_targets`.

- [ ] **Step 3: Implement the filter**

Add to `riabuild-cli/src/channel/mime.rs`, after `is_file_reference`:

```rust
/// The order the channel reports types in.
///
/// This is a functional decision rather than cosmetics: callers commonly take
/// the first match, and a request with no type at all is served the first
/// entry. Text leads so that `xclip -o` on the server produces what selecting
/// the same clipboard on the laptop would.
const PREFERENCE: &[&str] = &[TEXT, HTML, PNG, TIFF];

/// What the laptop's clipboard reports → what the channel advertises.
///
/// Three rules, in order: unknown and file-reference types are dropped,
/// duplicate spellings collapse, and TIFF is omitted when PNG is present
/// because it is the same pixels at ten times the size.
pub fn normalise_targets(vocab: Vocabulary, native: &[String]) -> Vec<String> {
    let mut present: Vec<&'static str> = Vec::new();

    for name in native {
        let name = name.trim();
        if name.is_empty() || is_file_reference(name) {
            continue;
        }
        let Some(mime) = to_mime(vocab, name) else {
            continue;
        };
        if !present.contains(&mime) {
            present.push(mime);
        }
    }

    if present.contains(&PNG) {
        present.retain(|mime| *mime != TIFF);
    }

    PREFERENCE
        .iter()
        .filter(|mime| present.contains(mime))
        .map(|mime| (*mime).to_string())
        .collect()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test channel::mime`
Expected: PASS, 15 tests.

- [ ] **Step 5: Check formatting, lints, and the whole suite**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/src/channel/mime.rs
git commit -m "feat(channel): filter and order the advertised clipboard types

Callers commonly take the first match, so order is functional. Text leads
when text is present, which is what makes a request with no type produce
what the same clipboard would locally.

TIFF is dropped rather than deprioritised when PNG exists: macOS puts a
screenshot on the pasteboard as both, the TIFF can be 40 MB for pixels
already available losslessly, and a caller that walks the whole list can
still choose it. File references never survive, whatever the input."
```

---

### Task 4: The protocol and the compiled-in allowlist

Newline-delimited JSON requests; a JSON header line then raw bytes for binary responses. Responses are length-prefixed and streamed rather than base64-encoded, because a screenshot is routinely 2–15 MB and base64 would inflate it by a third for no benefit.

The allowlist is the security property, so it is an explicit `match` over strings rather than a serde-derived enum: "a server can request only what the laptop's binary already implements" should be one readable function, not an emergent consequence of derive attributes.

**Files:**
- Create: `riabuild-cli/src/channel/protocol.rs`
- Modify: `riabuild-cli/src/channel/mod.rs` (add `pub mod protocol;`)

**Interfaces:**
- Produces:
  - `pub const PROTOCOL_VERSION: u8 = 1;`
  - `pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;`
  - `pub enum Request { ClipboardTargets, ClipboardRead { mime: String }, ChannelPing }`
  - `pub enum Response { Targets(Vec<String>), Payload { len: usize }, Pong, Error { code: ErrorCode, message: String } }`
  - `pub enum ErrorCode { BadRequest, Unsupported, Unavailable, TooLarge, Internal }` with `pub fn as_str(&self) -> &'static str`
  - `pub fn encode_request(request: &Request) -> String` — includes the trailing newline
  - `pub fn decode_request(line: &str) -> Result<Request, ProtocolError>`
  - `pub fn encode_response(response: &Response) -> String` — includes the trailing newline
  - `pub fn decode_response(line: &str) -> Result<Response, ProtocolError>`
  - `pub enum ProtocolError { Malformed(String), UnknownOp(String), UnsupportedVersion(u8), MissingField(&'static str), TooLarge(usize) }` implementing `Display` and `std::error::Error`

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/channel/protocol.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_targets_request_is_one_json_line() {
        let line = encode_request(&Request::ClipboardTargets);
        assert_eq!(line, "{\"v\":1,\"op\":\"clipboard.targets\"}\n");
        assert_eq!(decode_request(&line).unwrap(), Request::ClipboardTargets);
    }

    #[test]
    fn a_read_request_carries_its_mime_type() {
        let request = Request::ClipboardRead {
            mime: "image/png".into(),
        };
        let line = encode_request(&request);
        assert!(line.contains("\"op\":\"clipboard.read\""), "{line}");
        assert!(line.contains("\"mime\":\"image/png\""), "{line}");
        assert_eq!(decode_request(&line).unwrap(), request);
    }

    #[test]
    fn every_request_ends_in_a_newline_so_the_reader_knows_where_it_stops() {
        for request in [
            Request::ClipboardTargets,
            Request::ChannelPing,
            Request::ClipboardRead { mime: "text/html".into() },
        ] {
            assert!(encode_request(&request).ends_with('\n'));
        }
    }

    /// The allowlist is the security property of the whole design: the server
    /// asks and the laptop decides. An op the binary does not implement is
    /// refused by name, not attempted.
    #[test]
    fn an_operation_outside_the_allowlist_is_refused() {
        let line = r#"{"v":1,"op":"clipboard.write","data":"x"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::UnknownOp(op)) if op == "clipboard.write"
        ));
    }

    #[test]
    fn shell_shaped_operations_are_refused_like_any_other_unknown_op() {
        for op in ["exec", "channel.exec", "clipboard.targets;rm -rf /"] {
            let line = format!(r#"{{"v":1,"op":"{op}"}}"#);
            assert!(
                matches!(decode_request(&line), Err(ProtocolError::UnknownOp(_))),
                "{op} was not refused"
            );
        }
    }

    #[test]
    fn a_future_protocol_version_is_refused_rather_than_guessed_at() {
        let line = r#"{"v":2,"op":"clipboard.targets"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn a_read_without_a_mime_type_is_a_missing_field_not_a_panic() {
        let line = r#"{"v":1,"op":"clipboard.read"}"#;
        assert!(matches!(
            decode_request(line),
            Err(ProtocolError::MissingField("mime"))
        ));
    }

    #[test]
    fn malformed_json_is_an_error_rather_than_a_crash() {
        for line in ["", "not json", "{\"v\":1", "[]", "null"] {
            assert!(
                matches!(decode_request(line), Err(ProtocolError::Malformed(_))),
                "{line:?} was not rejected"
            );
        }
    }

    #[test]
    fn a_targets_response_round_trips() {
        let response = Response::Targets(vec!["image/png".into(), "text/html".into()]);
        let line = encode_response(&response);
        assert!(line.contains("\"ok\":true"), "{line}");
        assert_eq!(decode_response(&line).unwrap(), response);
    }

    #[test]
    fn a_payload_response_announces_its_length_before_the_bytes() {
        let line = encode_response(&Response::Payload { len: 184_320 });
        assert_eq!(line, "{\"ok\":true,\"len\":184320}\n");
        assert_eq!(
            decode_response(&line).unwrap(),
            Response::Payload { len: 184_320 }
        );
    }

    #[test]
    fn an_error_response_round_trips_with_its_code() {
        let response = Response::Error {
            code: ErrorCode::Unavailable,
            message: "no clipboard content of that type".into(),
        };
        let line = encode_response(&response);
        assert!(line.contains("\"ok\":false"), "{line}");
        assert!(line.contains("\"code\":\"unavailable\""), "{line}");
        assert_eq!(decode_response(&line).unwrap(), response);
    }

    #[test]
    fn a_ping_is_answered_with_a_bare_ok() {
        let line = encode_response(&Response::Pong);
        assert_eq!(line, "{\"ok\":true}\n");
        assert_eq!(decode_response(&line).unwrap(), Response::Pong);
    }

    /// A length past the cap is refused at decode time, before anything sized
    /// by it is allocated. A malicious or broken peer must not be able to make
    /// the reader reserve 4 GB.
    #[test]
    fn a_payload_over_the_cap_is_refused_before_it_is_allocated() {
        let line = format!("{{\"ok\":true,\"len\":{}}}\n", MAX_PAYLOAD + 1);
        assert!(matches!(
            decode_response(&line),
            Err(ProtocolError::TooLarge(_))
        ));

        // Exactly the cap is allowed: the boundary belongs on the legal side.
        let line = format!("{{\"ok\":true,\"len\":{MAX_PAYLOAD}}}\n");
        assert_eq!(
            decode_response(&line).unwrap(),
            Response::Payload { len: MAX_PAYLOAD }
        );
    }

    #[test]
    fn the_cap_is_thirty_two_megabytes() {
        assert_eq!(MAX_PAYLOAD, 32 * 1024 * 1024);
    }

    /// Every error a caller can trigger must produce a message worth reading:
    /// these strings end up in the channel log, which is the only place a
    /// developer can find out why paste stopped working.
    #[test]
    fn every_protocol_error_describes_itself() {
        let errors = [
            ProtocolError::Malformed("bad".into()),
            ProtocolError::UnknownOp("clipboard.write".into()),
            ProtocolError::UnsupportedVersion(2),
            ProtocolError::MissingField("mime"),
            ProtocolError::TooLarge(99),
        ];
        for error in errors {
            let text = error.to_string();
            assert!(text.len() > 10, "{text:?} is not a useful message");
        }
    }
}
```

- [ ] **Step 2: Declare the module**

In `riabuild-cli/src/channel/mod.rs`, add below `pub mod mime;`:

```rust
pub mod protocol;
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test channel::protocol`
Expected: FAIL to compile — `cannot find type Request`, `cannot find function encode_request`.

- [ ] **Step 4: Write the protocol**

Prepend to `riabuild-cli/src/channel/protocol.rs`:

```rust
//! The wire format, and the operation allowlist.
//!
//! Requests are newline-delimited JSON. Responses are a JSON header line
//! followed, for binary payloads, by exactly the announced number of raw
//! bytes. Length-prefixed and streamed rather than base64: a screenshot is
//! routinely 2–15 MB, and base64 would inflate it by a third for no benefit.
//!
//! `decode_request` is deliberately an explicit `match` over operation names
//! rather than a serde-derived enum. The property it enforces — a server can
//! request only what the laptop's binary already implements — is the reason
//! the whole design is defensible, and it should be one readable function
//! rather than an emergent consequence of derive attributes.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u8 = 1;

/// The largest payload the channel will move, in either direction.
///
/// Refused at decode time, before anything sized by the announced length is
/// allocated, so a broken or hostile peer cannot make the reader reserve 4 GB.
pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    ClipboardTargets,
    ClipboardRead { mime: String },
    ChannelPing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Targets(Vec<String>),
    /// A header announcing `len` raw bytes to follow on the same stream.
    Payload { len: usize },
    Pong,
    Error { code: ErrorCode, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadRequest,
    Unsupported,
    Unavailable,
    TooLarge,
    Internal,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::Internal => "internal",
        }
    }

    fn parse(code: &str) -> ErrorCode {
        match code {
            "bad_request" => ErrorCode::BadRequest,
            "unsupported" => ErrorCode::Unsupported,
            "unavailable" => ErrorCode::Unavailable,
            "too_large" => ErrorCode::TooLarge,
            // An unrecognised code from a newer peer is still an error, and
            // treating it as one is more useful than failing to parse the
            // reply that says so.
            _ => ErrorCode::Internal,
        }
    }
}

#[derive(Debug)]
pub enum ProtocolError {
    Malformed(String),
    UnknownOp(String),
    UnsupportedVersion(u8),
    MissingField(&'static str),
    TooLarge(usize),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Malformed(detail) => {
                write!(f, "the channel received a line that is not valid JSON: {detail}")
            }
            ProtocolError::UnknownOp(op) => write!(
                f,
                "`{op}` is not an operation this riabuild implements; the operation set is compiled in"
            ),
            ProtocolError::UnsupportedVersion(v) => write!(
                f,
                "the channel speaks protocol version {PROTOCOL_VERSION}, and the peer asked for {v}"
            ),
            ProtocolError::MissingField(field) => {
                write!(f, "the request is missing its `{field}` field")
            }
            ProtocolError::TooLarge(len) => write!(
                f,
                "the payload is {len} bytes, over the {MAX_PAYLOAD} byte channel limit"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// The JSON shape of a request line. Parsed permissively, then narrowed by
/// `decode_request` into the compiled-in operation set.
#[derive(Debug, Serialize, Deserialize)]
struct RequestLine {
    v: u8,
    op: String,
    #[serde(default)]
    mime: Option<String>,
}

pub fn encode_request(request: &Request) -> String {
    let line = match request {
        Request::ClipboardTargets => RequestLine {
            v: PROTOCOL_VERSION,
            op: "clipboard.targets".into(),
            mime: None,
        },
        Request::ClipboardRead { mime } => RequestLine {
            v: PROTOCOL_VERSION,
            op: "clipboard.read".into(),
            mime: Some(mime.clone()),
        },
        Request::ChannelPing => RequestLine {
            v: PROTOCOL_VERSION,
            op: "channel.ping".into(),
            mime: None,
        },
    };
    // Serialising a struct of owned scalars cannot fail; the fallback keeps
    // the deny-by-default `unwrap_used` lint satisfied without ceremony.
    let json = serde_json::to_string(&line).unwrap_or_default();
    format!("{json}\n")
}

/// The allowlist.
///
/// Everything a server may ask for is named here. Anything else is refused by
/// name and never attempted.
pub fn decode_request(line: &str) -> Result<Request, ProtocolError> {
    let parsed: RequestLine = serde_json::from_str(line.trim())
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;

    if parsed.v != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(parsed.v));
    }

    match parsed.op.as_str() {
        "clipboard.targets" => Ok(Request::ClipboardTargets),
        "channel.ping" => Ok(Request::ChannelPing),
        "clipboard.read" => match parsed.mime {
            Some(mime) => Ok(Request::ClipboardRead { mime }),
            None => Err(ProtocolError::MissingField("mime")),
        },
        other => Err(ProtocolError::UnknownOp(other.to_string())),
    }
}

/// The JSON shape of a response header line.
#[derive(Debug, Serialize, Deserialize)]
struct ResponseLine {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    targets: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

pub fn encode_response(response: &Response) -> String {
    let line = match response {
        Response::Targets(targets) => ResponseLine {
            ok: true,
            targets: Some(targets.clone()),
            len: None,
            code: None,
            message: None,
        },
        Response::Payload { len } => ResponseLine {
            ok: true,
            targets: None,
            len: Some(*len),
            code: None,
            message: None,
        },
        Response::Pong => ResponseLine {
            ok: true,
            targets: None,
            len: None,
            code: None,
            message: None,
        },
        Response::Error { code, message } => ResponseLine {
            ok: false,
            targets: None,
            len: None,
            code: Some(code.as_str().to_string()),
            message: Some(message.clone()),
        },
    };
    let json = serde_json::to_string(&line).unwrap_or_default();
    format!("{json}\n")
}

pub fn decode_response(line: &str) -> Result<Response, ProtocolError> {
    let parsed: ResponseLine = serde_json::from_str(line.trim())
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;

    if !parsed.ok {
        return Ok(Response::Error {
            code: ErrorCode::parse(parsed.code.as_deref().unwrap_or("internal")),
            message: parsed.message.unwrap_or_else(|| "the laptop refused the request".into()),
        });
    }

    if let Some(targets) = parsed.targets {
        return Ok(Response::Targets(targets));
    }

    match parsed.len {
        // Checked here so the cap is enforced before a reader allocates by it.
        Some(len) if len > MAX_PAYLOAD => Err(ProtocolError::TooLarge(len)),
        Some(len) => Ok(Response::Payload { len }),
        None => Ok(Response::Pong),
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test channel::protocol`
Expected: PASS, 15 tests.

- [ ] **Step 6: Check formatting, lints, and the whole suite**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add riabuild-cli/src/channel/protocol.rs riabuild-cli/src/channel/mod.rs
git commit -m "feat(channel): the wire format and the compiled-in allowlist

Newline-delimited JSON requests; a header line then raw bytes for payloads,
because a screenshot is 2-15 MB and base64 would inflate it by a third.

decode_request is an explicit match over operation names rather than a
serde-derived enum. 'A server can request only what the laptop already
implements' is the property that makes a reverse tunnel defensible, so it
should be one readable function rather than a consequence of derives.

The 32 MB cap is enforced at decode time, before anything sized by the
announced length is allocated."
```

---

### Task 5: Reading a Linux laptop's clipboard

The `Clipboard` trait plus the X11 and Wayland backends. Everything goes through `CommandRunner`, which is what lets the whole channel be tested with no second machine anywhere.

Backend selection takes the session type as a *parameter* rather than reading `cfg!(target_os)`, per the platform rule — otherwise only the runner's own answer could ever be asserted.

**Files:**
- Create: `riabuild-cli/src/channel/clipboard.rs`
- Modify: `riabuild-cli/src/channel/mod.rs` (add `pub mod clipboard;`)

**Interfaces:**
- Consumes: `CommandRunner`, `RunOptions`, `BytesOutput` (Task 1); `mime::{Vocabulary, from_mime, normalise_targets}` (Tasks 2–3)
- Produces:
  - `#[async_trait] pub trait Clipboard: Send + Sync { async fn targets(&self) -> Result<Vec<String>>; async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>>; }`
  - `pub struct X11Clipboard { runner: Arc<dyn CommandRunner> }` with `pub fn new(runner: Arc<dyn CommandRunner>) -> Self`
  - `pub struct WaylandClipboard { runner: Arc<dyn CommandRunner> }` with the same constructor
  - `pub enum Session { X11, Wayland, MacOs }`
  - `pub fn detect(runner: &Arc<dyn CommandRunner>, os: &str, wayland_display: Option<&str>) -> Option<Session>`
  - `pub fn backend(runner: Arc<dyn CommandRunner>, session: Session) -> Box<dyn Clipboard>`

`read` returns `Ok(None)` for "the clipboard has no content of that type" and `Err` only for "the tool could not be run at all". That distinction is what lets the agent answer `unavailable` without treating an empty clipboard as a channel fault.

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/channel/clipboard.rs` with this test module:

```rust
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
        let runner = Arc::new(
            FakeRunner::new().with_bytes(
                "xclip -selection clipboard -t UTF8_STRING -o",
                0,
                b"hello",
                "",
            ),
        );
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
        // Text leads, and the three spellings of it collapse to one entry.
        assert_eq!(clipboard.targets().await.unwrap(), vec![TEXT, "text/html"]);
    }

    #[tokio::test]
    async fn wayland_reads_without_a_trailing_newline() {
        let runner = Arc::new(FakeRunner::new().with_bytes(
            "wl-paste -n -t image/png",
            0,
            b"\x89PNG",
            "",
        ));
        let clipboard = WaylandClipboard::new(runner.clone());
        assert_eq!(clipboard.read(PNG).await.unwrap(), Some(b"\x89PNG".to_vec()));
        // Without -n, wl-paste appends a newline, which corrupts every image.
        assert!(
            runner.calls().iter().any(|c| c.contains("-n")),
            "{:?}",
            runner.calls()
        );
    }

    #[test]
    fn a_wayland_session_is_detected_by_its_display_variable() {
        let runner = arc(FakeRunner::new().with("wl-paste --version", 0, "2.2.1", ""));
        assert_eq!(
            detect(&runner, "linux", Some("wayland-0")),
            Some(Session::Wayland)
        );
    }

    #[test]
    fn an_x11_session_falls_back_to_xclip() {
        let runner = arc(FakeRunner::new().with("xclip -version", 0, "xclip 0.13", ""));
        assert_eq!(detect(&runner, "linux", None), Some(Session::X11));
    }

    /// Naming the missing tool is the whole value of this branch: the failure
    /// otherwise reads as "paste does not work" with nothing to act on.
    #[test]
    fn a_linux_laptop_with_no_clipboard_tool_has_no_session() {
        let runner = arc(FakeRunner::new());
        assert_eq!(detect(&runner, "linux", Some("wayland-0")), None);
        assert_eq!(detect(&runner, "linux", None), None);
    }

    #[test]
    fn macos_needs_no_tool_check_because_osascript_always_exists() {
        let runner = arc(FakeRunner::new());
        assert_eq!(detect(&runner, "macos", None), Some(Session::MacOs));
    }
}
```

- [ ] **Step 2: Declare the module**

In `riabuild-cli/src/channel/mod.rs`, add `pub mod clipboard;` above `pub mod mime;`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test channel::clipboard`
Expected: FAIL to compile — `cannot find type X11Clipboard`.

- [ ] **Step 4: Write the trait and the Linux backends**

Prepend to `riabuild-cli/src/channel/clipboard.rs`:

```rust
//! Reading the laptop's clipboard.
//!
//! Every backend goes through `CommandRunner`, without exception. That is what
//! makes the whole channel testable with no server and no second machine
//! anywhere — a scripted `xclip` is indistinguishable from a real one.
//!
//! `read` separates "no content of that type" (`Ok(None)`) from "the tool
//! could not be run" (`Err`). An empty clipboard is the normal case and must
//! never be reported as a broken channel.

use crate::channel::mime::{self, Vocabulary};
use crate::runner::{CommandRunner, RunOptions};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait Clipboard: Send + Sync {
    /// The types currently on the clipboard, normalised, filtered and ordered.
    async fn targets(&self) -> Result<Vec<String>>;

    /// The bytes for one type, or `None` if the clipboard has no such content.
    async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    X11,
    Wayland,
    MacOs,
}

/// Which clipboard backend this laptop needs, if any.
///
/// The OS and the Wayland display are parameters rather than `cfg!` reads:
/// `cfg!` compiles every branch but one out of the test binary, so only the
/// runner's own platform could ever be asserted. `paths::default_project_dir_on`
/// is the same pattern.
pub fn detect(
    runner: &Arc<dyn CommandRunner>,
    os: &str,
    wayland_display: Option<&str>,
) -> Option<Session> {
    if os == "macos" {
        // osascript ships with every macOS and needs no check.
        return Some(Session::MacOs);
    }

    let wayland = wayland_display.is_some_and(|display| !display.is_empty());
    if wayland && runner.which("wl-paste").is_some() {
        return Some(Session::Wayland);
    }
    if runner.which("xclip").is_some() {
        return Some(Session::X11);
    }
    // A Wayland laptop can still drive xclip through XWayland, so the X11
    // branch above is tried either way before giving up.
    None
}

pub fn backend(runner: Arc<dyn CommandRunner>, session: Session) -> Box<dyn Clipboard> {
    match session {
        Session::X11 => Box::new(X11Clipboard::new(runner)),
        Session::Wayland => Box::new(WaylandClipboard::new(runner)),
        Session::MacOs => Box::new(MacOsClipboard::new(runner)),
    }
}

/// The install command for a Linux laptop that has neither tool.
///
/// Named rather than described, the way `mosh-server` already is: "no
/// clipboard tool" is not something a developer can act on.
pub fn install_hint(wayland: bool) -> &'static str {
    if wayland {
        "install wl-clipboard (apt install wl-clipboard, dnf install wl-clipboard)"
    } else {
        "install xclip (apt install xclip, dnf install xclip)"
    }
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
```

- [ ] **Step 5: Stub the macOS backend so the module compiles**

`backend` names `MacOsClipboard`, which Task 6 implements. Add a stub now so this task compiles and its tests run:

```rust
pub struct MacOsClipboard {
    #[allow(dead_code)]
    runner: Arc<dyn CommandRunner>,
}

impl MacOsClipboard {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl Clipboard for MacOsClipboard {
    async fn targets(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn read(&self, _mime: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test channel::clipboard`
Expected: PASS, 12 tests.

- [ ] **Step 7: Check formatting, lints, and the whole suite**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add riabuild-cli/src/channel/clipboard.rs riabuild-cli/src/channel/mod.rs
git commit -m "feat(channel): read an X11 or Wayland laptop's clipboard

Both backends go through CommandRunner, which is what makes the channel
testable with no second machine anywhere.

read() separates 'no content of that type' from 'the tool could not run'.
xclip exits non-zero on an empty clipboard, and conflating the two would
report a broken channel every time nothing was copied.

wl-paste is always given -n: without it the tool appends a newline, which
corrupts a PNG and silently changes a string."
```

---

### Task 6: Reading a macOS laptop's pasteboard

The primary case, and the one with no clean CLI. `pbpaste` cannot enumerate pasteboard types and cannot emit binary for an arbitrary one. AppleScript can do both: `clipboard info` lists `{«class PNGf», 12345}` pairs, and `the clipboard as «class PNGf»` returns `«data PNGf89504E47…»` — hex text, which decodes to the original bytes and travels safely through `CommandRunner::run`.

`pbpaste` is still used for plain text, where it is exact and avoids AppleScript's string mangling.

**Files:**
- Modify: `riabuild-cli/src/channel/clipboard.rs` (replace the Task 5 stub)

**Interfaces:**
- Produces: a real `impl Clipboard for MacOsClipboard`; `fn class_for(mime: &str) -> Option<&'static str>`; `fn decode_applescript_data(raw: &str, class: &str) -> Option<Vec<u8>>`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `riabuild-cli/src/channel/clipboard.rs`:

```rust
    const CLIPBOARD_INFO: &str = "osascript -e clipboard info";

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
        let runner = arc(FakeRunner::new().with(
            CLIPBOARD_INFO,
            0,
            "«class PNGf», 4",
            "",
        ).with(
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

    #[test]
    fn a_reply_that_is_not_a_data_envelope_decodes_to_nothing() {
        for raw in ["", "missing value", "«data PNGf8950", "«data TIFF89504E47»"] {
            assert_eq!(decode_applescript_data(raw, "PNGf"), None, "{raw:?}");
        }
    }

    #[test]
    fn every_carried_mime_type_has_a_pasteboard_class() {
        assert_eq!(class_for(PNG), Some("PNGf"));
        assert_eq!(class_for(TEXT), Some("utf8"));
        assert_eq!(class_for("application/pdf"), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test channel::clipboard::tests::macos`
Expected: FAIL — `cannot find function decode_applescript_data`, and the targets tests return empty from the stub.

- [ ] **Step 3: Replace the stub**

Delete the `MacOsClipboard` stub from Task 5 and put this in its place:

```rust
/// The macOS pasteboard, read through AppleScript.
///
/// `pbpaste` cannot enumerate pasteboard types and cannot emit binary for an
/// arbitrary one, so it is not sufficient on its own. AppleScript can do both:
/// `clipboard info` lists `{«class PNGf», 184320}` pairs, and `the clipboard as
/// «class PNGf»` returns `«data PNGf89504E47…»`, a hex envelope that decodes to
/// the original bytes and travels safely as text through `CommandRunner`.
///
/// Text still goes through `pbpaste`, which is exact. AppleScript's string
/// handling rewrites line endings and mangles anything non-ASCII.
pub struct MacOsClipboard {
    runner: Arc<dyn CommandRunner>,
}

impl MacOsClipboard {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
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
/// for a class the pasteboard lacks by coercing to something else, and
/// decoding that as if it were the requested type produces a corrupt file
/// rather than a clean miss.
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

impl MacOsClipboard {
    async fn osascript(&self, script: &str) -> Result<Option<String>> {
        let output = self
            .runner
            .run("osascript", &["-e", script], &RunOptions::default())
            .await
            .context("could not read the pasteboard with osascript")?;
        Ok(output.ok().then(|| output.stdout.clone()))
    }
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
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test channel::clipboard`
Expected: PASS, 19 tests.

- [ ] **Step 5: Check formatting, lints, and the whole suite**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green.

- [ ] **Step 6: Verify against a real pasteboard**

This is the one backend whose contract is a guess about another program's output format, so it gets a smoke test in the shape of `shims::tests::claude_config_dir_smoke`. Add to the test module:

```rust
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
```

- [ ] **Step 7: Commit**

```bash
git add riabuild-cli/src/channel/clipboard.rs
git commit -m "feat(channel): read a macOS laptop's pasteboard

pbpaste can neither enumerate pasteboard types nor emit binary for an
arbitrary one, so AppleScript does both: clipboard info for the class list,
and a «data PNGf…» hex envelope for the bytes, which travels safely as text
through CommandRunner.

The class tag in that envelope is checked rather than skipped. AppleScript
answers a request for a class the pasteboard lacks by coercing to something
else, and decoding that as the requested type yields a corrupt file rather
than a clean miss.

Text still goes through pbpaste, which is exact; AppleScript's string
handling rewrites line endings and mangles non-ASCII."
```

---

### Task 7: The long-edge ceiling

The only transform the channel applies. Images at or below the ceiling are passed through byte-for-byte and never decoded; only an oversized image enters the resize path.

Add to `riabuild-cli/Cargo.toml` under `[dependencies]`:

```toml
# PNG and TIFF decode, PNG encode, for the MAX_LONG_EDGE ceiling. Default
# features pull in a dozen codecs riabuild never sees on a pasteboard.
image = { version = "0.25", default-features = false, features = ["png", "tiff"] }
```

**Files:**
- Create: `riabuild-cli/src/channel/resize.rs`
- Modify: `riabuild-cli/src/channel/mod.rs`, `riabuild-cli/Cargo.toml`

**Interfaces:**
- Produces: `pub const MAX_LONG_EDGE: u32 = 2576;` and `pub fn to_ceiling(mime: &str, bytes: Vec<u8>) -> Vec<u8>`

`to_ceiling` never fails: an image it cannot decode is returned untouched. A clipboard bridge that refuses to carry an image because it could not parse it is worse than one that carries it whole.

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/channel/resize.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbaImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("encode png");
        bytes.into_inner()
    }

    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        image::load_from_memory(bytes)
            .map(|image| (image.width(), image.height()))
            .expect("decode png")
    }

    #[test]
    fn the_ceiling_is_the_models_own_long_edge_limit() {
        assert_eq!(MAX_LONG_EDGE, 2576);
    }

    /// The common case. Decoding and re-encoding a screenshot that is already
    /// under the ceiling would change its bytes for no gain.
    #[test]
    fn an_image_under_the_ceiling_is_returned_byte_for_byte() {
        let original = png(800, 600);
        let out = to_ceiling("image/png", original.clone());
        assert_eq!(out, original);
    }

    #[test]
    fn an_image_exactly_at_the_ceiling_is_untouched() {
        let original = png(MAX_LONG_EDGE, 100);
        let out = to_ceiling("image/png", original.clone());
        assert_eq!(out, original);
    }

    #[test]
    fn a_wide_image_is_scaled_to_the_ceiling_on_its_long_edge() {
        let out = to_ceiling("image/png", png(5152, 2576));
        assert_eq!(dimensions(&out), (MAX_LONG_EDGE, 1288));
    }

    #[test]
    fn a_tall_image_is_scaled_on_its_long_edge_too() {
        let out = to_ceiling("image/png", png(1288, 5152));
        assert_eq!(dimensions(&out), (644, MAX_LONG_EDGE));
    }

    #[test]
    fn an_oversized_image_gets_smaller_on_the_wire() {
        let original = png(6000, 4000);
        let out = to_ceiling("image/png", original.clone());
        assert!(out.len() < original.len(), "{} -> {}", original.len(), out.len());
    }

    /// Text is never an image, whatever its bytes look like.
    #[test]
    fn non_image_types_are_never_decoded() {
        let text = b"long edge 9999".to_vec();
        assert_eq!(to_ceiling("text/plain;charset=utf-8", text.clone()), text);
        assert_eq!(to_ceiling("text/html", text.clone()), text);
    }

    /// Carrying an image whole is better than refusing to carry it because a
    /// decoder did not recognise it.
    #[test]
    fn an_undecodable_image_is_passed_through_rather_than_dropped() {
        let junk = b"\x89PNG not really a png".to_vec();
        assert_eq!(to_ceiling("image/png", junk.clone()), junk);
    }

    /// An oversized TIFF is re-encoded as PNG, which is what the far side is
    /// told it is getting — so the transform must be lossless in format terms.
    #[test]
    fn an_oversized_image_re_encodes_to_something_still_decodable() {
        let out = to_ceiling("image/png", png(4000, 3000));
        assert_eq!(image::guess_format(&out).unwrap(), ImageFormat::Png);
    }
}
```

- [ ] **Step 2: Declare the module and add the dependency**

Add `pub mod resize;` to `riabuild-cli/src/channel/mod.rs` and the `image` line to `Cargo.toml` as above.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test channel::resize`
Expected: FAIL — `cannot find function to_ceiling`.

- [ ] **Step 4: Implement the ceiling**

Prepend to `riabuild-cli/src/channel/resize.rs`:

```rust
//! The one transform the channel applies.
//!
//! Claude's vision resizes anything above this long edge and discards the
//! excess, so sending a 5K screenshot uncompressed costs several times the
//! transfer time and yields the model no additional pixels. Resizing *to* the
//! ceiling loses nothing the model would have seen; resizing below it would,
//! which is why the constant sits at the ceiling and not under it.
//!
//! This is not a setting. There is no config key, no environment variable, and
//! no dashboard field — riabuild does not ask a developer to pick a resolution
//! any more than it asks them to pick a Node version. Changing the number is a
//! release.
//!
//! It belongs here, on the laptop, rather than in the shim: the shim runs after
//! the bytes have already crossed the wire, so resizing there would save tokens
//! but not transfer time, which is the whole point.

use image::ImageFormat;
use image::imageops::FilterType;

/// The long-edge ceiling Claude's vision applies before it looks at an image.
/// Resizing to this is information-neutral: the detail discarded here is the
/// detail the model was going to discard anyway.
pub const MAX_LONG_EDGE: u32 = 2576;

/// Brings an oversized image down to the ceiling. Everything else is returned
/// untouched.
///
/// Never fails. An image that cannot be decoded is passed through whole — a
/// clipboard bridge that refuses to carry a picture because a decoder did not
/// recognise it is worse than one that carries it at full size.
pub fn to_ceiling(mime: &str, bytes: Vec<u8>) -> Vec<u8> {
    if !mime.starts_with("image/") {
        return bytes;
    }

    let Ok(image) = image::load_from_memory(&bytes) else {
        return bytes;
    };

    let long_edge = image.width().max(image.height());
    if long_edge <= MAX_LONG_EDGE {
        // The common case, and the reason this is a cheap check rather than an
        // unconditional re-encode: a screenshot already under the ceiling
        // crosses the wire as the exact bytes the pasteboard held.
        return bytes;
    }

    // `thumbnail` is fast but visibly soft on text, and a dense error dialog is
    // the paste this feature exists for. Lanczos3 keeps small type legible.
    let scaled = image.resize(MAX_LONG_EDGE, MAX_LONG_EDGE, FilterType::Lanczos3);

    let mut out = std::io::Cursor::new(Vec::new());
    match scaled.write_to(&mut out, ImageFormat::Png) {
        Ok(()) => out.into_inner(),
        Err(_) => bytes,
    }
}
```

- [ ] **Step 5: Run the tests, then the whole suite**

Run: `cd riabuild-cli && cargo test channel::resize`
Expected: PASS, 9 tests.

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/src/channel/resize.rs riabuild-cli/src/channel/mod.rs riabuild-cli/Cargo.toml
git commit -m "feat(channel): bring oversized images down to the model's ceiling

Claude's vision resizes anything above 2576px on the long edge and discards
the excess, so a 5K screenshot costs several times the transfer time for no
additional pixels. Resizing to the ceiling loses nothing the model would
have seen; resizing below it would.

Images at or under the ceiling are never decoded and cross byte-for-byte.
An image that fails to decode is passed through whole rather than dropped."
```

---

### Task 8: The laptop agent

The unix socket server: accept, read one request line, dispatch, reply. Plus the snapshot cache, which is what makes a paste atomic — a paste is two round trips, and if the clipboard changes between `TARGETS` and the read, the second call finds nothing and the paste fails for no visible reason.

Time is injected as a parameter rather than read from the clock, so the cache expiry is asserted rather than slept through.

**Files:**
- Create: `riabuild-cli/src/channel/agent.rs`
- Modify: `riabuild-cli/src/channel/mod.rs`

**Interfaces:**
- Consumes: `Clipboard` (Tasks 5–6), `protocol::*` (Task 4), `resize::to_ceiling` (Task 7)
- Produces:
  - `pub const SNAPSHOT_TTL: Duration = Duration::from_secs(5);`
  - `pub struct Agent { clipboard: Box<dyn Clipboard>, snapshot: Mutex<Option<Snapshot>> }`
  - `impl Agent { pub fn new(clipboard: Box<dyn Clipboard>) -> Self; pub async fn handle(&self, request: &Request, now: Instant) -> (Response, Option<Vec<u8>>); pub async fn serve(self: Arc<Self>, socket: &Path) -> Result<()> }`

`handle` returns the header and the optional body separately so the socket layer writes them in order and the tests never touch a socket.

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/channel/agent.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::mime::{PNG, TEXT};
    // Only the socket test needs these, so they are imported here rather than
    // at module level, where they would be unused in the shipped build and
    // `-D warnings` would reject them.
    use crate::channel::protocol::{decode_response, encode_request};
    use std::sync::Mutex as StdMutex;

    /// A clipboard whose contents the test can change between calls, which is
    /// the whole point of the snapshot.
    struct FakeClipboard {
        types: StdMutex<Vec<String>>,
        bytes: StdMutex<Vec<u8>>,
    }

    impl FakeClipboard {
        fn holding(types: &[&str], bytes: &[u8]) -> Arc<Self> {
            Arc::new(Self {
                types: StdMutex::new(types.iter().map(|t| t.to_string()).collect()),
                bytes: StdMutex::new(bytes.to_vec()),
            })
        }

        fn becomes_empty(&self) {
            self.types.lock().expect("lock").clear();
            self.bytes.lock().expect("lock").clear();
        }
    }

    #[async_trait]
    impl Clipboard for FakeClipboard {
        async fn targets(&self) -> Result<Vec<String>> {
            Ok(self.types.lock().expect("lock").clone())
        }
        async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>> {
            let held = self.types.lock().expect("lock").clone();
            if !held.iter().any(|t| t == mime) {
                return Ok(None);
            }
            Ok(Some(self.bytes.lock().expect("lock").clone()))
        }
    }

    fn agent(clipboard: Arc<FakeClipboard>) -> Agent {
        Agent::new(Box::new(ClipboardHandle(clipboard)))
    }

    /// Lets one fake back both the trait object the agent owns and the handle
    /// the test mutates.
    struct ClipboardHandle(Arc<FakeClipboard>);

    #[async_trait]
    impl Clipboard for ClipboardHandle {
        async fn targets(&self) -> Result<Vec<String>> {
            self.0.targets().await
        }
        async fn read(&self, mime: &str) -> Result<Option<Vec<u8>>> {
            self.0.read(mime).await
        }
    }

    #[tokio::test]
    async fn a_ping_is_answered_without_touching_the_clipboard() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let (response, body) = agent.handle(&Request::ChannelPing, Instant::now()).await;
        assert_eq!(response, Response::Pong);
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn targets_are_reported_from_the_clipboard() {
        let agent = agent(FakeClipboard::holding(&[PNG], b"\x89PNG"));
        let (response, _) = agent.handle(&Request::ClipboardTargets, Instant::now()).await;
        assert_eq!(response, Response::Targets(vec![PNG.to_string()]));
    }

    #[tokio::test]
    async fn a_read_returns_a_length_header_and_the_bytes() {
        let agent = agent(FakeClipboard::holding(&[PNG], b"\x89PNG"));
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, Instant::now()).await;
        assert_eq!(response, Response::Payload { len: 4 });
        assert_eq!(body, Some(b"\x89PNG".to_vec()));
    }

    /// The two-call race. A paste is TARGETS then read; if the clipboard
    /// changes in between, the read must still serve what was advertised or
    /// the paste fails for no visible reason.
    #[tokio::test]
    async fn a_read_is_served_from_the_snapshot_when_the_clipboard_has_moved_on() {
        let clipboard = FakeClipboard::holding(&[PNG], b"\x89PNG");
        let agent = agent(clipboard.clone());
        let now = Instant::now();

        agent.handle(&Request::ClipboardTargets, now).await;
        clipboard.becomes_empty();

        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, now).await;
        assert_eq!(response, Response::Payload { len: 4 });
        assert_eq!(body, Some(b"\x89PNG".to_vec()));
    }

    /// The snapshot is for one paste, not a cache. A read long after the
    /// advertisement must see the real clipboard.
    #[tokio::test]
    async fn the_snapshot_expires() {
        let clipboard = FakeClipboard::holding(&[PNG], b"\x89PNG");
        let agent = agent(clipboard.clone());
        let now = Instant::now();

        agent.handle(&Request::ClipboardTargets, now).await;
        clipboard.becomes_empty();

        let request = Request::ClipboardRead { mime: PNG.into() };
        let later = now + SNAPSHOT_TTL + Duration::from_secs(1);
        let (response, body) = agent.handle(&request, later).await;
        assert!(matches!(response, Response::Error { .. }), "{response:?}");
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn a_genuinely_empty_clipboard_is_unavailable_rather_than_a_fault() {
        let agent = agent(FakeClipboard::holding(&[], b""));
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, Instant::now()).await;
        assert!(
            matches!(&response, Response::Error { code, .. } if *code == ErrorCode::Unavailable),
            "{response:?}"
        );
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn a_payload_over_the_cap_is_refused_with_the_limit_named() {
        let huge = vec![0u8; MAX_PAYLOAD + 1];
        let agent = agent(FakeClipboard::holding(&[PNG], &huge));
        let request = Request::ClipboardRead { mime: PNG.into() };
        let (response, body) = agent.handle(&request, Instant::now()).await;
        let Response::Error { code, message } = response else {
            panic!("expected an error, got {response:?}");
        };
        assert_eq!(code, ErrorCode::TooLarge);
        assert!(message.contains("image/png"), "{message}");
        assert!(body.is_none());
    }

    /// End to end over a real socket, which is the only way to know the
    /// framing and the socket layer agree.
    #[tokio::test]
    async fn the_agent_answers_over_a_real_unix_socket() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("channel.sock");

        let agent = Arc::new(agent(FakeClipboard::holding(&[TEXT], b"hello")));
        let serving = tokio::spawn({
            let socket = socket.clone();
            async move { agent.serve(&socket).await }
        });

        // Wait for the listener rather than sleeping a fixed interval.
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("connect");
        stream
            .write_all(encode_request(&Request::ClipboardTargets).as_bytes())
            .await
            .expect("write");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read");
        assert_eq!(
            decode_response(&line).expect("decode"),
            Response::Targets(vec![TEXT.to_string()])
        );

        serving.abort();
    }
}
```

- [ ] **Step 2: Declare the module and run the tests to verify they fail**

Add `pub mod agent;` to `riabuild-cli/src/channel/mod.rs`.

Run: `cd riabuild-cli && cargo test channel::agent`
Expected: FAIL to compile — `cannot find type Agent`.

- [ ] **Step 3: Write the agent**

Prepend to `riabuild-cli/src/channel/agent.rs`:

```rust
//! The laptop side: answer requests, decide what to serve.
//!
//! One connection carries one request and one response. The socket is
//! request-scoped rather than session-scoped so a wedged reader cannot hold the
//! channel, and so the supervisor's ping is a real end-to-end probe rather than
//! a check on a socket that is merely still open.

use crate::channel::clipboard::Clipboard;
use crate::channel::protocol::{ErrorCode, MAX_PAYLOAD, Request, Response, decode_request, encode_response};
use crate::channel::resize;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

/// How long a `TARGETS` answer stays good for the read that follows it.
///
/// A paste is two round trips. Long enough to cover a slow link, short enough
/// that this is a snapshot for one paste rather than a cache of the clipboard.
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(5);

struct Snapshot {
    taken: Instant,
    types: Vec<String>,
    /// Filled lazily: `TARGETS` records what was advertised, and the read that
    /// follows it stores the bytes it fetched under that advertisement.
    content: Vec<(String, Vec<u8>)>,
}

pub struct Agent {
    clipboard: Box<dyn Clipboard>,
    snapshot: Mutex<Option<Snapshot>>,
}

impl Agent {
    pub fn new(clipboard: Box<dyn Clipboard>) -> Self {
        Self {
            clipboard,
            snapshot: Mutex::new(None),
        }
    }

    /// Answers one request. The body is returned beside the header rather than
    /// written, so every dispatch decision is testable without a socket.
    pub async fn handle(&self, request: &Request, now: Instant) -> (Response, Option<Vec<u8>>) {
        match request {
            Request::ChannelPing => (Response::Pong, None),
            Request::ClipboardTargets => self.targets(now).await,
            Request::ClipboardRead { mime } => self.read(mime, now).await,
        }
    }

    async fn targets(&self, now: Instant) -> (Response, Option<Vec<u8>>) {
        let types = match self.clipboard.targets().await {
            Ok(types) => types,
            Err(error) => {
                return (
                    Response::Error {
                        code: ErrorCode::Internal,
                        message: format!("could not read the laptop's clipboard: {error}"),
                    },
                    None,
                );
            }
        };

        *self.snapshot.lock().await = Some(Snapshot {
            taken: now,
            types: types.clone(),
            content: Vec::new(),
        });

        (Response::Targets(types), None)
    }

    async fn read(&self, mime: &str, now: Instant) -> (Response, Option<Vec<u8>>) {
        let mut snapshot = self.snapshot.lock().await;

        // Expire first, so a stale snapshot never answers.
        if snapshot
            .as_ref()
            .is_some_and(|s| now.duration_since(s.taken) > SNAPSHOT_TTL)
        {
            *snapshot = None;
        }

        if let Some(held) = snapshot.as_ref() {
            if let Some((_, bytes)) = held.content.iter().find(|(t, _)| t == mime) {
                return payload(mime, bytes.clone());
            }
        }

        let fetched = match self.clipboard.read(mime).await {
            Ok(Some(bytes)) => Some(bytes),
            Ok(None) => None,
            Err(error) => {
                return (
                    Response::Error {
                        code: ErrorCode::Internal,
                        message: format!("could not read the laptop's clipboard: {error}"),
                    },
                    None,
                );
            }
        };

        // The clipboard moved between the advertisement and the read, but the
        // type was advertised — the caller is mid-paste and must not be told
        // the clipboard is empty.
        let bytes = match fetched {
            Some(bytes) => bytes,
            None => {
                let advertised = snapshot
                    .as_ref()
                    .is_some_and(|held| held.types.iter().any(|t| t == mime));
                let message = if advertised {
                    format!("the clipboard changed while `{mime}` was being read")
                } else {
                    "no clipboard content of that type".to_string()
                };
                return (
                    Response::Error {
                        code: ErrorCode::Unavailable,
                        message,
                    },
                    None,
                );
            }
        };

        let bytes = resize::to_ceiling(mime, bytes);

        if let Some(held) = snapshot.as_mut() {
            held.content.push((mime.to_string(), bytes.clone()));
        }

        payload(mime, bytes)
    }

    /// Accepts connections until cancelled. One request per connection.
    pub async fn serve(self: Arc<Self>, socket: &Path) -> Result<()> {
        if let Some(parent) = socket.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        // A socket left by a killed agent blocks the bind, and the channel
        // comes up permanently dead. Ours is removed; the supervisor is what
        // refuses one owned by another uid on the server side.
        let _ = tokio::fs::remove_file(socket).await;

        let listener = UnixListener::bind(socket)
            .with_context(|| format!("could not listen on {}", socket.display()))?;

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .context("the channel socket stopped accepting connections")?;
            let agent = Arc::clone(&self);
            // Serving inline would let one slow clipboard read block every
            // other shell into the same server.
            tokio::task::spawn_local(async move {
                let _ = agent.serve_one(stream).await;
            });
        }
    }

    async fn serve_one(&self, stream: UnixStream) -> Result<()> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let (header, body) = match decode_request(&line) {
            Ok(request) => self.handle(&request, Instant::now()).await,
            Err(error) => (
                Response::Error {
                    code: ErrorCode::BadRequest,
                    message: error.to_string(),
                },
                None,
            ),
        };

        let stream = reader.get_mut();
        stream.write_all(encode_response(&header).as_bytes()).await?;
        if let Some(bytes) = body {
            stream.write_all(&bytes).await?;
        }
        stream.flush().await?;
        Ok(())
    }
}

fn payload(mime: &str, bytes: Vec<u8>) -> (Response, Option<Vec<u8>>) {
    if bytes.len() > MAX_PAYLOAD {
        return (
            Response::Error {
                code: ErrorCode::TooLarge,
                message: format!(
                    "`{mime}` is {} bytes, over the {MAX_PAYLOAD} byte channel limit",
                    bytes.len()
                ),
            },
            None,
        );
    }
    (Response::Payload { len: bytes.len() }, Some(bytes))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test channel::agent`
Expected: PASS, 8 tests.

If `spawn_local` fails outside a `LocalSet`, replace it with a direct `agent.serve_one(stream).await` and note the serialisation; the runtime is current-thread, so a `LocalSet` has to be established by the caller in Task 13.

- [ ] **Step 5: Check formatting, lints, and the whole suite, then commit**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add riabuild-cli/src/channel/agent.rs riabuild-cli/src/channel/mod.rs
git commit -m "feat(channel): serve clipboard requests from the laptop

One request per connection, so a wedged reader cannot hold the channel and
the supervisor's ping is a real end-to-end probe.

The snapshot is what makes a paste atomic. A paste is two round trips, and
without it a clipboard that changes between TARGETS and the read makes the
paste fail for no visible reason. It holds for five seconds: long enough for
a slow link, short enough to be a snapshot for one paste rather than a cache."
```

---

### Task 9: The server-side client

Connect, send one request, read one response. This is what the shim calls, and its whole contract is that it never hangs: a laptop that has gone away must produce a fast, clean failure rather than a Claude Code session that stops responding to Ctrl+V.

**Files:**
- Create: `riabuild-cli/src/channel/client.rs`
- Modify: `riabuild-cli/src/channel/mod.rs`

**Interfaces:**
- Produces:
  - `pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);`
  - `pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);`
  - `pub struct Reply { pub response: Response, pub body: Vec<u8> }`
  - `pub async fn request(socket: &Path, request: &Request) -> Result<Reply>`

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/channel/client.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::protocol::{ErrorCode, encode_response};

    /// A scripted agent: one connection, one canned reply.
    async fn serve(socket: &Path, header: Response, body: &'static [u8]) {
        let listener = tokio::net::UnixListener::bind(socket).expect("bind");
        let header = encode_response(&header);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                let mut reader = BufReader::new(&mut stream);
                let mut line = String::new();
                let _ = reader.read_line(&mut line).await;
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body).await;
                let _ = stream.flush().await;
            }
        });
    }

    #[tokio::test]
    async fn a_targets_request_returns_the_list() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(
            &socket,
            Response::Targets(vec!["image/png".into()]),
            b"",
        )
        .await;

        let reply = request(&socket, &Request::ClipboardTargets)
            .await
            .expect("request");
        assert_eq!(
            reply.response,
            Response::Targets(vec!["image/png".to_string()])
        );
        assert!(reply.body.is_empty());
    }

    /// The length prefix is a contract: read exactly that many bytes, not
    /// "until the peer closes". A short read here is a truncated screenshot.
    #[tokio::test]
    async fn a_payload_reply_reads_exactly_the_announced_bytes() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(&socket, Response::Payload { len: 4 }, b"\x89PNGtrailing junk").await;

        let reply = request(
            &socket,
            &Request::ClipboardRead {
                mime: "image/png".into(),
            },
        )
        .await
        .expect("request");
        assert_eq!(reply.body, b"\x89PNG");
    }

    #[tokio::test]
    async fn an_error_reply_is_returned_rather_than_raised() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(
            &socket,
            Response::Error {
                code: ErrorCode::Unavailable,
                message: "no clipboard content of that type".into(),
            },
            b"",
        )
        .await;

        let reply = request(&socket, &Request::ClipboardTargets)
            .await
            .expect("request");
        assert!(matches!(reply.response, Response::Error { .. }));
    }

    /// The laptop is gone. This must fail fast and legibly, because the
    /// alternative is Claude Code hanging on Ctrl+V.
    #[tokio::test]
    async fn a_missing_socket_is_an_error_not_a_hang() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let error = request(&dir.path().join("absent.sock"), &Request::ChannelPing)
            .await
            .expect_err("should fail");
        assert!(
            error.to_string().contains("channel"),
            "{error} does not mention the channel"
        );
    }

    /// A truncated body must not be returned as if it were complete: a
    /// half-written PNG that Claude Code accepts is worse than a clean miss.
    #[tokio::test]
    async fn a_body_shorter_than_its_header_promised_is_an_error() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let socket = dir.path().join("c.sock");
        serve(&socket, Response::Payload { len: 64 }, b"short").await;

        let result = request(
            &socket,
            &Request::ClipboardRead {
                mime: "image/png".into(),
            },
        )
        .await;
        assert!(result.is_err(), "a short body was accepted");
    }
}
```

- [ ] **Step 2: Declare the module and run the tests to verify they fail**

Add `pub mod client;` to `riabuild-cli/src/channel/mod.rs`.

Run: `cd riabuild-cli && cargo test channel::client`
Expected: FAIL — `cannot find function request`.

- [ ] **Step 3: Write the client**

Prepend to `riabuild-cli/src/channel/client.rs`:

```rust
//! The server side: connect, ask once, read once.
//!
//! Everything here is in the paste path, so the contract is that it never
//! hangs. A laptop that has closed its lid must produce a fast, clean failure
//! — the alternative is Claude Code stopping dead on Ctrl+V, which reads as
//! the editor being broken rather than the channel being down.

use crate::channel::protocol::{Request, Response, decode_response, encode_request};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// The socket is local — a forwarded one either answers immediately or is not
/// there at all — so this only has to cover scheduling, not a network.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Covers the round trip to the laptop and the transfer. Generous, because a
/// 15 MB screenshot over a hotel connection is a legitimate slow case.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Reply {
    pub response: Response,
    pub body: Vec<u8>,
}

pub async fn request(socket: &Path, request: &Request) -> Result<Reply> {
    let connect = UnixStream::connect(socket);
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .with_context(|| {
            format!(
                "the laptop channel at {} did not accept a connection",
                socket.display()
            )
        })?
        .with_context(|| {
            format!(
                "the laptop channel at {} is not available",
                socket.display()
            )
        })?;

    tokio::time::timeout(REQUEST_TIMEOUT, exchange(stream, request))
        .await
        .context("the laptop channel did not answer in time")?
}

async fn exchange(mut stream: UnixStream, request: &Request) -> Result<Reply> {
    stream
        .write_all(encode_request(request).as_bytes())
        .await
        .context("could not send the request to the laptop channel")?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("the laptop channel closed before replying")?;
    if line.trim().is_empty() {
        bail!("the laptop channel replied with nothing");
    }

    let response = decode_response(&line)?;

    let body = match &response {
        Response::Payload { len } => {
            // Exactly the announced length, never "until close": a short read
            // here is a truncated screenshot that Claude Code would accept.
            let mut buffer = vec![0u8; *len];
            reader
                .read_exact(&mut buffer)
                .await
                .context("the laptop channel sent fewer bytes than it announced")?;
            buffer
        }
        _ => Vec::new(),
    };

    Ok(Reply { response, body })
}
```

- [ ] **Step 4: Run the tests, the whole suite, then commit**

Run: `cd riabuild-cli && cargo test channel::client`
Expected: PASS, 5 tests.

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add riabuild-cli/src/channel/client.rs riabuild-cli/src/channel/mod.rs
git commit -m "feat(channel): ask the laptop from the server

Everything here sits in the paste path, so the contract is that it never
hangs: a laptop with its lid closed produces a fast clean failure rather
than Claude Code stopping dead on Ctrl+V.

Payload bodies are read to exactly the announced length rather than until
close. A short read is a truncated screenshot, and Claude Code would accept
one."
```

---

### Task 10: Shim argv parsing

Claude Code's Linux probe is exactly:

```sh
xclip -selection clipboard -t TARGETS -o 2>/dev/null | grep -E "image/(png|jpeg|jpg|gif|webp|bmp)" \
  || wl-paste -l 2>/dev/null | grep -E "..."
```

Note `2>/dev/null`: the shim's stderr is discarded, which is why all diagnostic value has to live outside the paste path.

This task is argv → intent, as a pure function. No IO, so every documented invocation is a unit test.

**Files:**
- Create: `riabuild-cli/src/shims/clipboard.rs`
- Modify: `riabuild-cli/src/shims/mod.rs` (add `pub mod clipboard;`)

**Interfaces:**
- Produces:
  - `pub enum Tool { Xclip, WlPaste }` with `pub fn from_name(name: &str) -> Option<Tool>`
  - `pub enum Intent { Targets, Read(Option<String>), Empty, PassThrough }`
  - `pub fn parse(tool: Tool, args: &[String]) -> Intent`

`Intent::Empty` is "a selection we deliberately do not bridge" — PRIMARY. `Intent::PassThrough` is "not ours, run the real binary".

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/shims/clipboard.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn parse_argv(tool: Tool, argv: &[&str]) -> Intent {
        parse(tool, &argv.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    /// The exact probe Claude Code runs on Linux. If this row is wrong,
    /// nothing else in the design matters.
    #[test]
    fn the_claude_code_probe_is_a_targets_request() {
        let intent = parse_argv(
            Tool::Xclip,
            &["-selection", "clipboard", "-t", "TARGETS", "-o"],
        );
        assert_eq!(intent, Intent::Targets);
        assert_eq!(parse_argv(Tool::WlPaste, &["-l"]), Intent::Targets);
        assert_eq!(parse_argv(Tool::WlPaste, &["--list-types"]), Intent::Targets);
    }

    #[test]
    fn a_typed_read_carries_its_type() {
        assert_eq!(
            parse_argv(
                Tool::Xclip,
                &["-selection", "clipboard", "-t", "image/png", "-o"]
            ),
            Intent::Read(Some("image/png".into()))
        );
        assert_eq!(
            parse_argv(Tool::WlPaste, &["-t", "image/png"]),
            Intent::Read(Some("image/png".into()))
        );
        assert_eq!(
            parse_argv(Tool::WlPaste, &["--type", "text/html"]),
            Intent::Read(Some("text/html".into()))
        );
    }

    /// No type requested: serve the first type in preference order.
    #[test]
    fn a_read_with_no_type_asks_for_the_preferred_one() {
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "clipboard", "-o"]),
            Intent::Read(None)
        );
        assert_eq!(parse_argv(Tool::WlPaste, &[]), Intent::Read(None));
        assert_eq!(parse_argv(Tool::WlPaste, &["-n"]), Intent::Read(None));
    }

    /// xclip's default selection is PRIMARY, not CLIPBOARD. PRIMARY is the X11
    /// highlight buffer, it changes on every mouse drag, and bridging it is a
    /// firehose for no benefit — so it is empty rather than wrong.
    #[test]
    fn xclip_without_a_selection_is_primary_and_is_not_bridged() {
        assert_eq!(parse_argv(Tool::Xclip, &["-o"]), Intent::Empty);
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "primary", "-o"]),
            Intent::Empty
        );
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "secondary", "-o"]),
            Intent::Empty
        );
    }

    #[test]
    fn wl_paste_can_be_asked_for_the_primary_selection_too() {
        assert_eq!(parse_argv(Tool::WlPaste, &["-p"]), Intent::Empty);
        assert_eq!(parse_argv(Tool::WlPaste, &["--primary"]), Intent::Empty);
    }

    /// xclip abbreviates. `-sel c`, `-selection clip` and `-sel clipboard` are
    /// all the clipboard, and a developer's muscle memory uses all of them.
    #[test]
    fn the_clipboard_selection_is_recognised_however_it_is_abbreviated() {
        for selection in ["c", "clip", "clipboard", "CLIPBOARD"] {
            assert_eq!(
                parse_argv(Tool::Xclip, &["-selection", selection, "-o"]),
                Intent::Read(None),
                "-selection {selection}"
            );
            assert_eq!(
                parse_argv(Tool::Xclip, &["-sel", selection, "-o"]),
                Intent::Read(None),
                "-sel {selection}"
            );
        }
    }

    /// Anything that writes is not ours. The channel is read-only, and a write
    /// that silently did nothing would be worse than one that works locally.
    #[test]
    fn writes_are_passed_through_to_the_real_binary() {
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "clipboard", "-i"]),
            Intent::PassThrough
        );
        // xclip with no -o at all reads stdin and copies.
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "clipboard"]),
            Intent::PassThrough
        );
        assert_eq!(parse_argv(Tool::WlPaste, &["--watch", "cat"]), Intent::PassThrough);
    }

    #[test]
    fn informational_flags_are_passed_through() {
        for argv in [vec!["-version"], vec!["-h"], vec!["--help"]] {
            assert_eq!(parse_argv(Tool::Xclip, &argv), Intent::PassThrough, "{argv:?}");
        }
        for argv in [vec!["--version"], vec!["-h"]] {
            assert_eq!(parse_argv(Tool::WlPaste, &argv), Intent::PassThrough, "{argv:?}");
        }
    }

    #[test]
    fn only_the_two_shimmed_tools_are_recognised() {
        assert_eq!(Tool::from_name("xclip"), Some(Tool::Xclip));
        assert_eq!(Tool::from_name("wl-paste"), Some(Tool::WlPaste));
        assert_eq!(Tool::from_name("/usr/bin/xclip"), Some(Tool::Xclip));
        assert_eq!(Tool::from_name("pbpaste"), None);
        assert_eq!(Tool::from_name("wl-copy"), None);
    }
}
```

- [ ] **Step 2: Declare the module and run the tests to verify they fail**

Add `pub mod clipboard;` at the top of `riabuild-cli/src/shims/mod.rs`.

Run: `cd riabuild-cli && cargo test shims::clipboard`
Expected: FAIL — `cannot find type Tool`.

- [ ] **Step 3: Write the parser**

Prepend to `riabuild-cli/src/shims/clipboard.rs`:

```rust
//! The `xclip` and `wl-paste` shims: argv in, intent out.
//!
//! Claude Code probes the Linux clipboard with exactly
//!
//! ```sh
//! xclip -selection clipboard -t TARGETS -o 2>/dev/null | grep -E "image/(png|jpeg|…)" \
//!   || wl-paste -l 2>/dev/null | grep -E "…"
//! ```
//!
//! so a binary named `xclip` earlier on `PATH` owns the image-paste path
//! entirely. Note the `2>/dev/null`: the shim's stderr is discarded, which is
//! why every diagnostic has to live outside the paste path — in the banner, in
//! `riabuild channel status`, and in the log.
//!
//! The shim's job is to be indistinguishable from the real tool, and to get out
//! of the way for anything it does not handle.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Xclip,
    WlPaste,
}

impl Tool {
    pub fn from_name(name: &str) -> Option<Tool> {
        let base = std::path::Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string());
        match base.as_str() {
            "xclip" => Some(Tool::Xclip),
            "wl-paste" => Some(Tool::WlPaste),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Tool::Xclip => "xclip",
            Tool::WlPaste => "wl-paste",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// List the available types.
    Targets,
    /// Read one type, or the preferred one when none was named.
    Read(Option<String>),
    /// A selection riabuild deliberately does not bridge. Behaves as an empty
    /// clipboard, which is what the real tool does when nothing is selected.
    Empty,
    /// Not ours. Run the real binary.
    PassThrough,
}

fn is_clipboard_selection(value: &str) -> bool {
    // xclip accepts any unambiguous prefix of `clipboard`.
    let value = value.to_ascii_lowercase();
    !value.is_empty() && "clipboard".starts_with(&value)
}

pub fn parse(tool: Tool, args: &[String]) -> Intent {
    match tool {
        Tool::Xclip => parse_xclip(args),
        Tool::WlPaste => parse_wl_paste(args),
    }
}

fn parse_xclip(args: &[String]) -> Intent {
    let mut selection: Option<String> = None;
    let mut target: Option<String> = None;
    let mut output = false;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-o" | "-out" | "-output" => output = true,
            "-selection" | "-sel" => {
                index += 1;
                selection = args.get(index).cloned();
            }
            "-t" | "-target" => {
                index += 1;
                target = args.get(index).cloned();
            }
            // Display and verbosity flags do not change what is read.
            "-d" | "-display" => index += 1,
            "-quiet" | "-silent" | "-verbose" | "-noutf8" | "-r" | "-rmlastnl" | "-l" => {}
            // -i, -in, -f, -filter, -version, -h and anything unrecognised are
            // not a clipboard read.
            _ => return Intent::PassThrough,
        }
        index += 1;
    }

    if !output {
        // No -o means xclip is copying, not pasting.
        return Intent::PassThrough;
    }

    // xclip's default selection is PRIMARY, not CLIPBOARD.
    match selection {
        Some(value) if is_clipboard_selection(&value) => {}
        _ => return Intent::Empty,
    }

    match target.as_deref() {
        Some("TARGETS") => Intent::Targets,
        Some(target) => Intent::Read(Some(target.to_string())),
        None => Intent::Read(None),
    }
}

fn parse_wl_paste(args: &[String]) -> Intent {
    let mut target: Option<String> = None;
    let mut list = false;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-l" | "--list-types" => list = true,
            "-p" | "--primary" => return Intent::Empty,
            "-t" | "--type" => {
                index += 1;
                target = args.get(index).cloned();
            }
            "-n" | "--no-newline" => {}
            "-s" | "--seat" => index += 1,
            _ => return Intent::PassThrough,
        }
        index += 1;
    }

    if list {
        return Intent::Targets;
    }
    Intent::Read(target)
}
```

- [ ] **Step 4: Run the tests, the whole suite, then commit**

Run: `cd riabuild-cli && cargo test shims::clipboard`
Expected: PASS, 9 tests.

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add riabuild-cli/src/shims/clipboard.rs riabuild-cli/src/shims/mod.rs
git commit -m "feat(shims): parse the xclip and wl-paste invocations Claude Code uses

Claude Code's Linux probe is a single xclip TARGETS call, so a binary of
that name earlier on PATH owns the image-paste path entirely. It runs the
probe with 2>/dev/null, which is why no diagnostic can live in the shim.

xclip's default selection is PRIMARY rather than CLIPBOARD, and PRIMARY is
the highlight buffer that changes on every mouse drag. It reads as empty
rather than as the clipboard. Anything that writes passes through."
```

---

### Task 11: Shim execution and the PATH guard

The shim resolves the real binary by searching `PATH` — but `~/.riabuild/bin` *is* on `PATH`, ahead of everything. Searched naively, the shim `exec`s itself forever. This is the single most likely way to hard-hang a developer's server and it is one line to get wrong.

**Files:**
- Modify: `riabuild-cli/src/shims/clipboard.rs`
- Modify: `riabuild-cli/src/shims/mod.rs` (write the two shim scripts)
- Modify: `riabuild-cli/src/paths.rs` (add `channel_log_file`)

**Interfaces:**
- Produces:
  - `pub fn path_without(path: &str, ours: &Path) -> String`
  - `pub fn render(tool: Tool, intent_output: &Output) -> Vec<u8>` where `pub enum Output { Targets(Vec<String>), Bytes(Vec<u8>), Nothing }`
  - `pub async fn run(tool: Tool, args: &[String], socket: Option<PathBuf>, bin_dir: &Path, runner: &Arc<dyn CommandRunner>) -> i32`
  - `shims::write_clipboard_shims(ctx: &Ctx) -> Result<()>`
  - `Paths::channel_log_file(&self) -> PathBuf` — `~/.riabuild/logs/channel.log`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `riabuild-cli/src/shims/clipboard.rs`:

```rust
    /// The hard-hang. `~/.riabuild/bin` leads PATH, so a naive search finds
    /// the shim itself and execs it forever.
    #[test]
    fn our_own_directory_is_stripped_before_the_real_binary_is_resolved() {
        let stripped = path_without(
            "/home/ada/.riabuild/bin:/usr/local/bin:/usr/bin",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert_eq!(stripped, "/usr/local/bin:/usr/bin");
    }

    #[test]
    fn stripping_handles_trailing_slashes_and_repeats() {
        let stripped = path_without(
            "/home/ada/.riabuild/bin/:/usr/bin:/home/ada/.riabuild/bin",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert_eq!(stripped, "/usr/bin");
    }

    /// A PATH that was only ever our directory must not silently become the
    /// whole filesystem or an empty string that some shells read as ".".
    #[test]
    fn stripping_everything_leaves_a_safe_default_rather_than_an_empty_path() {
        let stripped = path_without(
            "/home/ada/.riabuild/bin",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert_eq!(stripped, "/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn xclip_renders_targets_one_per_line() {
        let out = render(
            Tool::Xclip,
            &Output::Targets(vec!["image/png".into(), "text/html".into()]),
        );
        assert_eq!(out, b"image/png\ntext/html\n");
    }

    /// The channel speaks MIME; xclip's callers expect atoms. TARGETS output
    /// that says `text/plain;charset=utf-8` is not what an xclip caller greps
    /// for.
    #[test]
    fn xclip_renders_text_targets_as_x11_atoms() {
        let out = render(
            Tool::Xclip,
            &Output::Targets(vec!["text/plain;charset=utf-8".into()]),
        );
        assert_eq!(out, b"UTF8_STRING\n");
    }

    #[test]
    fn wl_paste_renders_targets_in_its_own_vocabulary() {
        let out = render(
            Tool::WlPaste,
            &Output::Targets(vec!["text/plain;charset=utf-8".into(), "image/png".into()]),
        );
        assert_eq!(out, b"text/plain;charset=utf-8\nimage/png\n");
    }

    #[test]
    fn bytes_are_rendered_untouched() {
        let out = render(Tool::Xclip, &Output::Bytes(vec![0x89, b'P', b'N', b'G']));
        assert_eq!(out, vec![0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn nothing_renders_as_nothing() {
        assert!(render(Tool::Xclip, &Output::Nothing).is_empty());
    }
```

And a test for the generated scripts, in `riabuild-cli/src/shims/mod.rs`'s test module:

```rust
    #[tokio::test]
    async fn the_clipboard_shims_route_through_riabuild() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        write_clipboard_shims(&ctx).await.unwrap();
        write_clipboard_shims(&ctx).await.unwrap(); // safe twice, like every apply

        for tool in ["xclip", "wl-paste"] {
            let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join(tool))
                .await
                .unwrap();
            assert!(script.contains("channel shim"), "{script}");
            assert!(script.contains(tool), "{script}");
            assert!(script.contains(r#""$@""#), "{script}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test shims`
Expected: FAIL — `cannot find function path_without`, `render`, `write_clipboard_shims`.

- [ ] **Step 3: Implement the guard, the renderer, and the run path**

Add to `riabuild-cli/src/shims/clipboard.rs`:

```rust
use crate::channel::client;
use crate::channel::mime::{self, Vocabulary};
use crate::channel::protocol::{Request, Response};
use crate::runner::{CommandRunner, RunOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the shim will print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Targets(Vec<String>),
    Bytes(Vec<u8>),
    Nothing,
}

/// `PATH` with our own directory removed.
///
/// `~/.riabuild/bin` leads `PATH` inside the environment shell, and the shim
/// lives there under the same name as the tool it shadows. Resolving the real
/// binary against an unmodified `PATH` finds the shim again and `exec`s it
/// forever — a hard hang on the developer's server with no output.
pub fn path_without(path: &str, ours: &Path) -> String {
    let ours = ours.to_string_lossy();
    let ours = ours.trim_end_matches('/');

    let kept: Vec<&str> = path
        .split(':')
        .filter(|entry| !entry.is_empty() && entry.trim_end_matches('/') != ours)
        .collect();

    if kept.is_empty() {
        // An empty PATH is read as "." by some shells, which would resolve
        // whatever happens to be in the working directory.
        return "/usr/local/bin:/usr/bin:/bin".to_string();
    }
    kept.join(":")
}

/// The channel speaks MIME; each tool's callers expect that tool's vocabulary.
pub fn render(tool: Tool, output: &Output) -> Vec<u8> {
    match output {
        Output::Nothing => Vec::new(),
        Output::Bytes(bytes) => bytes.clone(),
        Output::Targets(targets) => {
            let vocab = match tool {
                Tool::Xclip => Vocabulary::X11,
                Tool::WlPaste => Vocabulary::Wayland,
            };
            let mut out = String::new();
            for target in targets {
                if let Some(native) = mime::from_mime(vocab, target) {
                    out.push_str(native);
                    out.push('\n');
                }
            }
            out.into_bytes()
        }
    }
}

/// Runs the shim. Returns the exit code the real tool would have used.
///
/// "Channel down" and "clipboard empty" are deliberately identical to the
/// caller: xclip has no way to say "your laptop is asleep", and Claude Code
/// discards stderr. The distinction lives in the log.
pub async fn run(
    tool: Tool,
    args: &[String],
    socket: Option<PathBuf>,
    bin_dir: &Path,
    runner: &Arc<dyn CommandRunner>,
) -> i32 {
    let intent = parse(tool, args);

    let request = match intent {
        Intent::PassThrough => return pass_through(tool, args, bin_dir, runner).await,
        Intent::Empty => return emit(&Output::Nothing, tool),
        Intent::Targets => Request::ClipboardTargets,
        Intent::Read(Some(ref target)) => {
            let vocab = match tool {
                Tool::Xclip => Vocabulary::X11,
                Tool::WlPaste => Vocabulary::Wayland,
            };
            match mime::to_mime(vocab, target) {
                Some(mime) => Request::ClipboardRead { mime: mime.into() },
                // A type the channel does not carry reads as an empty
                // clipboard, which is what the real tool does for a target the
                // selection does not hold.
                None => return emit(&Output::Nothing, tool),
            }
        }
        // No type named: ask what is there, then take the first — which is the
        // preferred text flavour when text is present.
        Intent::Read(None) => Request::ClipboardTargets,
    };

    let Some(socket) = socket else {
        log("the channel socket is not configured for this session");
        return emit(&Output::Nothing, tool);
    };

    let reply = match client::request(&socket, &request).await {
        Ok(reply) => reply,
        Err(error) => {
            log(&format!("{error}"));
            return emit(&Output::Nothing, tool);
        }
    };

    match (&intent, reply.response) {
        (Intent::Targets, Response::Targets(targets)) => emit(&Output::Targets(targets), tool),
        (Intent::Read(None), Response::Targets(targets)) => {
            let Some(first) = targets.first().cloned() else {
                return emit(&Output::Nothing, tool);
            };
            match client::request(&socket, &Request::ClipboardRead { mime: first }).await {
                Ok(second) => match second.response {
                    Response::Payload { .. } => emit(&Output::Bytes(second.body), tool),
                    other => {
                        log(&format!("{other:?}"));
                        emit(&Output::Nothing, tool)
                    }
                },
                Err(error) => {
                    log(&format!("{error}"));
                    emit(&Output::Nothing, tool)
                }
            }
        }
        (_, Response::Payload { .. }) => emit(&Output::Bytes(reply.body), tool),
        (_, other) => {
            log(&format!("{other:?}"));
            emit(&Output::Nothing, tool)
        }
    }
}

/// Writes to stdout and returns the exit code.
///
/// Empty output exits 1, which is what both real tools do when the selection
/// holds nothing.
fn emit(output: &Output, tool: Tool) -> i32 {
    use std::io::Write;
    let bytes = render(tool, output);
    if bytes.is_empty() {
        return 1;
    }
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    if handle.write_all(&bytes).is_err() || handle.flush().is_err() {
        return 1;
    }
    0
}

async fn pass_through(
    tool: Tool,
    args: &[String],
    bin_dir: &Path,
    runner: &Arc<dyn CommandRunner>,
) -> i32 {
    let path = std::env::var("PATH").unwrap_or_default();
    let stripped = path_without(&path, bin_dir);

    let borrowed: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
    let options = RunOptions {
        env: vec![("PATH".into(), stripped)],
        ..Default::default()
    };

    match runner.run_interactive(tool.name(), &borrowed, &options).await {
        Ok(code) => code,
        // Reproduces the shell's own "command not found" code rather than a
        // riabuild error, because the caller is expecting the real tool.
        Err(_) => 127,
    }
}

/// The only place a shim diagnostic can survive.
///
/// Claude Code runs the probe with `2>/dev/null`, so stderr is discarded.
fn log(message: &str) {
    if let Ok(path) = std::env::var("RIABUILD_CHANNEL_LOG") {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "shim: {message}");
        }
    }
    eprintln!("riabuild: {message}");
}
```

Note: `pass_through` and `log` use synchronous stdio, which is the documented `ui.rs` exception — this is a handoff to a child process and a terminal write, not IO riabuild performs.

- [ ] **Step 4: Write the shim scripts and the log path**

Add to `riabuild-cli/src/paths.rs`, beside `log_file`:

```rust
    /// Where the channel records why paste stopped working.
    ///
    /// Separate from `riabuild.log` because it is the answer to a specific
    /// question a developer asks in the middle of something else.
    fn channel_log_file(&self) -> PathBuf {
        self.root().join("logs").join("channel.log")
    }
```

Add to `riabuild-cli/src/shims/mod.rs`:

```rust
/// `~/.riabuild/bin/xclip` and `~/.riabuild/bin/wl-paste`.
///
/// Both route into riabuild, which decides whether the invocation is a
/// clipboard read to send down the channel or something to hand to the real
/// binary. They are written on the *server*, where `~/.riabuild/bin` leads
/// PATH and Claude Code's probe will find them first.
pub async fn write_clipboard_shims(ctx: &Ctx) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;

    for tool in ["xclip", "wl-paste"] {
        let script = format!(
            r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Reads the laptop's clipboard over the riabuild channel. Anything that is not
# a clipboard read is handed to the real {tool} on PATH.
exec riabuild channel shim {tool} "$@"
"#
        );
        let path = bin.join(tool);
        tokio::fs::write(&path, script).await?;
        make_executable(&path).await?;
    }
    Ok(())
}
```

- [ ] **Step 5: Run the tests, the whole suite, then commit**

Run: `cd riabuild-cli && cargo test shims`
Expected: PASS.

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add riabuild-cli/src/shims/ riabuild-cli/src/paths.rs
git commit -m "feat(shims): serve the clipboard, and never exec ourselves

~/.riabuild/bin leads PATH and the shim lives there under the same name as
the tool it shadows, so resolving the real binary against an unmodified PATH
finds the shim again and execs it forever. Stripping our own directory first
is the single most likely way to hard-hang a developer's server.

Channel-down and clipboard-empty are deliberately identical to the caller:
xclip has no vocabulary for 'your laptop is asleep' and Claude Code discards
stderr, so the distinction lives in the channel log."
```

---

### Task 12: The supervisor

Three resilience mechanisms, because each catches what the others miss. This task builds the decidable parts — the ssh argv that encodes two of the three mechanisms, the backoff schedule, and the diagnosis of a refused forward.

**The run loop itself is deferred**, for two reasons worth stating rather than discovering later. It needs a host, a port, and an identity, all of which remote mode owns and none of which exist yet. And it needs to hold a long-lived child process while pinging concurrently, which `CommandRunner` cannot express — `run` waits for the child to exit, and adding a spawn-and-supervise method is a change to the crate's most load-bearing abstraction that should be made when there is a caller to shape it. Everything the loop will need to *decide* is built and tested here; what remains is the plumbing that drives it.

**Files:**
- Create: `riabuild-cli/src/channel/supervisor.rs`
- Modify: `riabuild-cli/src/channel/mod.rs`

**Interfaces:**
- Produces:
  - `pub struct Tunnel { pub host: String, pub user: String, pub port: u16, pub identity: PathBuf, pub remote_socket: PathBuf, pub local_socket: PathBuf }`
  - `pub fn ssh_args(tunnel: &Tunnel) -> Vec<String>`
  - `pub fn backoff(attempt: u32) -> Duration`
  - `pub fn diagnose(stderr: &str) -> Option<Failure>`

- [ ] **Step 1: Write the failing tests**

Create `riabuild-cli/src/channel/supervisor.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tunnel() -> Tunnel {
        Tunnel {
            host: "build-01.clubria.dev".into(),
            user: "ada".into(),
            port: 22,
            identity: PathBuf::from("/home/ada/.riabuild/ssh/id_ed25519"),
            remote_socket: PathBuf::from("/run/user/1000/riabuild/channel.sock"),
            local_socket: PathBuf::from("/tmp/riabuild/agent.sock"),
        }
    }

    /// Both of these are load-bearing rather than tuning.
    #[test]
    fn the_forward_fails_loudly_and_cleans_up_after_itself() {
        let args = ssh_args(&tunnel()).join(" ");
        // Without this, a forward that fails to bind leaves a live connection
        // forwarding nothing and the failure is invisible.
        assert!(args.contains("ExitOnForwardFailure=yes"), "{args}");
        // Without this, a socket left by a killed session blocks the rebind and
        // the channel comes up permanently dead.
        assert!(args.contains("StreamLocalBindUnlink=yes"), "{args}");
    }

    /// Converts a black-hole network into an exit the supervisor can see, in
    /// about 45 seconds.
    #[test]
    fn keepalives_turn_silence_into_an_exit() {
        let args = ssh_args(&tunnel()).join(" ");
        assert!(args.contains("ServerAliveInterval=15"), "{args}");
        assert!(args.contains("ServerAliveCountMax=3"), "{args}");
    }

    #[test]
    fn the_forward_maps_the_remote_socket_onto_the_local_one() {
        let args = ssh_args(&tunnel()).join(" ");
        assert!(
            args.contains("-R /run/user/1000/riabuild/channel.sock:/tmp/riabuild/agent.sock"),
            "{args}"
        );
        // -N: a forward, never a shell. The mosh session is the shell.
        assert!(args.contains("-N"), "{args}");
    }

    #[test]
    fn the_tunnel_uses_the_riabuild_identity_and_port() {
        let args = ssh_args(&tunnel()).join(" ");
        assert!(args.contains("-i /home/ada/.riabuild/ssh/id_ed25519"), "{args}");
        assert!(args.contains("-p 22"), "{args}");
        assert!(args.contains("ada@build-01.clubria.dev"), "{args}");
    }

    /// A tight loop against a server that refuses the forward is a denial of
    /// service against the developer's own machine.
    #[test]
    fn backoff_grows_from_one_second_to_a_thirty_second_ceiling() {
        assert!(backoff(0) >= Duration::from_secs(1));
        assert!(backoff(0) < Duration::from_secs(2));
        assert!(backoff(3) > backoff(1));
        for attempt in 8..20 {
            assert!(
                backoff(attempt) <= Duration::from_secs(30),
                "attempt {attempt} exceeded the ceiling"
            );
        }
    }

    /// Every laptop reconnecting at once after a network blip would be a
    /// thundering herd against the server.
    #[test]
    fn backoff_is_jittered_rather_than_a_fixed_schedule() {
        let delays: Vec<Duration> = (0..40).map(|_| backoff(5)).collect();
        let distinct = delays.iter().collect::<std::collections::HashSet<_>>();
        assert!(distinct.len() > 1, "backoff(5) is deterministic");
        // Jitter stays inside the band rather than wandering.
        for delay in delays {
            assert!(delay <= Duration::from_secs(30));
        }
    }

    /// The failure nobody can diagnose from the symptom. Without this the
    /// developer sees "paste does not work" and has nothing to act on.
    #[test]
    fn a_server_that_forbids_socket_forwarding_is_named_precisely() {
        let failure = diagnose(
            "Error: remote port forwarding failed for listen path /run/user/1000/riabuild/channel.sock",
        )
        .expect("should be diagnosed");
        // `Failure`'s Display is `{attempting} — {action}`, which is exactly
        // the pair this assertion is about.
        let text = failure.to_string();
        assert!(text.contains("AllowStreamLocalForwarding"), "{text}");
    }

    #[test]
    fn an_openssh_too_old_for_socket_forwarding_is_a_hard_stop() {
        let failure =
            diagnose("Bad remote forwarding specification").expect("should be diagnosed");
        let text = failure.to_string();
        assert!(text.contains("6.7") || text.contains("OpenSSH"), "{text}");
    }

    /// An ordinary disconnect is the supervisor's job to retry, not something
    /// to stop and complain about.
    #[test]
    fn a_routine_disconnect_is_not_diagnosed_as_a_configuration_fault() {
        assert!(diagnose("Connection to build-01 closed by remote host.").is_none());
        assert!(diagnose("").is_none());
    }
}
```

- [ ] **Step 2: Declare the module and run the tests to verify they fail**

Add `pub mod supervisor;` to `riabuild-cli/src/channel/mod.rs`.

Run: `cd riabuild-cli && cargo test channel::supervisor`
Expected: FAIL — `cannot find type Tunnel`.

- [ ] **Step 3: Implement it**

Prepend to `riabuild-cli/src/channel/supervisor.rs`:

```rust
//! Keeping the tunnel up.
//!
//! The requirement is mosh-grade: recover whenever the channel drops *or goes
//! quiet for too long*. Three mechanisms, because each catches what the others
//! miss.
//!
//! | Mechanism | Catches |
//! |---|---|
//! | `ssh -N -R` as a supervised child, rebuilt with jittered backoff | clean exits — the connection died and said so |
//! | `ServerAliveInterval`/`ServerAliveCountMax` | black-hole networks: converts silence into an exit, in ~45 s |
//! | `channel.ping` every 30 s, teardown after two misses | half-open sockets — SSH believes the connection is fine while the forward is wedged. Keepalives run below the forward and cannot see this |
//!
//! The supervisor lives on the laptop, because the laptop holds the identity
//! and is the side that comes and goes. The server end is entirely passive.

use crate::ui::Failure;
use rand::Rng;
use std::path::PathBuf;
use std::time::Duration;

/// How often the supervisor proves the forward actually carries traffic.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Misses before the tunnel is torn down and rebuilt.
pub const PING_MISSES: u32 = 2;

const BACKOFF_CEILING: Duration = Duration::from_secs(30);

pub struct Tunnel {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity: PathBuf,
    /// Where the socket appears on the server.
    pub remote_socket: PathBuf,
    /// Where the agent is listening on the laptop.
    pub local_socket: PathBuf,
}

pub fn ssh_args(tunnel: &Tunnel) -> Vec<String> {
    let forward = format!(
        "{}:{}",
        tunnel.remote_socket.display(),
        tunnel.local_socket.display()
    );

    vec![
        // A forward, never a shell — the mosh session is the shell.
        "-N".into(),
        "-R".into(),
        forward,
        "-i".into(),
        tunnel.identity.display().to_string(),
        "-p".into(),
        tunnel.port.to_string(),
        // Without this, a forward that fails to bind leaves a live connection
        // forwarding nothing, and the failure is invisible.
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        // Without this, a socket left by a killed session blocks the rebind
        // and the channel comes up permanently dead.
        "-o".into(),
        "StreamLocalBindUnlink=yes".into(),
        // Turns a black-hole network into an exit the supervisor can see.
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        format!("{}@{}", tunnel.user, tunnel.host),
    ]
}

/// Exponential from one second, jittered, capped at thirty.
///
/// Jitter matters as much as the ceiling: every laptop reconnecting at the
/// same moment after a network blip is a thundering herd against the server.
pub fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_secs(1)
        .saturating_mul(2u32.saturating_pow(attempt.min(5)))
        .min(BACKOFF_CEILING);

    let jitter = rand::rng().random_range(0.75..1.0);
    let millis = (base.as_millis() as f64 * jitter) as u64;
    Duration::from_millis(millis.max(1_000)).min(BACKOFF_CEILING)
}

/// Turns an ssh failure into something a developer can act on, or `None` when
/// it is an ordinary disconnect the supervisor should simply retry.
pub fn diagnose(stderr: &str) -> Option<Failure> {
    let lower = stderr.to_ascii_lowercase();

    if lower.contains("remote port forwarding failed") || lower.contains("forwarding not permitted")
    {
        return Some(
            Failure::new(
                "The server refused to forward the clipboard socket",
                "Ask whoever administers the server to set `AllowStreamLocalForwarding yes` in /etc/ssh/sshd_config, then reload sshd.",
            )
            .detail(stderr.trim().to_string()),
        );
    }

    if lower.contains("bad remote forwarding specification") {
        return Some(
            Failure::new(
                "The server's OpenSSH is too old to forward a unix socket",
                "Upgrade the server to OpenSSH 6.7 or newer. riabuild does not fall back to a TCP port, because a loopback port is readable by every other user on that machine.",
            )
            .detail(stderr.trim().to_string()),
        );
    }

    None
}
```

- [ ] **Step 4: Run the tests, the whole suite, then commit**

Run: `cd riabuild-cli && cargo test channel::supervisor`
Expected: PASS, 9 tests.

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`

```bash
git add riabuild-cli/src/channel/supervisor.rs riabuild-cli/src/channel/mod.rs
git commit -m "feat(channel): keep the tunnel up, mosh-grade

Three mechanisms because each catches what the others miss: a supervised
child with jittered backoff for clean exits, ssh keepalives to turn a
black-hole network into an exit in ~45s, and an application-level ping for
half-open sockets, which run above the forward and are the case keepalives
structurally cannot see.

ExitOnForwardFailure and StreamLocalBindUnlink are load-bearing rather than
tuning: without the first a failed bind leaves a live connection forwarding
nothing, and without the second a socket from a killed session makes the
channel come up permanently dead.

A refused forward names the sshd_config directive, which is the failure
nobody can diagnose from the symptom."
```

---

### Task 13: The `channel` subcommand

Wires everything to a command line. `riabuild channel agent` runs the laptop side; `riabuild channel shim <tool>` is what the generated scripts call; `riabuild channel status` is where a developer finds out why paste stopped.

**Files:**
- Modify: `riabuild-cli/src/cli.rs`, `riabuild-cli/src/main.rs`, `riabuild-cli/src/channel/mod.rs`

**Interfaces:**
- Produces: `Command::Channel { action: ChannelAction }`, `pub enum ChannelAction { Agent { socket: Option<String> }, Shim { tool: String, args: Vec<String> }, Status }`, and `channel::socket_path(paths: &dyn Paths) -> Option<PathBuf>`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `riabuild-cli/src/cli.rs`:

```rust
    #[test]
    fn the_shim_passes_its_arguments_through_verbatim() {
        // The generated ~/.riabuild/bin/xclip runs exactly this. Flags that
        // look like riabuild's own must reach the parser as the tool's.
        let cli = Cli::parse_from([
            "riabuild",
            "channel",
            "shim",
            "xclip",
            "-selection",
            "clipboard",
            "-t",
            "TARGETS",
            "-o",
        ]);
        let Some(Command::Channel {
            action: ChannelAction::Shim { tool, args },
        }) = cli.command
        else {
            panic!("expected a shim invocation");
        };
        assert_eq!(tool, "xclip");
        assert_eq!(args, ["-selection", "clipboard", "-t", "TARGETS", "-o"]);
    }

    #[test]
    fn the_agent_can_be_told_where_to_listen() {
        let cli = Cli::parse_from(["riabuild", "channel", "agent", "--socket", "/tmp/a.sock"]);
        let Some(Command::Channel {
            action: ChannelAction::Agent { socket },
        }) = cli.command
        else {
            panic!("expected the agent");
        };
        assert_eq!(socket.as_deref(), Some("/tmp/a.sock"));
    }

    #[test]
    fn channel_status_is_a_plain_subcommand() {
        let cli = Cli::parse_from(["riabuild", "channel", "status"]);
        assert!(matches!(
            cli.command,
            Some(Command::Channel {
                action: ChannelAction::Status
            })
        ));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test cli::`
Expected: FAIL — `cannot find type ChannelAction`.

- [ ] **Step 3: Add the subcommand**

In `riabuild-cli/src/cli.rs`, add to `enum Command`:

```rust
    /// The laptop channel: what makes paste work over `riabuild remote`.
    Channel {
        #[command(subcommand)]
        action: ChannelAction,
    },
```

And below the enum:

```rust
#[derive(Debug, Subcommand)]
pub enum ChannelAction {
    /// Serve this laptop's clipboard to a remote session.
    ///
    /// Hidden: started by `riabuild remote`, not by a developer.
    #[command(hide = true)]
    Agent {
        /// Where to listen. Defaults to the session's runtime directory.
        #[arg(long, value_name = "PATH")]
        socket: Option<String>,
    },
    /// Stand in for `xclip` or `wl-paste` on the server.
    ///
    /// Hidden: invoked by the generated shims in `~/.riabuild/bin`.
    #[command(hide = true)]
    Shim {
        /// The tool being shadowed.
        tool: String,
        /// That tool's own arguments, passed through untouched.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Report whether the clipboard channel is up.
    Status,
}
```

- [ ] **Step 4: Resolve the socket path**

Add to `riabuild-cli/src/channel/mod.rs`:

```rust
use std::path::PathBuf;

/// The environment variable the shim reads to find the channel.
///
/// Set by remote mode in the environment shell. Its absence is how a local
/// session — where the clipboard is already the developer's own — leaves the
/// real tools alone.
pub const SOCKET_ENV: &str = "RIABUILD_CHANNEL_SOCKET";

/// Where the shim should look for the channel.
///
/// Explicit configuration wins. Otherwise the runtime directory, resolved the
/// way remote mode already resolves it: `$XDG_RUNTIME_DIR`, then `$TMPDIR`,
/// then `/tmp`.
///
/// When remote mode lands, this should defer to its runtime-directory helper,
/// which additionally enforces the 0700, ownership, and symlink rules. Until
/// then it computes the same path without those checks — which is safe here
/// because this function only ever *reads* a path the supervisor created.
pub fn socket_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(SOCKET_ENV) {
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }

    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .or_else(|| std::env::var("TMPDIR").ok().filter(|dir| !dir.is_empty()))
        .unwrap_or_else(|| "/tmp".to_string());

    Some(PathBuf::from(runtime).join("riabuild").join("channel.sock"))
}
```

- [ ] **Step 5: Dispatch in `main.rs`**

`Channel` must be handled before the setup flow — the shim runs on every Ctrl+V and must not check the machine, and `main` currently builds a `Ctx` that talks to the API. Add near the top of `run`, beside the `Reset` early return:

```rust
    if let Some(Command::Channel { action }) = &cli.command {
        return channel::dispatch(action, cli.quiet).await;
    }
```

Add to `riabuild-cli/src/channel/mod.rs`:

```rust
use crate::cli::ChannelAction;
// `bin_dir` is a trait method, so the trait has to be in scope even though
// only the concrete `RealPaths` is named below.
use crate::paths::{Paths, RealPaths};
use crate::runner::{CommandRunner, RealRunner};
use crate::shims::clipboard::Tool;
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::sync::Arc;

pub async fn dispatch(action: &ChannelAction, quiet: bool) -> Result<i32> {
    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);

    match action {
        ChannelAction::Shim { tool, args } => {
            let Some(tool) = Tool::from_name(tool) else {
                // Not a tool riabuild shadows. Say so rather than exit 0.
                return Ok(127);
            };
            let bin = crate::paths::RealPaths::new()?.bin_dir();
            Ok(crate::shims::clipboard::run(tool, args, socket_path(), &bin, &runner).await)
        }
        ChannelAction::Agent { socket } => {
            let socket = socket
                .as_ref()
                .map(PathBuf::from)
                .or_else(socket_path)
                .ok_or_else(|| {
                    Failure::new(
                        "riabuild could not work out where to serve the clipboard",
                        "Pass --socket with an explicit path.",
                    )
                })?;

            let os = std::env::consts::OS;
            let wayland = std::env::var("WAYLAND_DISPLAY").ok();
            let Some(session) = clipboard::detect(&runner, os, wayland.as_deref()) else {
                return Err(Failure::new(
                    "This laptop has no clipboard tool riabuild can read",
                    clipboard::install_hint(wayland.is_some()),
                )
                .into());
            };

            let agent = Arc::new(agent::Agent::new(clipboard::backend(runner, session)));
            // The runtime is current-thread, so the agent's per-connection
            // tasks need a LocalSet to be spawned into.
            let local = tokio::task::LocalSet::new();
            local.run_until(agent.serve(&socket)).await?;
            Ok(0)
        }
        ChannelAction::Status => {
            let ui = Ui::new(quiet);
            let Some(socket) = socket_path() else {
                ui.info("No clipboard channel is configured for this session.");
                return Ok(1);
            };
            match client::request(&socket, &protocol::Request::ChannelPing).await {
                Ok(_) => {
                    ui.info(&format!("Clipboard channel — connected ({})", socket.display()));
                    Ok(0)
                }
                Err(error) => {
                    ui.warn(&format!("Clipboard channel — down: {error}"));
                    ui.info("Paste will not work until the laptop reconnects. Everything else is unaffected.");
                    Ok(1)
                }
            }
        }
    }
}
```

`std::env::consts::OS` appears here, which the platform rule confines to five files. That rule's purpose is that no *decision* be made where a test cannot reach it — the decision lives in `clipboard::detect`, which takes the OS as a parameter and is tested for every platform. This call site only supplies the real value, exactly as `paths::default_project_dir` wraps `default_project_dir_on`. Add a comment saying so.

- [ ] **Step 6: Run everything and commit**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green.

Verify the shim end to end by hand:

```bash
cargo build
RIABUILD_CHANNEL_SOCKET=/tmp/absent.sock ./target/debug/riabuild channel shim xclip \
  -selection clipboard -t TARGETS -o; echo "exit=$?"
```
Expected: no stdout, `exit=1`, and a line on stderr — the down-and-empty contract.

```bash
git add riabuild-cli/src/cli.rs riabuild-cli/src/main.rs riabuild-cli/src/channel/mod.rs
git commit -m "feat(channel): the channel subcommand

agent serves the laptop, shim stands in for xclip and wl-paste on the
server, and status is where a developer finds out why paste stopped — the
one place a diagnostic can survive, since Claude Code runs the probe with
2>/dev/null.

Channel is dispatched before the setup flow: the shim runs on every Ctrl+V
and must not check the machine or talk to the API."
```

---

## Deferred: what remote mode unlocks

Open these as a follow-up PR once `src/remote/` exists. Each is small; none changes anything above.

- [x] **The supervisor run loop.** Task 12 builds every decision it makes; this is the plumbing that drives them. It needs a `CommandRunner` method that spawns a long-lived child and hands back a handle, so `ssh -N -R` can be held while `channel.ping` runs concurrently — `run` waits for exit and cannot express it. Tests: scripted `ssh` exits assert the backoff schedule and jitter bounds; an agent that stops answering `channel.ping` is torn down and rebuilt after `PING_MISSES`.
- [x] Start the supervisor from the `riabuild remote` flow, and set `RIABUILD_CHANNEL_SOCKET` in the environment shell. It is namespaced (`<namespace>/channel.sock`), not left to the server's own runtime directory: developers share one Unix account, so they share one uid and one `$XDG_RUNTIME_DIR`.
- [x] Add `● Clipboard channel — connected` to remote mode's banner.
- [x] Replace `channel::socket_path`'s inline runtime-directory resolution with remote mode's helper, which enforces the 0700, ownership, and symlink rules. **The checklist named the wrong function:** `choose_runtime_dir` only guarantees the directory exists and is writable. The 0700/ownership/symlink rules live in `private_dir::ensure_private_dir`, which is `pub(super)` and unreachable from `channel/` — so `channel/socket.rs` implements them, creating the parent *at* 0700 rather than creating it at the umask and repairing it.
- [x] Refcount the tunnel with remote mode's `sessions/<pid>` markers and `kill -0` sweep: two terminals into one server share one channel, the first to exit tears down nothing, the last tears down. Markers live on the **laptop** (`~/.riabuild/channel-sessions/<server-hash>/<pid>`), because that is where the supervisor runs. Two divergences from `gh_session`, both deliberate: no 24 h age cap (declaring a day-old mosh session stale is exactly how the second `ssh -R` gets started, and its `StreamLocalBindUnlink=yes` silently breaks the first), and a `read_dir` fault is an error rather than a zero.
- [x] Refuse a pre-existing socket owned by another uid rather than unlinking it. This is the *laptop's* create path only — the server end carries `StreamLocalBindUnlink=yes` on purpose, without which a socket left by a killed session blocks the rebind forever.
- [x] Write the amended-invariant paragraph into `2026-08-06-remote-mode-design.md`.
- [x] `e2e/remote` degradation test: kill the tunnel mid-session; setup re-runs, the shell works, and only clipboard fails. A paste degrades **within a bounded `timeout`** — exit 124 is treated as a distinct failure, because a wedged Ctrl+V is the "environment broken" outcome this whole property exists to rule out. Secrets are asserted as far as `env_local` still being evaluated; a real re-pull needs an installed `infisical`.
- [x] `e2e/remote` end-to-end test: a PNG and a UTF-8 string paste through a real shim, **and** a copy on the server that lands on the laptop's clipboard. Real `xclip` on a real `Xvfb`, not a stand-in — a scripted binary would re-test what `FakeRunner` already covers while dropping the only things a real tool can prove: that riabuild's argv is one `xclip` accepts, that a PNG survives X11's atom vocabulary both ways, and that `run_forking` returns rather than blocking on the selection owner `xclip -i` leaves behind.

Both live in `e2e/remote/channel.sh`, which needs no `RIABUILD_E2E_GH_TOKEN` — it never runs `riabuild remote`, so nothing re-verifies org membership — and therefore runs on every pull request, including from forks. What it does **not** observe is remote mode's own wiring: it owns its tunnel deliberately, because a supervisor that rebuilds a killed tunnel is correct behaviour and the wrong thing to test a degradation against.

---

## Task 14: Clipboard writes (added after the first thirteen landed)

Built as an increment on the finished channel, so this is a record of what changed rather than a script to follow. The design section is *What Claude Code actually does* in the spec.

**Why it is not what it appears to be.** The request was "Claude Code can select and copy text, so add writes." Reading the shipped bundle showed Claude Code skips `xclip` entirely when `SSH_CONNECTION` is set and emits OSC 52 instead — and a real mosh 1.4.0 round trip relays OSC 52 verbatim. So Claude Code's own copy already worked, over both transports, and a write shim can never intercept it. The write path was built anyway, for the reason that survives: every *other* program on the server that copies (`gh`, `git`, `pass`, editors, `| xclip` scripts) writes into a clipboard nobody can paste from.

**Two runner changes, both because the trait was text-shaped.**

- `RunOptions.stdin` became `Vec<u8>`. A `String` cannot represent a PNG, so an image write was not lossy but unconstructible. One caller (`keychain.rs`) changed.
- `CommandRunner::run_forking` was added. `xclip -i` and `wl-copy` fork a background child to serve the selection, and it inherits the captured stdout; `run` finishes by reading stdout to EOF, which arrives only when the selection is replaced. Measured at 3.01s against a 3-second forked child versus 0.00s with stdio nulled. `run_forking` nulls stdio and reaps only the direct child, at the cost of stderr — which is why a failed write reports its exit status instead.

**What was added.**

- [x] `Request::ClipboardWrite { mime, len }` and `Response::Written`. The only request with a body, framed exactly as a payload response is. The cap is enforced in `decode_request`, because this is the only length a peer chooses.
- [x] `Clipboard::write` on all three backends: `xclip -t <atom> -i`, `wl-copy --type <mime>`, and on macOS `pbcopy` for text with an `«data …»` AppleScript literal on stdin for everything else — the exact inverse of the envelope `read` already decodes, which keeps writes off the filesystem and clear of `ARG_MAX`.
- [x] `Agent::write`, which drops the snapshot **before** the write so a partial failure cannot leave a reader served content the laptop no longer holds.
- [x] `client::request_with_body`, and body framing in `agent/server.rs`.
- [x] A `wl-copy` shim beside `xclip` and `wl-paste`, with `CLIPBOARD_TOOLS` as the single list and a test that every shadowed name has a parser behind it.
- [x] `Intent::Write`, including `wl-copy hello world` copying its arguments. PRIMARY writes, `--clear` and `-f`/`-filter` pass through.
- [x] A failed write exits **non-zero**. Reads degrade into something indistinguishable from an empty clipboard on purpose; a write has no such twin, and a silent success loses what the developer copied.

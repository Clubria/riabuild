# Subdued child output

**Date:** 2026-08-12
**Status:** Implemented
**Amends:** the stdio exception in `riabuild-cli/CLAUDE.md`

## Problem

`riabuild` prints a deliberate, role-coloured page: a brand-coloured mark, a `◐` that
becomes a `●`, dim reasons under each task. Then it runs `sudo apt-get install`, and the
terminal fills with apt's own progress bars, its own colours, and a screenful of package
names — none of it riabuild's, none of it distinguishable from riabuild's, and all of it
louder than the line above it.

Every child process that reaches the developer's terminal does so through one method,
`CommandRunner::run_interactive`, which calls `Command::status()` — a plain fd inherit.
riabuild hands over the terminal and sees nothing until the child exits. Whatever the
child draws is what the developer gets: colour riabuild did not choose, cursor motion over
lines riabuild wrote, and in principle an alternate screen or a window title of the
child's choosing.

The output riabuild produces itself is already governed. `theme.rs` picks colour by role
and degrades down a ladder to nothing. That governance stops at the fd inherit, and the
noisiest output of a provisioning run is on the far side of it.

## Approach

A subdued mode: the child runs under a pty riabuild owns, everything it draws with is
discarded, and what remains is printed one dimmed line at a time.

Colour is the visible half. The durable half is **line discipline** — a subdued child
emits lines and nothing else. It cannot move the cursor, cannot clear the screen, cannot
enter the alternate screen, and cannot rename the developer's window. riabuild does not
trust a third-party program to keep a tidy terminal, and under this mode it no longer has
to.

This applies only to the provisioning commands riabuild runs *at* the developer. The
environment shell, `ssh`/`mosh`, and `claude` are the developer's workspace, not
riabuild's output, and stay on the raw handoff.

## The seam

```rust
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    /// Run under a pty, discard everything the child draws with, and print
    /// what is left one dimmed line at a time.
    pub subdued: Option<Theme>,
}
```

`None` is the derived default, so every existing `..Default::default()` site is unchanged
and every capturing method ignores the field. Only `run_interactive` honours it.

The field carries a `Theme` rather than a `bool` for the reason `riabuild-cli/CLAUDE.md`
already gives for generated rcfile text: the palette is resolved on the side that has a
`Ui` and passed to the side that does not. `runner/` has no `Ui` and must not grow one.
Callers pass `ctx.ui.theme()`. `Theme::plain()` is a legitimate value — line discipline
with no dim — and is what a `NO_COLOR` run produces without a special case.

## Where it applies

| Call site | Command | Subdued |
|---|---|---|
| `update.rs:221` | `sudo apt-get update` | yes |
| `update.rs:227` | `sudo apt-get install --only-upgrade -y riabuild` | yes |
| `update.rs:238` | `sudo dnf upgrade -y --refresh riabuild` | yes |
| `tasks/github_cli/sign_in.rs:82` | `gh auth login` | yes |
| `remote/authorise.rs:314` | `ssh-copy-id` | yes |
| `update.rs:255` | riabuild re-execing itself | no |
| `shell/mod.rs:239` | the environment shell | no |
| `remote/shell.rs` | `ssh`, `mosh` | no |
| `accounts/command.rs:83`, `tasks/claude_accounts.rs:263` | `claude auth login` | no |
| `shims/clipboard/serve.rs:257` | the real `xclip`/`pbcopy` | no |

Two of the exclusions are not merely taste.

**The clipboard shim.** That call is riabuild impersonating `xclip` or `pbcopy` for a
caller that expects the real tool's bytes. Its stdout is a payload, not a page. Filtering
it corrupts a pass-through, and the caller — Claude Code — has no way to notice.

**The re-exec.** The child is riabuild, whose output is already themed. Subduing it would
dim a whole second run of the program and nest its indent under the first.

`gh auth login` is safe to subdue because both commands reaching `run_gh_auth` open a
**device-code flow** — text plus a wait for a person, as the comment at `sign_in.rs:47`
records. Had it been `gh`'s arrow-key selection prompt, full line discipline would have
destroyed it, and that call site would belong in the second half of the table.

## Degradation

With no controlling terminal — CI, `cargo test`, a pipe, `riabuild` under a script —
`subdued` is ignored outright: no pty is allocated and the call is exactly today's
`Command::status()`.

This is not a convenience. An unattended run must not take a different code path from an
attended one for a *cosmetic* reason, and a pty allocated where no terminal exists would
be riabuild inventing a tty for a child that correctly concluded there wasn't one. It also
keeps `e2e/` output byte-identical, so nothing in that suite has to learn about this
feature.

`NO_COLOR` is a separate axis and needs no branch: the theme resolves to `Depth::None`,
`Role::Muted` renders as nothing, and the line discipline still applies. Colour off does
not mean untidy output is welcome.

## The pty — `runner/pty.rs`

`#[cfg(unix)]`, which is the whole supported surface. `runner/mod.rs` already carries a
`cfg(unix)` split for `is_executable`, and a pty is a platform capability in the same
sense the keychain is, so it lives in its own module rather than spreading `cfg!` through
`run_interactive`.

- **Allocation.** `libc::openpty`. `libc` is already a direct dependency, added for
  `gh_session`'s `O_NOFOLLOW` fchmod work. `portable-pty` would do this too and would add
  a dependency subtree to a binary whose `Cargo.toml` explains, twice, that every
  developer downloads it over a laptop connection.
- **The child.** `pre_exec` runs `setsid()` then `ioctl(TIOCSCTTY)`, so the child gets the
  slave as its *controlling* terminal rather than merely as three open fds. Without this
  `sudo` finds no controlling terminal and refuses to prompt.
- **The master.** `tokio::io::unix::AsyncFd`. Reads and writes stay on the current-thread
  runtime; no blocking call reaches the reactor thread.
- **Size.** `TIOCGWINSZ` on the real terminal, less the four-column indent, set on the
  slave with `TIOCSWINSZ`. `SIGWINCH` is forwarded for the child's lifetime. This needs
  tokio's `signal` feature, which the `process` feature already pulls in on unix, so the
  dependency graph does not change.
- **Raw mode.** `tcgetattr`/`tcsetattr` on the real terminal, restored by a `Drop` guard
  so an error path above cannot leave a developer's shell in raw mode with no echo. This
  is the failure a provisioner must not have: it survives the process that caused it.
- **Exit.** A read on the master after the child exits returns `EIO` on Linux and `0` on
  macOS. Both are EOF. Remaining buffered output is flushed before the exit code returns.

## The filter — `runner/subdue.rs`

Pure. No terminal, no IO, no `Theme`:

```rust
pub struct Subdue { /* line buffer, column, escape-parser state */ }

impl Subdue {
    /// Bytes in, completed lines out.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String>;
    /// Whatever is buffered mid-line, when the read has drained or the child has exited.
    pub fn flush(&mut self) -> Option<String>;
}
```

What it drops:

- **CSI** sequences entirely — SGR colour, cursor motion, erase, `?1049h` alternate
  screen, bracketed paste.
- **OSC** sequences entirely, terminated by `BEL` or `ST`. This includes OSC 0 and OSC 2,
  which is how a child would otherwise rename the developer's terminal window and leave it
  renamed.
- Lone `ESC` + single-character sequences.

What it keeps:

- `\n` completes a line and emits it.
- `\r` rewinds the column within the current line buffer rather than emitting. A progress
  bar that redraws itself fifty times collapses to its final frame — one line, its last
  state, which is the state that was true.
- `\x08` backspaces the column, since some tools erase that way.
- Tabs and every other printable byte are line content.

**The unterminated line is repainted after every read.** `sudo` writes `Password: ` with
no newline and blocks. A filter that waited for `\n` would show the developer a terminal
that had gone silent at the exact moment it wanted a password. So the current line is
available at all times through `partial()`, and the writer repaints it in place — `\r`,
the indent, the text, and padding covering whatever longer frame it replaces, which is the
same idiom `ui.applied` uses at `ui.rs:268`. A repaint of an unchanged line writes nothing,
so a child that prints a prompt and then waits does not have it reprinted on every wakeup.
This is what makes the `\r` collapse and the blocked prompt one mechanism rather than two.

The filter never prints. `pty.rs` owns the terminal and the theme, and writes each line the
filter hands back as:

```rust
println!("    {}", theme.paint(Role::Muted, &line));
```

Four spaces is `ui.note`'s indent (`ui.rs:281`), which puts child output under its task
line at the depth of a note — which is what it is. Keeping the paint out of `subdue.rs` is
what lets the filter's tests assert on plain strings rather than on escape sequences.

Being a pure function over bytes, the filter is unit-tested against canned transcripts
with no tty anywhere in the test. That is the same instinct that put every subprocess
behind `CommandRunner`: a thing that needs a real terminal in a real state to test is a
thing that stops being tested.

## Stdin, and the one guarantee that changes

Today the kernel wires the developer's keystrokes directly to `sudo` and `ssh-copy-id`.
Under a pty, riabuild's process is in that path.

Bytes are forwarded from the real terminal to the master **verbatim, unbuffered,
uninspected, and retained nowhere**. The filter operates on the output direction only;
there is no input filter and must not be one. But the honest statement is that riabuild
now copies a password rather than standing beside one, and
`remote/authorise.rs:311` — *"Nothing here reads it"* — stops being true and must be
rewritten to say what is.

This does not touch the "secrets are brokered, never stored" invariant, which is about
what riabuild writes down. Nothing here is written down. It is a narrower claim about the
keystroke path, made explicitly rather than left to be discovered.

Two behaviours are preserved, and one improves:

- **Echo.** The slave's termios owns it, so `sudo` turning `ECHO` off still hides the
  password. Raw mode on the real terminal is what stops a local echo appearing instead.
- **Signals.** Raw mode turns `ISIG` off locally, so `0x03` is forwarded and the *child's*
  line discipline raises `SIGINT`. Ctrl-C reaches the process the developer is looking at,
  which is what they meant by it — today it reaches riabuild, which is not running apt.
- **EOF.** `0x04` forwards the same way.

## riabuild becomes the terminal, and it answers no question

Dropping what a child draws with has a consequence this design did not state, and the
omission cost a hang. Some escape sequences are **questions**, and under a pty riabuild
owns, riabuild is the only thing that could answer them. It answers none: the filter drops
`ESC[6n` along with every other CSI, and nothing writes a reply back to the master.

A child that asks one therefore waits for ever — and worse than silently, because the read
it is blocked in consumes the developer's keystrokes while it looks for a reply that is not
coming. The prompt on screen is not slow to answer; it cannot be answered.

This is not hypothetical. `gh auth login` opens with a `survey` confirm — *"Authenticate
Git with your GitHub credentials? (Y/n)"* — **before** it authenticates anything, and
`survey` measures the terminal by parking the cursor at `ESC[999;999f` and reading the
reply to `ESC[6n`. `riabuild remote` sat on that line ignoring every `y`, with no device
code above it to suggest what had gone wrong. The fix was to stop the question being asked
(`tasks/github_cli/sign_in.rs`'s `own_git_credentials` settles it in advance), not to
answer it.

So the rule for adding a subdued site is narrower than "its output is untidy":

> A child may be subdued only if it never asks the terminal anything. A child whose
> prompts are drawn by a full-screen prompt library asks; plain text and a wait for a
> person does not.

apt, dnf and the device-code flow all satisfy that. If a future child does not, the
choices are to remove the question at the source, or to stop subduing it — and if neither
is possible, to teach `pty.rs` to reply, which means riabuild owning a cursor position it
currently and deliberately throws away.

## Amendment to the stdio invariant

`riabuild-cli/CLAUDE.md` currently exempts stdio from the async-IO rule on the grounds
that `run_interactive` "hands the terminal to a child process — that is a handoff, not IO
riabuild performs."

Subdued mode performs it. The invariant is amended, not broken: the handoff remains the
default and the rule for every non-subdued site, and where riabuild does perform the IO it
does so through `AsyncFd` on the current-thread runtime, never with a blocking read. The
sentence in `CLAUDE.md` gains the exception, and the `Colour` section gains a line saying
child output has a role too.

## Code layout

| File | Lines, roughly | What |
|---|---|---|
| `runner/subdue.rs` | 120 + tests | the byte filter, pure |
| `runner/pty.rs` | 180 + tests | openpty, raw mode, the guard, the read/write loop |
| `runner/mod.rs` | +30 | the `subdued` field, the branch in `run_interactive`, stub recording |

`runner/mod.rs` is 1812 lines, the large majority of it the `#[cfg(test)]` stub runner,
which `CLAUDE.md` explicitly does not count against the ~300-line guidance. The two new
concerns get their own modules regardless: neither is about *choosing* a subprocess, which
is what `runner/mod.rs` is for.

## Testing

**Unit — `subdue.rs`.** Canned transcripts, no terminal:

- an apt progress rewrite (`\r`-heavy) collapsing to one line per final frame
- a dnf line with SGR colour, emitted plain
- `Password: ` with no newline, emitted on a drained read, and the following bytes
  continuing rather than repeating the line
- an OSC 0 window-title attempt, dropped, with the surrounding text intact
- a `?1049h` alternate-screen attempt, dropped
- a sequence split across two `feed` calls mid-escape, parsed as one

**Unit — `runner/mod.rs`.** The stub runner records `subdued` per invocation, so a task
test asserts that apt is subdued and the environment shell is not — the table above
becomes an assertion rather than a convention.

**Unit — degradation.** With no terminal, `subdued: Some(theme)` takes the plain-inherit
path and allocates no pty.

**Manual, on a real terminal.** `riabuild` self-update against apt on Linux, `gh auth
login` on both platforms, and a Ctrl-C mid-`apt-get` confirming the child dies and the
terminal comes back out of raw mode. The pty itself is not covered by `e2e/`, which runs
unattended and therefore takes the degraded path by construction.

## Not in scope

- Subduing the environment shell, `claude`, or `ssh`/`mosh`. That is the developer's
  workspace.
- A streaming path for the capturing methods (`run`, `run_bytes`). Those are silent behind
  `ui.working(...)` and nothing has asked to see them.
- Scrollback management, output caps, or a "show me the full log" affordance. If a subdued
  command fails, its lines are on screen where it printed them.
- Windows. `runner/mod.rs` has a `cfg(not(unix))` arm for `is_executable`; subdued mode
  there is the plain inherit, the same as having no terminal.

# riabuild-agents

**Status:** implemented
**Date:** 2026-08-24

`riabuild agents` runs Claude Code, Codex and Grok Build sessions in one terminal window.
It is reached as `agents` — a generated shim in `~/.riabuild/bin`, which riabuild already
owns and already keeps at the front of `PATH`. All three harnesses open, always. Sessions
live on disk, survive the window closing and the machine restarting, and a turn keeps
running when nobody is watching.

## Why this is in scope

riabuild is a provisioner, and `CLAUDE.md` rules out "agent session sharing" by name. This
is not that. Nothing is shared between developers, nothing is persisted to riabuild-web,
and no session leaves the machine it started on — the store sits under `root()`, which on a
server is the per-developer namespace, so two people on one box cannot see each other's
sessions at all.

What it does is shorten the path riabuild already exists to shorten: riabuild installs
three agent harnesses, writes nineteen launchers for them, and then leaves the developer to
open three terminals and remember which of them is waiting.

The line it must not cross is durability *off this machine*. If a future version syncs
sessions to the dashboard, that is session sharing and belongs to a different product.

## The decision that shaped everything: render, don't embed

Two ways to put three agents in one window.

**Embed.** Run each harness's real TUI in a pty, emulate the terminal, draw the grids side
by side. Perfect fidelity for free and no knowledge of any vendor's protocol.

**Render.** Run each harness in its structured output mode and draw from the events.

Embedding was rejected, and the reason is not fidelity. A pty gives you *pixels*, and this
window's entire purpose is *state* — which agent is blocked, which is running a command,
which has failed, what each has spent. Recovering that from a screen means matching on
somebody's spinner. Three vendors' spinners. It also makes persistence nearly impossible:
what would you replay, and into what?

Two smaller facts confirmed it. `riabuild-runner` already has pty machinery and it is the
wrong shape — `Subdue` is a line-level emulator that deliberately discards the alternate
screen, cursor motion and colour, precisely what a pane would need. And the only turnkey
ratatui pty widget, `tui-term`, drives `vt100` and documents its lifecycle controller as
oneshot-only; the better emulator, `wezterm-term`, **is not published on crates.io**, and a
git dependency in a binary shipped through signed Homebrew, apt and dnf releases is a
supply-chain regression against riabuild's own rules.

The cost accepted is that riabuild owns the rendering, and a harness feature with no
representation in the event stream is invisible here. The developer can always run `claude`
directly, and the launchers that let them do it are untouched.

## Persistence, and the simplification it forced

The requirement: close the window, reopen it, see your agents and carry on — and a turn
must keep working while nobody is watching, across a reboot.

Most of the machinery for the first half already existed. All three harnesses persist their
own transcripts and all three resume by id; the resume argv was written and tested before
any of this. What was missing was only that nothing remembered the id.

The second half — a turn that outlives its window — is what forced the interesting change.

**Detached execution and Claude Code's persistent stdin are incompatible.** A detached
child has nobody left holding the write end of its stdin, so `--input-format stream-json`
sees EOF and Claude Code exits. So every harness now runs **one child per turn**, resumed
by id, and `Restart::Persistent` is gone. The difference between the three collapses to how
they spell resume:

| Harness | Resume |
|---|---|
| Claude Code | `--resume <uuid>` |
| Codex CLI | `exec resume <SESSION_ID> <PROMPT>` |
| Grok Build | `--resume <id>` |

What that costs is process warmth, not context. Verified against Claude Code 2.1.235 by
running it: `claude -p --output-format stream-json --verbose --permission-mode
bypassPermissions --resume <uuid> "…"` answers inside the session that id names.

That simplification paid for the rest. Every turn became fire-and-forget, which made the
spool possible, which made live viewing and rehydration the *same* code path.

## The shape on disk

```
<root>/agents/<session-id>/
  meta.json        harness, account, thread id, profile home, checkout, title, times
  events.ndjson    every turn's stdout, appended, exactly as the harness wrote it
  turn.lock        held by the running turn and by nothing else
  pending/*.txt    prompts waiting for a turn to pick them up
  errors.log       what riabuild itself could not do
```

**The spool is the harness's own bytes, not riabuild's event model.** Replaying it through
the same `Reader` that reads a live turn yields precisely the events the window saw,
because it is the same decoder over the same bytes. Storing decoded events would have meant
a second format to version and a reopened session that could disagree with the one that was
on screen.

**`errors.log` is not redundant with it.** The spool holds one vendor's wire format, so a
line riabuild wrote there would decode to nothing — and a detached wrapper has no stderr
anybody reads. Without a second file, a harness that will not start produces a session that
sits idle for ever with no explanation. The window tails both.

**Prompts are a queue of files, not one file.** Two prompts sent while a turn is running are
two waiting wrappers, and a single `prompt.txt` would have the second overwrite the first
before either had read it — losing a message the developer watched being accepted. They are
files rather than arguments because argv is world-readable through `ps`, and on a shared
server `ps` shows other developers' processes.

## Liveness is a lock, never a pid

Whether a turn is running is answered by trying to take `turn.lock`. This is
`remote::channel::lease`'s decision for the reasons `CLAUDE.md` already sets out: a pid in a
file is a claim somebody has to check, a marker outlives the process that wrote it, and
`kill -0` on a recycled pid answers about the wrong process.

Here it also gets a reboot right for free. The kernel releases an `flock` when the holder
exits however it exits, so after a restart every session reads as idle with nothing having
to clean up after it.

The lock is held by `riabuild internal agent-turn` rather than by the harness, because the
harness is a third-party binary that knows nothing about riabuild. That wrapper exists for
exactly three reasons, none of which a vendor's binary could be asked to do: it holds the
lock, it appends the spool, and it writes down the thread id — without which the next turn
starts a new conversation instead of continuing this one. It is not a supervisor: it runs
one turn, records what happened, and exits.

## Detaching, precisely

`CommandRunner::spawn_detached` is the one method that starts a child riabuild does not stay
responsible for. Three things together, and leaving out any one looks like it works until
somebody closes a terminal:

- **`setsid`**, so the child leads its own session. A terminal that goes away sends
  `SIGHUP` to its foreground process group; being reparented to init is not enough.
- **stdio nulled**, so the child holds no descriptor on the tty.
- **no `kill_on_drop`**, which every other spawn in that crate sets.

It returns no handle, deliberately: a process expected to outlive this one is not something
this one can honestly claim to be able to wait for or kill. Liveness is answered by the
lock instead.

## The profile is recorded, never recomputed

riabuild keeps nine sign-ins for each harness, and a session is only resumable under the one
that created it — each tool stores its transcripts inside its own home. So `meta.json`
carries the `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `GROK_HOME` the session was started with.

This was a latent bug before persistence: the first version of this crate spawned the
binaries with no home at all, so sessions landed in whichever default each tool picked.
Recomputing it would be worse than not storing it — a changed primary Claude account would
point the next turn at a different store, where it finds no session and quietly begins a new
conversation under the same pane, with nothing on screen saying so.

The *binary* is the opposite and is resolved per turn, because a versioned path moves with
every riabuild upgrade: a session started last week must run this week's Claude Code.

## All twenty-seven sign-ins, and three panes

riabuild writes a launcher for every profile it keeps — `claude-1` … `claude-9`, and the
same for Codex and Grok Build — and this window could reach exactly three of them: the first
of each. A developer who had deliberately signed `grok-2` in to a second xAI account had no
way to use it here at all.

So `Request` carries the whole list rather than three homes, `n` opens a chooser over it,
and the session records the **number** beside the home. Both, because they are two different
facts: the home is what a turn runs under, and the number is what the developer calls it.
Every row in the session list is labelled `claude-2` rather than `claude`, which is the only
thing that tells two panes on one harness apart before either has been asked anything.

What the window still does *not* do is open one pane per account. Twenty-seven panes is a
list nobody can read, built for a developer who is using two of them; the rest are one
keypress away instead. The chooser opens on the sign-in the selected session is already
running under, because "another one of these, beside the one that is busy" is the thing
actually asked for most.

Claude's accounts are the list riabuild manages, keyed by uuid — they can be deleted and
renumbered, and position is the number. Codex's and Grok Build's are a fixed nine, so the
number is the directory name. A machine with no Claude account yet still gets a Claude pane,
under whatever home Claude Code picks for itself, which is what every session did before
this window could offer a choice: answering a half-finished setup by hiding a tool would be
worse than the setup.

## The keyboard, and the two keys a laptop does not have

Reading is the resting state. The arrow keys scroll the transcript, `←` goes to the session
column and `↑↓` pick a session only while the keyboard is talking to it, `→` (or enter, or
escape) comes back. The divider between the two carries the focus, because `↑↓` mean two
different things depending on which side of it you are on and that has to be visible without
reading the footer.

It was the other way round, and `PageUp`/`PageDown` were the only way to scroll. On every
laptop keyboard in the room those are a chord — `Fn` and an arrow — which made the main
gesture of this screen one most of its developers could not perform. They still work where
the keys exist; nothing now depends on them.

## Clearing the screen riabuild took

`riabuild agents` claims the terminal by hand — raw mode, alternate screen — rather than
through `ratatui::init`, and so it has to do what that function does: **clear**. Ratatui
writes only the cells that differ from the frame before, and on the first draw "the frame
before" is ratatui's idea of a blank screen rather than the terminal's. Every cell this
interface leaves empty is therefore never written at all, and whatever was on the alternate
screen shows through underneath — old shell history behind a live interface.

Resizing hid it: `autoresize` clears on every size change, so the symptom vanished the first
time anyone dragged the window and never came back. The popup has the same problem in
miniature, which is what `Clear` under it is for.

## Permissions are bypassed, in three spellings

| Harness | Flags |
|---|---|
| Claude Code | `--permission-mode bypassPermissions` |
| Codex | `--dangerously-bypass-approvals-and-sandbox`, `--dangerously-bypass-hook-trust` |
| Grok Build | `--always-approve` |

None is interchangeable. `codex exec` does not accept the `--yolo` the launchers pass, and
`dontAsk` — which exists on two of the three and reads like the same thing — silently
**denies** whatever was not pre-approved, presenting as an agent that refuses its own tools.

There is no approval round-trip anywhere in `riabuild-harness`. That is not an omission: the
three harnesses each ask permission in a different and badly documented way, and never being
asked is what makes one event model possible.

### The one thing full bypass cannot buy

Claude Code's `--bare` would suppress the remaining hook, LSP, plugin and MCP discovery. Its
own `--help` says why it is not passed:

> Anthropic auth is strictly `ANTHROPIC_API_KEY` or `apiKeyHelper` via `--settings` (OAuth
> and keychain are never read).

Every Claude account riabuild manages is an OAuth sign-in, so `--bare` would break all nine.
**Full prompt-suppression and subscription auth are mutually exclusive on that harness.**
riabuild keeps the accounts.

Codex has no such conflict, which is why `--dangerously-bypass-hook-trust` *is* passed: it
grants hooks configured in a checkout riabuild itself cloned, and its absence means a
headless session waits for an interactive trust nobody can give.

## What is verified, and what is inferred

- **Claude Code 2.1.235** — pinned against transcripts captured from the real binary,
  including a resumed turn. Every field the decoder reads is exactly as it was written.
- **codex-cli 0.148.0** — the *envelope* (`thread.started`, `turn.started`,
  `item.completed`, `turn.failed`, top-level `error`) is captured, but only its **failure**
  path: the machine this was written on had no OpenAI sign-in. The successful item bodies
  are documentation, marked `INFERRED`.
- **Grok Build 1.0.5** — only the error frame is captured. Everything else is ACP, marked
  `INFERRED`, and accepts both a bare update and one nested under a JSON-RPC
  `params.update`, because which Grok writes could not be observed.

Two rules fall out and both are load-bearing. **Decoders degrade, never fail**: an
unrecognised frame produces no events rather than an error. And **stdout only**: Codex
writes `tracing` diagnostics to stderr and a plain `Reading additional input from stdin...`
to stdout, so a decoder that merged the streams would die on the first retry a flaky
connection causes.

## Colour: ratatui's types, riabuild's ladder

`riabuild-theme` was rewritten onto ratatui's `Color`, `Style` and `Modifier`, because
riabuild now paints two surfaces — printed lines and drawn frames — and a private `Rgb`
would mean converting at that boundary, which is a second palette by another name.

Ratatui does not replace the crate: it has no notion of terminal capability (its backends
write a `Color::Rgb` as a 24-bit escape whatever is on the other end) and no `NO_COLOR`. So
the depth ladder stays riabuild's and runs *before* a style reaches a frame, and
`Theme::paint` — one styled string for a `println!` — stays riabuild's too.

One finding worth recording: `Role::legacy`, the sixteen-colour rendering, has to be a
**chosen table rather than a nearest-match**. `--green` (`#3ddc84`) is nearer to `Cyan` than
to `Green` on channel distance, and `--orange` lands on `Red` beside `Danger` — so computed
downgrades would put "done" on cyan and make "in progress" and "fatal" the same colour.

## Testing

The whole interface is tested against transcripts three real binaries produced.
`riabuild_harness::testing::decode` runs the **production** decoder over canned bytes, so
`riabuild-agents`' own tests fail when a decoder changes under them — hand-written
`Vec<Event>` fixtures would test the renderer against a fiction.

`app.rs` is pure: no terminal, no process, no IO. `draw.rs` splits line-building from widget
rendering so that what the screen *says* is assertable without a backend. One test walks
every span on every line and fails on a style that is not a `Role` from the palette, which
is the rule ratatui makes easiest to break. `store.rs` and `turn.rs` run against a temporary
directory and a `FakeRunner`.

`FakeRunner` gained `piping`, and its `spawn_piped` now closes the far end of a child
scripted to exit. That is a bug fix as much as a feature: a scripted child whose pipe stayed
open left a reader looping until EOF for ever, which is a hang rather than a failure — and
hangs are what `ci.yml`'s macOS job exists to catch, after one cost a release twenty-five
minutes of a runner building nothing.

## Threading

Keys are read on a dedicated OS thread, not a task. `event::read` blocks, and riabuild runs
a current-thread runtime, so reading on the reactor would hold every session's output behind
whether a developer happens to be typing — the same reason `runner/pty.rs` uses `AsyncFd`.

There is no reader task per session any more. The window polls the spools and the locks on
its redraw tick, which is simpler than a channel *and* is the only thing that works: the
turns are not this process's children, and there is no pipe to hold.

## Open questions

- **`codex app-server` and `grok agent stdio`.** Both give real interrupts and steering,
  which per-turn respawning cannot. Blocked on their schemas stabilising; both are marked
  experimental or beta by their own vendors. The decoders are already written against ACP's
  own names so that adopting one replaces a transport and keeps a decoder.
- ~~**Cross-provider delegation.**~~ **Answered, 2026-09-04**, by the mechanism this
  paragraph guessed at: `riabuild internal mcp-codex` is an MCP server that opens a session
  here, and Claude Code reaches Codex through it. The event model was indeed not the hard
  part — the hard part was that MCP carries no way for a server to ask which session is
  calling it, which `RIABUILD_AGENT_SESSION` in the turn's environment answers. Sessions now
  record a `parent` and the rail draws one level of children. `Event::Delegated` is
  unchanged and still means Claude's own subagents inside one pane; the two do not interact.
  See `2026-09-04-codex-subagents-design.md`.
- **Interrupts.** There is no key that stops a running turn. Doing it properly means killing
  a process this window deliberately does not own, which needs the lock to carry something
  more than "held". Left out rather than half-built.
- **`riabuild agents list|forget`.** The store has both operations and retention is enforced
  on open (fifty per checkout). Neither is exposed as a command yet.

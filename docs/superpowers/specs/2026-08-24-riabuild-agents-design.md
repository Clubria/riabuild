# riabuild-agents

**Status:** implemented
**Date:** 2026-08-24

`riabuild agents` runs Claude Code, Codex and Grok Build sessions in one terminal window.
It is reached as `agents` — a generated shim in `~/.riabuild/bin`, which riabuild already
owns and already keeps at the front of `PATH`.

## Why this is in scope

riabuild is a provisioner, and `CLAUDE.md` rules out "agent session sharing" by name. This
is not that. Nothing here is shared between developers, nothing is persisted to
riabuild-web, and no session leaves the laptop it started on. What it does is shorten the
path riabuild already exists to shorten: riabuild installs three agent harnesses, writes
nineteen launchers for them, and then leaves the developer to open three terminals and
remember which of them is waiting. One window that says which agent is blocked is the
provisioner finishing its own sentence.

The line it must not cross is durability. If a future version starts recording sessions
somewhere a colleague can open them, that is session sharing and belongs to a different
product.

## The decision that shaped everything: render, don't embed

Two ways to put three agents in one window.

**Embed.** Run each harness's real TUI in a pty, emulate the terminal, draw the grids side
by side. Perfect fidelity for free, survives upstream changes, and needs no knowledge of
any vendor's protocol.

**Render.** Run each harness in its structured output mode and draw from the events.

Embedding was rejected, and the reason is not fidelity. It is that a pty gives you *pixels*
and this window's entire purpose is *state* — which agent is blocked, which is running a
command, which has failed, what each has spent. Recovering that from a screen means
matching on somebody's spinner. Three vendors' spinners.

Two smaller facts confirmed it. `riabuild-runner` already has the pty machinery, and it is
the wrong shape: `Subdue` is a line-level emulator that deliberately discards the alternate
screen, cursor motion and colour — precisely what a pane would need. And the only turnkey
ratatui pty widget, `tui-term`, drives `vt100` and documents its lifecycle controller as
oneshot-only, which is not what a long-lived agent session is. The better emulator,
`wezterm-term`, **is not published on crates.io**: a git dependency, in a binary shipped
through signed Homebrew, apt and dnf releases, is a supply-chain regression against
riabuild's own rules.

The cost accepted is that riabuild owns the rendering, and that a harness feature with no
representation in the event stream is invisible here. That is the trade, and it is the
right way round: the developer can always run `claude` directly, and the launchers that let
them do it are untouched.

## The transport genuinely differs per harness

This was the working hypothesis and it turned out to be stronger than expected. The three
vendors did not merely diverge; they answered the same question three incompatible ways,
and none is a superset.

| Harness | Session shape | Continuity |
|---|---|---|
| Claude Code 2.1.235 | one process, many turns | `--input-format stream-json` never closes stdin |
| codex-cli 0.148.0 | one process per turn | `codex exec resume <SESSION_ID> <PROMPT>` |
| Grok Build 1.0.5 | one process per turn | `--resume <id>` |

So `Kind::restart` is a field, not three code paths, and the fleet reads a child exiting as
the end of a *turn* for two of them and the end of the *session* for one. Modelling Codex
as persistent produces an agent that appears to die after every reply — a bug that looks
like a crash and is a category error.

Both per-turn harnesses also speak a persistent protocol — `codex app-server` (JSON-RPC
2.0) and `grok agent stdio` (native ACP) — and either would be better than respawning.
Neither is used yet, deliberately: `codex --help` marks `app-server` `[experimental]` and
OpenAI documents it as changing without notice, and `grok agent stdio` is beta with
reported gaps in resume and managed MCP injection. Both are schema-per-version, so adopting
one means generating types against a pinned release. That is worth doing and is a separate
change. The decoders are already written against ACP's own discriminant names for exactly
this reason: when `grok agent stdio` stabilises, the transport is replaced and the decoder
is kept.

## Permissions are bypassed, in three spellings

riabuild provisions "agents can do anything" environments, and its launchers already say
so. This crate spawns the harnesses directly — it needs argv the launchers do not pass — so
it restates the bypass per harness:

| Harness | Flags |
|---|---|
| Claude Code | `--permission-mode bypassPermissions` |
| Codex | `--dangerously-bypass-approvals-and-sandbox`, `--dangerously-bypass-hook-trust` |
| Grok Build | `--always-approve` |

None is interchangeable. `codex exec` does not accept the `--yolo` the launchers pass;
Grok's `--permission-mode` is a root option only and would be `unexpected argument` after
the `-p` this always passes. And `dontAsk`, which exists on two of the three and reads like
the same thing, silently **denies** whatever was not pre-approved — an agent that refuses
its own tools.

The consequence worth stating plainly: there is no approval round-trip anywhere in
`riabuild-harness`. That is not an omission, it is what makes one event model possible. The
three harnesses each ask permission in a different and badly documented way, and never
being asked removes the hardest part of driving any of them headless.

### The one thing full bypass cannot buy

Claude Code's `--bare` is the flag that would suppress the remaining hook, LSP, plugin and
MCP discovery. It is not passed, and its own `--help` says why:

> Anthropic auth is strictly `ANTHROPIC_API_KEY` or `apiKeyHelper` via `--settings` (OAuth
> and keychain are never read).

Every Claude account riabuild manages is an OAuth sign-in, so `--bare` would break all
nine. **Full prompt-suppression and subscription auth are mutually exclusive on that
harness.** riabuild keeps the accounts. If a deployment ever wants `--bare` instead, it
needs `ANTHROPIC_API_KEY`, and that is a decision about billing rather than about this
window.

Codex has no such conflict, which is why `--dangerously-bypass-hook-trust` *is* passed:
it grants hooks configured in a checkout riabuild itself cloned, and its absence means a
headless session waits for an interactive trust nobody can give.

## What is verified, and what is inferred

Every wire format here is undocumented, explicitly unstable, or both. The response is to
say which is which, at each match arm.

- **Claude Code** — pinned against a transcript captured from 2.1.235 by running the real
  binary. Every field the decoder reads is exactly as it was written.
- **Codex** — the *envelope* (`thread.started`, `turn.started`, `item.completed`,
  `turn.failed`, top-level `error`) is captured from 0.148.0, but only its **failure**
  path: the machine this was written on had no OpenAI sign-in. The successful item bodies
  are from documentation and marked `INFERRED`.
- **Grok Build** — only the error frame is captured from 1.0.5, from a machine with no xAI
  sign-in. Everything else is ACP, marked `INFERRED`, and accepts both a bare update and
  one nested under a JSON-RPC `params.update`, because which Grok writes could not be
  observed.

Two rules fall out and both are load-bearing. **Decoders degrade, never fail**: an
unrecognised frame produces no events rather than an error, so a schema that moves under us
costs a line of transcript instead of a session. And **stdout only**: Codex writes
`tracing` diagnostics to stderr and a plain `Reading additional input from stdin...` to
stdout, so a decoder that merged the streams would die on the first retry a flaky
connection causes, and one that treated non-JSON as fatal would kill every Codex session at
the moment it started.

## Colour: ratatui's types, riabuild's ladder

`riabuild-theme` was rewritten onto ratatui's `Color`, `Style` and `Modifier`. riabuild now
paints two surfaces — printed lines and drawn frames — and a private `Rgb` would mean
converting at that boundary, which is a second palette by another name.

Ratatui does not replace the crate, because it has no notion of terminal capability: its
backends write a `Color::Rgb` as a 24-bit escape whatever is on the other end, and it has
no `NO_COLOR`. So the depth ladder stays riabuild's and runs *before* a style reaches a
frame, and `Theme::paint` — one styled string for a `println!` — stays riabuild's too,
because ratatui only ever writes whole frames.

One finding worth recording: `Role::legacy`, the sixteen-colour rendering, has to be a
**chosen table rather than a nearest-match**. `--green` (`#3ddc84`) is nearer to `Cyan`
than to `Green` on channel distance, and `--orange` lands on `Red` beside `Danger` — so
computed downgrades would put "done" on cyan and make "in progress" and "fatal" the same
colour.

## Testing

The whole interface is tested against transcripts three real binaries produced.
`riabuild_harness::testing::decode` runs the **production** decoder over canned bytes, so
`riabuild-agents`' own tests fail when a decoder changes under them — the alternative,
hand-written `Vec<Event>` fixtures, would test the renderer against a fiction.

`app.rs` is pure: no terminal, no process, no IO. `draw.rs` splits line-building from
widget rendering so that what the screen *says* is assertable without a backend. One test
walks every span on every line and fails on a style that is not a `Role` from the palette,
which is the rule ratatui makes easiest to break.

## Threading

Keys are read on a dedicated OS thread, not a task. `event::read` blocks, and riabuild runs
a current-thread runtime, so reading on the reactor would hold every session's output
behind whether a developer happens to be typing — the same reason `runner/pty.rs` uses
`AsyncFd`. Each session's stdout is pumped by its own task reporting into one unbounded
channel, so the draw loop awaits a single receiver rather than N children.

The thread is detached: it ends with the process, parked in a `read` only the terminal can
complete. That is acceptable because `agents` is a command that owns the process for its
lifetime, and it is the reason this crate would need revisiting before ever being embedded
in something longer-lived.

## Open questions

- **`codex app-server` and `grok agent stdio`.** Both give real interrupts and steering,
  which per-turn respawning cannot. Blocked on their schemas stabilising.
- **Cross-provider delegation.** `Event::Delegated` exists and Claude Code populates it
  through `parent_tool_use_id`, which is the only attribution any of the three emits. An
  agent in one provider spawning a subagent in another would be an MCP server that opens a
  session here. Nothing is built for it yet, and the event model is the part that would
  have been hard.
- **Interrupts.** There is no key that stops a running turn. Claude Code takes a
  `control_request` interrupt; the other two would have to be killed. Deliberately left
  out rather than half-built.

# The laptop channel, and the clipboard — Design

**Date:** 2026-08-07
**Status:** Draft
**Extends:** [`2026-08-04-riabuild-design.md`](2026-08-04-riabuild-design.md),
[`2026-08-06-remote-mode-design.md`](2026-08-06-remote-mode-design.md)

**Depends on remote mode**, which is unmerged at the time of writing. This is PR D: it
needs `src/remote/` to exist, the SSH identity to be established, and the shell to be
running before there is anything for a channel to attach to. Nothing here changes remote
mode's three PRs.

## Purpose

A developer working on a server through `riabuild remote` cannot paste. They copy a
screenshot on their laptop, press Ctrl+V in Claude Code, and are told there is no image in
the clipboard — because the clipboard Claude Code read is the server's, and a headless
server has none.

This adds a **general-purpose request channel from the server back to the laptop**, and
makes the clipboard its first consumer. The goal is not "get images across." It is
**parity with sitting at the laptop**: every clipboard type, read the same way, with paste
behaving exactly as it does locally and no new gesture to learn.

## What this is, and is not

**The server asks; the laptop decides.** The channel carries a fixed, compiled-in set of
request operations. The laptop answers them or refuses. A server cannot push work at a
laptop, cannot extend the operation set, and cannot execute anything. This is the
architecture rule "the server ships data, never logic" applied to the one direction remote
mode had not yet opened — and it is the reason a reverse tunnel is defensible at all.

**The channel is strictly optional.** Its absence degrades to "no clipboard" and never to
"environment broken." A laptop that closes its lid leaves a mosh session that still runs
setup, still re-pulls rotated secrets, and still opens a shell. Only paste stops working.

**Reads only.** The server never writes to the laptop's clipboard. `clipboard.write` is a
coherent operation and the protocol has room for it, but adding it inverts the trust
direction — the server would be putting data into the laptop rather than answering a
question — and it is out of scope. See *Not in scope*.

**The transport is riabuild's, not a third party's.** No `lemonade`, no `clipper`, no
`isomorphic-copy`. Those are useful prior art (and `isomorphic-copy` validates the
PATH-shim architecture, though it is text-only), but shipping a third-party binary
conflicts with the digest-verified tools rule, and their transports bind loopback TCP,
which every co-tenant on the server can reach.

## The invariant this amends

`2026-08-06-remote-mode-design.md` says:

> No browser on the server, no keyring on the server, **no SSH forwarding**, no broker
> process. The server can re-run `riabuild` on its own afterwards — including re-pulling
> rotated secrets mid-session — which is what makes the mosh shell self-sufficient once the
> laptop disconnects.

That sentence is about the **credential path**, and its purpose is self-sufficiency. This
design adds forwarding, so the amendment is to be written into the remote-mode spec in this
change:

> A riabuild-managed server may hold **one forwarded unix socket** for the laptop channel.
> It carries no credentials, it is owned by the laptop's session, and every property the
> server depends on — setup, secrets, the shell — continues to work when it is gone.

What that sentence existed to protect is unmoved. No credential crosses the channel. The
Infisical credential is still brokered per use; the session token is still minted by the
laptop and revocable; the mosh shell is still self-sufficient. The channel is an ergonomic
attached to a session, not a dependency of one.

---

# The developer's experience

There isn't one, and that is the whole point.

```
$ riabuild remote
…
Checking build-01
  ● Node 22 · pnpm · checkout · Claude Code
  ● Clipboard channel — connected

ada@build-01 ~/Clubria/ai-builders-hub $ c
```

Copy a screenshot on the laptop. Press Ctrl+V in Claude Code on the server. The image
attaches. Copy a paragraph of HTML out of a browser, paste it into a file — the HTML
arrives as HTML.

When the laptop goes away, the banner and `riabuild channel status` say so. Paste stops
working, and nothing else does.

---

# Architecture

Four components, two of which are the same binary invoked differently.

```
LAPTOP                                          SERVER
──────                                          ──────
riabuild channel agent                          Claude Code (or any program)
  listens on agent.sock                           │ runs: xclip -selection clipboard -t TARGETS -o
  serves a compiled-in op allowlist               ▼
  reads the local clipboard via CommandRunner   ~/.riabuild/bin/xclip     ← the shim
  ▲                                               │ connects to
  │                                               ▼
  └──── ssh -N -R (supervised) ◄────────── <runtime>/channel.sock   (0700 dir)
                                                  │
                                                  │ ← {"ok":true,"targets":["image/png", …]}
                                                  ▼
                                                stdout, in xclip's own format
```

The paste path, end to end:

1. Claude Code runs `xclip -selection clipboard -t TARGETS -o`.
2. The shim opens `channel.sock` and sends `{"v":1,"op":"clipboard.targets"}`.
3. `ssh -R` carries it to the laptop agent, which reads the **laptop's** clipboard.
4. The reply comes back; the shim prints the target list in xclip's format.
5. Claude Code greps it, matches, and runs `xclip … -t image/png -o`. Same round trip,
   raw bytes on stdout.
6. Claude Code attaches the image. It never learns any of this happened.

Two round trips per paste, so roughly `2 × RTT` plus transfer. On a normal laptop-to-cloud
link that is imperceptible.

## Why a unix socket, and not a port

`ssh -R 9998:localhost:9998` — the shape every prior-art tool uses — binds a **loopback TCP
port on the server**, which every other user on that box can connect to. The remote-mode
spec already has a trust boundary about developers sharing a machine; a TCP port would put
"read this developer's laptop clipboard on demand" inside it.

A forwarded unix socket lives in the namespace's existing runtime directory, which is
already `0700` and already ownership- and symlink-checked. Filesystem permissions do the
gating, and the check is one riabuild already performs. OpenSSH has supported unix-socket
remote forwarding since 6.7; servers older than that are a hard stop rather than a TCP
fallback, because the fallback is the thing being avoided.

## The protocol

Newline-delimited JSON requests; a JSON header line then raw bytes for binary responses.

```
→ {"v":1,"op":"clipboard.targets"}
← {"ok":true,"targets":["image/png","text/html","text/plain;charset=utf-8"]}

→ {"v":1,"op":"clipboard.read","mime":"image/png"}
← {"ok":true,"len":184320}
  <184320 raw bytes>

→ {"v":1,"op":"channel.ping"}
← {"ok":true}

← {"ok":false,"code":"unavailable","message":"no clipboard content of that type"}
```

Responses are length-prefixed and streamed rather than base64-encoded: a screenshot is
routinely 2–15 MB, and base64 would inflate it by a third for no benefit. Hard cap of
32 MB, with a legible failure past it.

Operations are namespaced so a future consumer — notifications, opening a URL on the
laptop, pulling a file — slots into the same tunnel rather than needing a second one. The
allowlist is compiled into the binary. **A server can request only what the laptop's
binary already implements.**

---

# The shim contract

The shim's job is to be indistinguishable from the real tool, and to get out of the way for
anything it does not handle.

| Invocation | Behaviour |
|---|---|
| `xclip -selection clipboard -t TARGETS -o` | one MIME per line, laptop's list, filtered and ordered (below) |
| `xclip -selection clipboard -t <mime> -o` | raw bytes, exit 0 |
| `wl-paste -l` / `wl-paste -t <mime>` | the same two operations in `wl-paste`'s vocabulary |
| `xclip -selection clipboard -o`, `wl-paste` with no `-t` | no type requested: serves the **first type in preference order**, which is the preferred text flavour when text is present |
| `xclip -o` with no `-selection` | xclip's default selection is **PRIMARY**, not CLIPBOARD — returns empty (see *Not in scope*) |
| clipboard genuinely empty | empty stdout, exit 1 — what the real tool does |
| channel unavailable | **also** empty stdout, exit 1; the reason goes to stderr and a log file |
| anything else (writes, odd flags) | `exec` the real binary |

## Type vocabularies

The three platforms disagree on what a clipboard type is called. The agent normalizes to
MIME; the shim translates MIME back into its own tool's vocabulary; `protocol.rs` owns the
table. The macOS column is a **laptop** platform — the primary case — and is unrelated to
macOS as a server, which is out of scope below.

| Concept | macOS pasteboard | X11 (`xclip`) | Wayland (`wl-paste`) |
|---|---|---|---|
| UTF-8 text | `public.utf8-plain-text` | `UTF8_STRING`, `STRING`, `TEXT` | `text/plain;charset=utf-8` |
| HTML | `public.html` | `text/html` | `text/html` |
| PNG | `public.png` | `image/png` | `image/png` |
| TIFF | `public.tiff` | `image/tiff` | `image/tiff` |

Without this layer, `text/html` copied out of Safari does not exist under any name `xclip`
recognises.

## Ordering, and what is dropped

`TARGETS` is not a passthrough of what the laptop reports. The agent applies two rules:

**Preference order.** `image/png` before `image/tiff`; `text/plain;charset=utf-8` before
the legacy `STRING`/`TEXT` atoms. Callers commonly take the first match, so order is a
functional decision, not cosmetics.

**Redundant representations are dropped, not deprioritised.** macOS puts a screenshot on
the pasteboard as *both* PNG and uncompressed TIFF, and the TIFF can be 40 MB for pixels
already available losslessly. Ordering alone is insufficient — a caller that walks the
whole list can still choose it — so when a PNG representation exists, TIFF is omitted.

**File references are dropped entirely.** `text/uri-list`, `public.file-url`, and the
file-reference flavours never appear. Copying a file in Finder puts a *path* on the
pasteboard; bridged verbatim the server receives `file:///Users/ada/Desktop/report.pdf`,
which does not exist there. It is the one payload that is syntactically valid and
semantically false on the far side, and it is exactly the kind of thing Claude will
confidently try to read.

This exclusion is **type-level only**. A laptop path copied as plain text is
byte-identical to any other string; scanning text content for path-shaped substrings would
corrupt legitimate text to prevent a case the developer chose deliberately. Types are
filtered; content is never inspected.

## Two failure modes worth naming

**PATH recursion.** The shim resolves the real binary by searching `PATH` — but
`~/.riabuild/bin` *is* on `PATH`, ahead of everything. Searched naively, the shim `exec`s
itself forever. It must strip its own directory before resolving. This is the single most
likely way to hard-hang a developer's server and it is one line to get wrong.

**The two-call race.** A paste is two round trips. If the clipboard changes between
`TARGETS` and the read, the second call finds nothing and the paste fails for no visible
reason. The agent caches the content it reported for a few seconds and serves the
follow-up read from that snapshot, so a paste is atomic with respect to what the developer
had copied when they pressed Ctrl+V.

## Compression, and why there is none

Bytes cross the wire as-is. PNG is already compressed, so `ssh -C` or a gzip layer buys
almost nothing on the payload that dominates.

The lever that would matter is **downscaling**, and it is applied exactly once, at a
constant compiled into the binary:

```rust
/// The long-edge ceiling Claude's vision applies before it looks at an image.
/// Resizing to this is information-neutral: the detail discarded here is the
/// detail the model was going to discard anyway.
const MAX_LONG_EDGE: u32 = 2576;
```

This is **not a setting**. There is no config key, no environment variable, and no
dashboard field — riabuild does not ask a developer to pick a resolution any more than it
asks them to pick a Node version. Changing the number is a release.

The value is chosen so that the usual objection to downscaling does not apply. The most
common laptop-origin paste is a dense error dialog or a UI with small text, and
aggressive downscaling is what makes those unreadable — but Claude's vision already
resizes anything above this ceiling and discards the excess. Sending a 5K screenshot
uncompressed costs several times the transfer time of a 2576 px one and yields the model
no additional pixels. Resizing *to the ceiling* loses nothing the model would have seen;
resizing below it would, which is why the constant sits at the ceiling and not under it.

Two consequences worth stating plainly, because both are real:

- Images at or below 2576 px on the long edge are passed through **byte-for-byte** and
  never decoded. The resize path is only entered by images that exceed the ceiling.
- An oversized image is re-encoded, so a developer who runs `wl-paste > shot.png` on the
  server to archive an original gets the resized copy, not the original bytes. This is the
  one place the channel is deliberately not byte-transparent. It is accepted because the
  channel exists to feed Claude Code, and a developer who needs original pixels has `scp`.

Resizing belongs in the **agent**, not the shim. The shim runs after the bytes have
already crossed the wire, so resizing there would save tokens but not transfer time —
which is the whole point.

---

# Transport and resilience

The requirement is mosh-grade: recover whenever the channel drops *or goes quiet for too
long*. Three mechanisms, because each catches what the others miss.

| Mechanism | Catches |
|---|---|
| Supervisor: `ssh -N -R` as a supervised child, rebuilt with jittered backoff (1 s → 30 s) | clean exits — the connection died and said so |
| `ServerAliveInterval=15`, `ServerAliveCountMax=3` | black-hole networks: converts silence into an exit the supervisor can see, in ~45 s |
| `channel.ping` every 30 s, teardown after two misses | **half-open sockets** — SSH believes the connection is fine while the forward is wedged. Keepalives run below the forward and cannot see this |

Two options are load-bearing rather than tuning:

- **`ExitOnForwardFailure=yes`** — without it, a forward that fails to bind leaves a live
  connection forwarding nothing, and the failure is invisible.
- **`StreamLocalBindUnlink=yes`** — without it, a socket left by a killed session blocks
  the rebind and the channel comes up permanently dead. A socket owned by *another uid* is
  a hard stop and is never unlinked.

The supervisor lives on the laptop, because the laptop is the side that holds the identity
and the side that comes and goes. The server end is entirely passive.

## Lifetime

One channel per remote namespace, shared by every shell into that server. Lifetime is
refcounted with the **same `sessions/<pid>` markers and `kill -0` sweep** the GitHub
credential already uses — not a second mechanism. The socket lives in the namespace's
runtime directory and inherits its `0700`, ownership, and symlink rules unchanged.

Two terminals into one server share one channel. The first to exit tears down nothing; the
last tears down the tunnel.

## Degradation

When the laptop disconnects, the tunnel dies and the mosh session survives — by design, and
identically to how remote mode already treats a departed laptop. The server must remain
fully functional: setup re-runs, secrets re-pull, `riabuild shell` works, Claude Code runs.
Only paste stops.

This is the property that reconciles the design with the amended invariant, so it is a
test, not an aspiration. See *Testing*.

---

# Trust boundary

**No credential crosses the channel.** It carries clipboard content in one direction, in
response to a request. The session token, the GitHub credential, and the brokered Infisical
token all continue to travel the paths remote mode already defined.

**The socket is gated by the filesystem.** It sits inside the `0700` runtime directory
riabuild already validates for ownership and symlinks. Co-tenants on the server cannot
reach it. This is strictly stronger than the loopback TCP port the prior art uses.

**A compromised server can read the clipboard while the channel is up.** This is the real
exposure and it should be stated plainly rather than engineered around. Two things bound
it: the request must originate from a process running as the developer's own uid, and
nothing moves unless something asks — a laptop that is not being read from transmits
nothing.

It is also the exposure the developer already accepts by pointing `riabuild remote` at that
machine: it holds the checkout, a live session token, and brokered secrets. A clipboard
read is a smaller surface than any of those. On the laptop itself, every program the
developer runs can already read the clipboard; bridging restores that property rather than
creating a new class of access.

---

# Failure modes

Each has its own remedy, so each is detected separately.

| What went wrong | What riabuild says |
|---|---|
| Server refuses socket forwarding (`AllowStreamLocalForwarding no`) | hard stop naming the `sshd_config` directive — the failure nobody can diagnose from the symptom |
| Server's OpenSSH predates 6.7 | hard stop; no TCP fallback, because loopback TCP is what the socket exists to avoid |
| Stale socket from a killed session | ours is unlinked; one owned by another uid is a hard stop |
| Runtime directory not ours, or not 0700 | hard stop — the existing rule, unchanged |
| Laptop has no clipboard tool (Linux laptop, no `wl-clipboard`/`xclip`) | names the install command, as `mosh-server` already does |
| Channel down — lid closed, laptop gone | the shim exits 1 silently; the reason is in the shell banner, `riabuild channel status`, and the log |
| Clipboard genuinely empty | identical exit 1 — deliberately indistinguishable to the caller |
| Payload over 32 MB | the cap and the type that exceeded it |
| Type vanished between `TARGETS` and read | served from the snapshot; a genuine miss is exit 1 |
| Server-side `xclip` invoked for a write or an odd flag | `exec`s the real binary; if absent, reproduces xclip's own usage error |

The two middle rows are a decision, not an oversight. The caller's contract is xclip's
contract, and xclip has no way to say "your laptop is asleep" — Claude Code additionally
runs its probe with `2>/dev/null`, so the shim's stderr is discarded. **All diagnostic
value must live outside the paste path.**

---

# Code layout

The channel is not remote-flow-specific — the shim runs on the server, the agent on the
laptop — so it gets its own module rather than growing `src/remote/`.

```
src/channel/
  protocol.rs   request/response types, framing, the op allowlist, the MIME table
  agent.rs      laptop side: serves ops via CommandRunner, snapshot cache, target filtering
  supervisor.rs keeps ssh -N -R alive: backoff, ping, teardown
  client.rs     server side: connect, one request, one response
src/shims/
  clipboard.rs  the xclip and wl-paste shims, including pass-through and the PATH guard
```

`src/remote/mod.rs` gains only the wiring that starts the supervisor as part of the flow.
One concern per file, as `riabuild-cli/CLAUDE.md` requires, none near 300 lines.

Every subprocess goes through `CommandRunner`, without exception. That is what makes the
whole channel testable with no server and no second machine anywhere.

---

# Testing

| Layer | Approach |
|---|---|
| Protocol framing | pure functions: length prefix, truncated frame, oversize payload, malformed JSON |
| MIME normalisation | table-driven across all three vocabularies, both directions |
| Target filtering | TIFF dropped when PNG present; preference order asserted; **no file-URL type survives any input** |
| Shim argv parsing | every documented `xclip`/`wl-paste` invocation → correct op or pass-through |
| PATH recursion | a shim whose own directory leads `PATH` resolves the real binary and never itself |
| Down vs. empty | both produce empty stdout and exit 1, distinguishable only in the log |
| Two-call snapshot | `TARGETS`, clipboard mutated, then read → serves the snapshot |
| Supervisor | scripted `ssh` exits assert the backoff schedule and jitter bounds |
| Ping timeout | an agent that stops answering `channel.ping` is torn down and rebuilt |
| Forward refused | a canned sshd refusal produces the `AllowStreamLocalForwarding` message, not a generic connect error |
| Stale socket safety | a pre-existing socket owned by another uid is refused, never unlinked |
| Refcounting | two shells share one channel; the first to exit tears down nothing, the second tears down |
| **Degradation** | container test: the tunnel is killed mid-session; setup re-runs, secrets re-pull, the shell works, and only clipboard fails |
| End to end | the `e2e/remote` sshd container plus a laptop-side agent: a PNG and a UTF-8 string paste through a real shim |

The degradation row earns its cost for the same reason remote mode's namespace test does:
it is the executable form of the amended invariant. Without it, "the channel is strictly
optional" is a sentence in a document rather than a property of the system.

---

# Not in scope

- **Writing to the laptop's clipboard.** `clipboard.write` inverts the trust direction —
  the server pushing data rather than answering a request — and weakens the property that
  makes the channel defensible. A separate change if it is ever wanted.
- **Compression, and any downscaling below the model's ceiling.** Bytes cross as-is;
  the sole transform is the compiled-in `MAX_LONG_EDGE` resize, and it is not a setting.
- **Content inspection.** File *types* are filtered; text content is never scanned for
  paths or anything else.
- **PRIMARY selection.** The X11 highlight buffer changes on every mouse drag; bridging it
  is a firehose for no benefit.
- **Windows laptops.** Consistent with remote mode's platform set.
- **macOS servers**, until the shim surface is verified there. Claude Code's clipboard
  probe is confirmed to shell out to `xclip`/`wl-paste` on Linux; whether macOS reads the
  pasteboard through a subprocess or a native API is unverified, and if it is native there
  is nothing to shadow. Linux servers ship first; macOS is a follow-up that begins with
  that measurement.
- **Any other consumer of the channel.** Notifications, opening URLs, file pulls are all
  natural fits for the op namespace and none are built here.

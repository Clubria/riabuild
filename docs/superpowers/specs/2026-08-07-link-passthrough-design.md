# Opening links on the laptop

**Date:** 2026-08-07
**Status:** Implemented
**Extends:**
[`2026-08-07-clipboard-channel-design.md`](2026-08-07-clipboard-channel-design.md)

A remote Claude Code session that wants to open a URL has two bad options today: render
it in a terminal browser on the server, or print it and make the developer copy it across
by hand. This adds a third — the server asks the laptop to open it, over the channel the
clipboard already uses.

Companion to `2026-08-07-clipboard-channel-design.md`. That document built the channel;
this one adds the second operation to it.

---

# The problem

`~/.riabuild/bin` leads `PATH` on the server, and `xdg-open` on a headless box resolves
through `/etc/mailcap` to whatever text browser is installed. A login URL then renders
w3m or lynx **inside the session's own TTY**, over the top of Claude Code.

Printing the URL instead is what Claude Code does unprompted on a headless server, and it
is survivable but not good: every `gh auth login`, every Claude Code sign-in, every
`infisical login` becomes a select-and-paste across machines.

The laptop already has a browser, and riabuild already has a request path to it.

## Why the obvious fix is not enough

Shadowing `xdg-open` in `bin/` is the reflex, and it does not work for the caller that
matters most. Claude Code decides whether to open anything at all before it ever resolves
a command:

```js
async function openUrl(url) {
  let browser = attacherCaps()?.browser ?? env.BROWSER
  if (!browser && isHeadlessLinux()) return { ok: false, reason: "no_display" }
  return classify(await spawn(browser || "xdg-open", [url]))
}
function isHeadlessLinux() { return platform() === "linux" && !env.DISPLAY && !env.WAYLAND_DISPLAY }
```

On a headless server with `BROWSER` unset, that returns `no_display` and **never execs
`xdg-open`**. A shim alone is unreachable from Claude Code.

So the design needs both, for different callers:

| Caller | Reached by |
|---|---|
| Claude Code | `BROWSER` pointing at the shim |
| `gh`, `infisical`, anything else | `xdg-open` earlier on `PATH` |

Both point at the same script, so there is one implementation and one code path.

---

# Shape

```
server                                            laptop
──────                                            ──────
Claude Code ──BROWSER──┐
                       ├──► ~/.riabuild/bin/xdg-open
gh auth login ──PATH───┘         │
                                 │ exec <tools>/riabuild/<version>/riabuild channel open <url>
                                 ▼
                          channel.sock ──ssh -R──► agent ──► open(1) / xdg-open(1)
```

**The shim names riabuild in full, and a bare `riabuild` is a bug.** riabuild is the one
tool riabuild does not put on `PATH` — `shell::riabuild_path_dirs` leads with `bin/` and
Node's `bin/` and nothing else, while the binary itself sits in a versioned directory that
only the invocation which started the session names. So the bare name this diagram once
carried resolved to whatever *else* was called riabuild on the box: on a server with no
system copy, nothing, and `$BROWSER` failed with `xdg-open: exec: riabuild: not found`
rather than opening anything; on a server with an apt or Homebrew copy it was worse,
because it worked — as a different version, against a channel this session owns.
`shims::running_binary` is where the path comes from, and it is `/proc/self/exe`, so it
survives the developer's `PATH`, the `claude` launcher's `PATH` strip, and a `$BROWSER`
spawned from a process that sanitised its environment.

The server asks; the laptop decides. `browser.open` joins `clipboard.*` in the compiled-in
operation set, and like every other operation it is a name the laptop's binary already
implements rather than anything the server can extend.

## The protocol operation

One new request and one new response:

```rust
Request::OpenUrl { url: String }
Response::Opened
```

On the wire, `{"v":1,"op":"browser.open","url":"https://github.com/login/device"}`.

`RequestLine` gains an optional `url`; `ResponseLine` gains an optional `opened`. The
`opened` discriminator is needed rather than reusing bare `ok`, because `decode_response`
already treats a header with no other field as `Pong` — a reply that meant "opened" would
otherwise decode as a ping answer.

## The scheme allowlist

`http` and `https` only, enforced in `decode_request` **before the request becomes a
`Request` at all**, and refused as `ProtocolError::UnsupportedScheme`.

This is the security boundary, and it belongs on the laptop because that is the side with
something to lose. `open(1)` on macOS dispatches by scheme to whatever application claims
it: `file://` reaches the filesystem, and `vscode://`, `slack://`, `zoommtg://` and every
other registered handler reach a local application with an attacker-chosen payload. A URL
that reaches this operation was chosen by a model reading a repository, so "the server
would not send that" is not an assumption available to us.

`riabuild channel open` re-checks the same rule server-side, through the same
`protocol::is_openable` function, so a bad URL produces a clear local message instead of a
round trip. The laptop's copy is the authoritative one; the server's is a courtesy.

## Trust model

The laptop opens an allowed URL **silently and logs it**. No prompt.

This matches the operation next to it: `clipboard.read` already hands the server the
contents of the laptop's clipboard with no confirmation, and a channel that asks
permission per URL turns a device-code login into a two-machine dance. The channel log is
the audit trail, and the log line is written before the opener runs, so a URL that hangs a
browser is still recorded.

---

# Degradation

The clipboard channel's rule is that its absence degrades to "no clipboard" and never to
"environment broken". The equivalent here is that its absence degrades to "the URL is
printed" and never to "a text browser has taken over the terminal".

| Condition | Behaviour |
|---|---|
| No `RIABUILD_CHANNEL_SOCKET` | log, exit 1 |
| Channel down or agent refuses | log, exit 1 |
| Scheme not `http`/`https` | log, exit 1 |
| Laptop opened it | exit 0, silent |

**Never falls through to the real `xdg-open`.** This is the one place the browser shim
deliberately diverges from the clipboard shim, which passes unhandled invocations to the
real binary. Passing through here would run the text browser this feature exists to
prevent, so a channel that cannot serve the request fails instead.

## Why the fallback is an exit code and not a message

Claude Code runs the browser command through its subprocess helper, which **captures**
stdout rather than letting it reach the terminal. A shim that printed
`open this on your laptop: <url>` would have that text swallowed, and the developer would
see nothing at all.

A non-zero exit is the only signal that crosses reliably: Claude Code maps it to
`{ ok: false }` and its caller surfaces the URL itself. For `gh`, whose stdout *is* the
terminal, a printed message would work — but one behaviour that works for both callers
beats two that each work for one. The shim writes its diagnosis to the channel log and to
stderr, and lets the exit code carry the outcome.

Verified against 2.1.224 rather than assumed: the sign-in flow renders the URL in its own
UI and captions it *"If your browser didn't open automatically, copy this URL manually"*.
The URL is on screen whether or not the browser command succeeds, so a shim that exits
non-zero degrades to exactly the experience a developer has on a headless box today —
minus the terminal browser. No `/dev/tty` write is needed.

## Why `BROWSER` is conditional

`BROWSER` is exported **only when the session already carries
`RIABUILD_CHANNEL_SOCKET`**, which is to say only in remote mode.

Setting it unconditionally would break local sessions: on a laptop, Claude Code opens a
browser perfectly well on its own, and pointing `BROWSER` at a shim that finds no socket
would turn a working local sign-in into an exit 1. The variable appears exactly where the
channel it depends on does.

---

# Components

| Unit | Responsibility |
|---|---|
| `channel/protocol.rs` | `OpenUrl`/`Opened` codec, `is_openable` — the scheme rule |
| `channel/opener.rs` | `Opener` trait, `SystemOpener` over `CommandRunner` |
| `channel/agent/mod.rs` | one dispatch arm; logs, then delegates |
| `shims/browser.rs` | server side: parse argv, check, send, degrade |
| `shims/mod.rs` | writes `bin/xdg-open` |
| `shell/mod.rs` | exports `BROWSER` when the socket is present |
| `cli.rs` | `riabuild channel open <args…>`, hidden |

`channel open` takes the caller's whole argv rather than a single `<url>` operand, because
`$BROWSER` and `xdg-open` are both invoked by programs riabuild does not control. The URL
is the first non-`-` argument; unknown flags are skipped rather than rejected, so a caller
that grows a new option still gets its link opened.

`Opener` exists so the platform decision is a value behind a trait rather than a `cfg!`,
the way `Clipboard` and `paths::default_project_dir_on` already are — `cfg!` compiles every
branch but one out of the test binary, so only the runner's own platform could be
asserted. macOS runs `open <url>`; Linux runs `xdg-open <url>`, resolved against a `PATH`
with `~/.riabuild/bin` removed so the laptop's own agent cannot find a riabuild shim and
recurse.

---

# Testing

| Property | Test |
|---|---|
| `browser.open` round-trips | encode → decode equality |
| A non-http scheme never becomes a `Request` | `file://`, `vscode://`, `javascript:`, `slack://`, bare `https://`, embedded space — all rejected at decode |
| A URL carrying a newline is refused | it would break the protocol's own line framing |
| `Opened` does not decode as `Pong` | explicit assertion — the discriminator bug this design avoids |
| The agent reaches the opener with the URL untouched | `FakeOpener` records it |
| A laptop that cannot open reports `Unavailable` | so the shim exits non-zero rather than claiming success |
| The platform opener runs the right command | `FakeRunner` asserts `open` on macOS, `xdg-open` on Linux |
| No socket exits non-zero | `browser::run` returns 1 |
| The generated shim has no `xdg-open` fallback | asserted on the script text |
| `BROWSER` is absent without a socket, present with one | `shell::environment` all three ways, empty value included |

There is no end-to-end test here. The `e2e/` suite on this branch covers setup, not the
channel, and the sshd container the clipboard design proposes has not landed — when it
does, a real shim invocation reaching a scripted opener on the far side belongs in it.
Calling that out rather than implying coverage that does not exist.

---

# Not in scope

- **Prompting or per-host policy.** Decided against above; revisit if the operation set
  grows past opening a URL.
- **Opening files or directories.** `xdg-open ./report.pdf` is a local operation on the
  server and stays one — it is not a URL, and forwarding it would mean shipping the file.
- **`BROWSER` for local sessions.** Nothing to fix there.

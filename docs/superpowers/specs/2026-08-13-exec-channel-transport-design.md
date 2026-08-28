# The channel over an exec session, not a forward — Design

**Date:** 2026-08-13
**Status:** Implemented
**Amends:** [`2026-08-07-clipboard-channel-design.md`](2026-08-07-clipboard-channel-design.md)

## Purpose

The clipboard channel is carried by `ssh -N -R <remote.sock>:<local.sock>`. That asks an
SSH server for **remote unix-domain socket forwarding**, which is a separate permission
from running a command, is refused outright by hardened servers and by non-OpenSSH
implementations, and — when it is allowed — leaves the socket's lifecycle owned by `sshd`
rather than by riabuild.

This replaces the transport with a plain command execution: `ssh -T <host> riabuild
channel pump`. The protocol, the operation allowlist, the agent and the shims are
unchanged. Only the pipe changes.

**riabuild should expect as few features as possible from an SSH server.** Command
execution is the floor — remote mode already depends on it for setup, for
`session::ensure`, for installing the server's binary and for the shell itself. A channel
that needs nothing beyond that floor needs nothing new at all.

## What this fixes, concretely

`AllowStreamLocalForwarding` defaults to `yes`, so the failure it was blamed for is rarely
the real one. Two failures that *are* real:

1. **A stale socket is fatal and unrecoverable.** For `-R`, **sshd** calls `bind()`, so
   `sshd_config`'s `StreamLocalBindUnlink` (default `no`) governs whether a leftover
   socket is removed. The `-o StreamLocalBindUnlink=yes` riabuild passes is a *client*
   option and applies only to sockets `ssh` itself creates, i.e. `-L`. A `channel.sock`
   left by a killed session therefore blocks every later session permanently, and no
   riabuild flag can clear it. Observed on `shared-cloudcli`, 2026-08-13.

2. **Servers that do not implement the extension at all.** `streamlocal-forward@openssh.com`
   is an OpenSSH extension. Brokered frontends and non-OpenSSH servers commonly implement
   `session`/`exec` and nothing else. There is no directive to set.

Under the exec transport, riabuild binds the socket itself, in a process it started, in
the developer's own namespace. Removing a stale socket becomes an ordinary `unlink` by the
owner instead of a server policy question.

## Architecture

```
LAPTOP                                        SERVER
──────                                        ──────
supervisor                                    riabuild channel pump
  spawns ssh -T <host> riabuild channel pump    binds <namespace>/channel.sock  ← it owns this
  ├── writes response frames → ssh stdin        accepts shim connections
  └── reads request frames  ← ssh stdout        multiplexes them onto stdout/stdin
  dispatches each frame to the in-process Agent          ▲
                                                         │
                                              ~/.riabuild/bin/xclip (the shim)
```

The laptop no longer binds a socket for remote sessions at all: the supervisor holds the
`Agent` in-process and speaks frames directly down the pipe. `riabuild channel agent`
keeps its unix socket, because a developer running it by hand has nothing else to connect
to.

### Framing

One request or response per frame. A frame is a JSON header line followed by exactly the
announced number of raw bytes:

```
{"id":7,"len":1234}\n<1234 bytes>
```

The payload is the **existing** wire form — a `protocol` request line plus its optional
body, or a response header line plus its optional body. Nothing about `protocol.rs`
changes, which is what keeps this a transport change rather than a protocol change.

`id` is assigned by the pump, monotonic per session, and scopes one shim connection. It
exists because `-R` gave one socket connection per request for free and a single pipe does
not: two shells pasting at once must not interleave their bytes.

`len` is bounded by `MAX_PAYLOAD` at decode time, as the protocol already bounds its own
bodies, so a broken peer cannot make either side reserve 4 GB.

### Health

The old design ran a **second** `ssh` on every ping interval to execute `riabuild channel
status` on the server, for as long as a developer's session lasted. That goes away
entirely, and no probe replaces it.

The probe existed because `ssh -N -R` had two channels: the ssh session, and the forwarded
socket riding on it. The forward could wedge while ssh believed itself perfectly
connected, and keepalives run *below* a forward and cannot see that. Detecting it needed a
request that actually crossed the forward, and it had to originate on the server.

Here there is only one channel. Requests travel over the ssh session's own stdio, so "is
the connection carrying traffic" and "is the connection alive" become the same question,
and `ServerAliveInterval`/`ServerAliveCountMax` already answer it on that exact
connection. A pump that dies ends the remote command, which ends `ssh`, which the
supervisor sees; a pipe that closes is reported by `serve_pipe` as an end.

So the probe is not an optimisation that was removed — it is a question that stopped being
askable. What it fed, the backoff reset, is now something `serve_pipe` already has: what
that connection carried.

#### The half this got wrong, and the keepalive that fixes it

Everything above is written from the laptop, and the laptop is only one end. `ssh` measures
the connection for the side that *started* it; the server measures nothing. `sshd` ships
with `ClientAliveInterval 0`, and a TCP connection whose peer has stopped acknowledging is
indistinguishable from an idle one for as long as the kernel goes on retransmitting into it
— a quarter of an hour, and unbounded if the peer is wedged rather than gone.

So a laptop on a flaky link produced this, every time: the laptop's `ssh` gave up after
45 s and reconnected, while on the server the pump from the *previous* connection stayed
alive, bound to `channel.sock`, relaying into a pipe nobody would ever read again. Three
symptoms, one cause, and no two of them looked related:

- every paste and every `riabuild channel status` reached that pump, waited out the full
  reply timeout and failed;
- every pump the reconnecting laptop started found the socket answering and exited with
  `another riabuild is already serving the clipboard channel`;
- the supervisor, whose `diagnose` has no pattern for that, retried it silently and then
  reported **"the clipboard channel cannot reach this server"** — about a server it had
  reached on every single attempt.

That last bullet was answered twice, and only the second answer was the right one. The
first gave the supervisor a *second sentence* for the same wall — "another session on this
server is still holding the channel" — which is a truer description of a stale pump and a
false alarm in the case that is far commoner: one developer's second terminal, where the
socket is answering because the channel is *working*. Recognising the refusal and treating
it as a wall at all was the mistake. `supervise` now answers `pump::ALREADY_SERVED` with
`Outcome::AlreadyServed` before `diagnose`, silently and without a retry, and the session
hands its clipboard lease back and stands by. See
`2026-08-28-many-windows-one-server-design.md`.

**The pump now measures the laptop, the way `ssh` measures the server.** Every
`KEEPALIVE_INTERVAL` (15 s, the laptop's `ServerAliveInterval`) it sends one frame up the
pipe; after `KEEPALIVE_DEADLINE` (45 s, three of them, the laptop's `ServerAliveCountMax`)
with nothing coming back, it returns — unbinding the socket on the way out. The corpse is
gone before the reconnecting supervisor's second attempt, paste fails immediately instead
of after twenty seconds, and the socket is free for the pump that should have it.

The frame is `id: 0` — reserved in `mux`, since connection ids start at one — and carries
**no payload**, which is what keeps `the pump is a relay and never a parser` true of it. It
names no operation and reads no answer; it needs a frame back and nothing else, and
`serve_pipe` already guarantees one for every frame including a frame it cannot parse. A
laptop too old to recognise the id answers with a parse error, which counts.

#### "It carried something" was the wrong bar for the *message*

The keepalive also settles what the supervisor had been getting wrong on its own side.
"Did this connection do any work" and "did this connection come up" had been one flag, and
the message about an unreachable server was gated on the first. On a link that drops and
rebuilds — the whole reason the developer is on mosh — a developer who simply was not
pasting carried nothing on any attempt, and the fourth rebuild of a perfectly healthy
channel told them it could not reach their server.

`serve_pipe` now returns `Served { requests, keepalives }`. `connected()` is either of
them, and it is what gates both the message and the backoff reset: a connection that
exchanged a keepalive demonstrably worked, and making paste wait out the thirty-second
ceiling on a link that is fine is the worse error.

### What is removed

`-R`, `ExitOnForwardFailure`, `StreamLocalBindUnlink`, `probe_args` and its second `ssh`,
`sockets::local_socket`, and both forwarding branches of `supervisor::diagnose`. The
`sun_path` limit still applies to the server's socket and `fits` still enforces it; it no
longer applies to a laptop socket that no longer exists.

## Changes by crate

| Crate | Change |
|---|---|
| `runner` | `CommandRunner::spawn_piped` and `PipedChild`. `RealChild::spawn` hardcodes `stdin(null)`/`stdout(null)`, correct for `ssh -N` and useless here. `FakeRunner` gains a scriptable counterpart so the supervisor stays unit-testable without a server. |
| `channel` | new `mux` (framing) and `pump` (the server side). `agent::pipe` serves an `Agent` over a frame pipe. `supervisor` spawns the exec session instead of the forward. |
| `remote` | `Plan` loses `probe`, gains the pump command. `sockets::local_socket` goes. |
| `cli` | `riabuild channel pump`, dispatched like `channel status`, and excluded from self-update along with the other `channel …` subcommands — its stdout is a payload. |
| `ui` | `StatusBar`: one line at a fixed row of a terminal something else is drawing on. See below. |

### The supervisor speaks on a status bar, not in the scrollback

The supervisor is the one thing riabuild runs *beside* the developer's shell rather than in
front of it, and it had been printing the same way every other part of riabuild does. Both
halves of that are wrong in a remote session, and the first is what gets reported:

- an interactive shell puts the terminal in **raw mode**, where `\n` drops a row and does
  not return to column one, so a folded warning arrives as a staircase down the screen —
  "ruined newlines", in the words of the person who hit it;
- it lands in the middle of a screen mosh and Claude Code are painting, and neither of them
  knows a line appeared.

`riabuild_ui::StatusBar` draws one line on **row two** — mosh's own bar owns row one, and
two programs writing the same cells is a race — with the cursor saved and put back, over
`/dev/tty` rather than a stdout that may be a pipe. `supervisor::StatusLine` redraws it
every two seconds, because the program underneath repaints and there is no notification
when it does; `remote::channel` owns it, since the session's end is when the line comes
off, and the supervisor clears it the moment a connection comes up. A bar holds one line
and truncates it, so the detail and the remedy stay where a developer can read them:
`riabuild channel status`.

Where there is no bar — every run that is not a remote session, `--quiet`, every test — the
supervisor prints exactly as it did.

## Security

Unchanged, and in two places improved.

**The server still only asks.** The operation allowlist is compiled into the laptop's
binary and `decode_request` still narrows every line to it. A pump is a relay: it never
constructs a `Request` variant, it copies bytes the shim wrote and the laptop parses. A
server cannot extend the operation set, and the direction of trust is exactly as before.

**The socket is still namespaced and still never stolen.** `<namespace>/channel.sock`,
parent created at 0700, a path that is a symlink or owned by another uid refused rather
than unlinked. The one change is that the *owner* may now clear its own stale socket,
which is the recovery `-R` made impossible.

**One fewer forwarding permission requested.** A server that never grants
`streamlocal-forward@openssh.com` to anyone is now a server riabuild works on.

## Testing

| Property | Test |
|---|---|
| A frame round-trips a body with newlines and non-UTF-8 bytes | `mux` unit test — the framing is the whole risk, exactly as it is for `protocol` |
| An oversized `len` is refused before allocation | `mux` unit test |
| Two concurrent shim connections do not interleave | `pump` test with two sockets in flight at once |
| A stale socket left by a killed pump is replaced | `pump` test — the failure this design exists to end |
| A request over the pipe reaches the agent and the answer comes back | end-to-end over an in-memory duplex, no ssh |
| A channel that cannot start still opens a shell | existing `remote::channel` test, unchanged and must stay green |
| The supervisor spawns an exec session and never `-R` | `supervisor` test asserting the argv |

The e2e suite (`e2e/`) exercises the real path against a real server and is the gate for
"a paste actually works".

## Migration

None. The channel is not a persisted format and both ends ship in the same binary —
`remote::install::version_for_server` already guarantees the server's riabuild is never
older than the laptop's. A laptop that somehow reached an older server gets a pump that
does not exist, `ssh` exits non-zero, and the supervisor reports a channel that would not
start: the documented degradation, and never a lost shell.

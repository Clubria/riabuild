# mosh over TCP

**Status:** implemented
**Date:** 2026-08-25
**Code:** `riabuild-cli/crates/remote/src/mosh.rs` and `mosh/`

## The problem

mosh is UDP, and a network that lets no UDP out is an ordinary thing for a Clubria
developer to be sitting on: a conference guest network, a corporate egress filter, a
hotel, a captive portal that opened 80 and 443 and nothing else. On one of those,
`riabuild remote` did the worst possible thing — it started a mosh session that could
never come up, the developer watched `mosh-client` say nothing for nineteen seconds, and
then a plain `ssh` appeared with no line anywhere explaining what had just happened or
why the session they were promised was not the session they got.

Two things were wrong with that. It cost nineteen seconds of every connection from such
a network, on every run. And it silently downgraded the developer to `ssh`, which is the
outcome mosh exists to avoid: a session that dies when the lid closes, with no local echo
on a link where the round trip is what makes typing unpleasant in the first place.

## The decision

Ask first, and when the answer is no, **keep mosh** by carrying its datagrams over the
TCP stream riabuild already has to the server, rather than giving mosh up.

The tunnel is Mullvad's [`udp-over-tcp`](https://github.com/mullvad/udp-over-tcp) — each
datagram framed with a 16-bit big-endian length, which is the whole of the protocol.
`Udp2Tcp` is the laptop's half, `tcp2udp` the server's.

## Why the crate is compiled in rather than installed

Every other tool riabuild depends on is downloaded, verified against a digest and kept
under `~/.riabuild/` — the rule in the root `CLAUDE.md`. `udp-over-tcp` cannot be that
tool, and the reason is upstream's rather than ours: the project publishes **no releases
and no binaries at all**, and the crate is `publish = false`, so there is no artifact for
a digest to describe. The two ways to obey the letter of the rule would both be worse
than the rule exists to prevent — mirroring bytes that were never published anywhere, or
downloading a floating build nobody verified.

So it is a rev-pinned git dependency in `riabuild-cli/Cargo.toml`, compiled into the
binary. That is the conservative reading of the same rule: the version is a constant in
this repository, it moves only in a release, and what runs on a laptop is what a
maintainer reviewed.

It also removes the second half of the problem for free. **Both ends of the tunnel are
riabuild itself.** `install::ensure_riabuild` has already put the server's copy there at
the same version — the "never older than the laptop provisioning it" invariant — so there
is no second tool to install on the far side, no package to ask a server's admin for, and
no protocol version to negotiate between the two ends. A server whose riabuild predates
these subcommands answers an unknown one on *stderr* and exits, which the laptop reads as
"could not tell" and treats exactly as it treated every server before this existed.

The honest cost, recorded beside the dependency: `udp-over-tcp` pulls tokio's
`rt-multi-thread` and `macros` into the workspace's unified feature set. riabuild still
runs on a current-thread runtime — nothing here builds a multi-threaded one — but the
feature is now compiled.

## The question riabuild asks

Not "can this laptop send UDP to the internet". That is a different question and it comes
apart from the real one in **both** directions:

- a network that passes DNS and QUIC still drops 60001;
- a cloud firewall that has never opened an inbound UDP port fails the session from a
  laptop whose own UDP is wide open.

So riabuild sends a datagram to **this server, on a port in mosh's own range** (60000–61000),
and asks whether it comes back. That answers the question riabuild is about to act on,
and — unlike a STUN request to a public server, which was the obvious alternative — it
tells no third party that this developer is connecting to anything.

The probe (`mosh/probe.rs`) is a 16-byte random nonce behind a `riabuild-udp-probe `
magic, sent up to four times at 500 ms, so the whole probe is bounded at two seconds and
finishes in one round trip on a working network. Four tries because UDP is allowed to
lose a packet: concluding "blocked" from one dropped datagram would tunnel a session that
had no need to be tunnelled, at the cost of mosh's roaming.

The server's half is `riabuild internal udp-echo` — bind a port in mosh's range, print
which, echo back whatever arrives. It echoes rather than merely receiving because a
one-way datagram proves nothing the laptop can see, and it echoes *every* datagram rather
than stopping after the first, because the laptop sends several and a responder that
answered only the first would turn its own success into a silence for every retry.

## One connection, not two

"Does this server have `mosh-server`?" and "does UDP reach it?" are one question about one
subsystem, and a `riabuild remote` already opens about ten SSH connections to a machine
that may be a continent away. So `mosh::ask` sends one shell script:

```sh
command -v mosh-server >/dev/null 2>&1 || exit 3; exec <riabuild> internal udp-echo
```

Exit 3 means "no `mosh-server` here" — chosen to be distinct from 255 (ssh's own), and
from 126/127 (a shell's answer for a command it could not run), so a server without mosh
is never confused with a connection that failed or a riabuild that is not where riabuild
thought it was. Otherwise the script `exec`s the echo responder, and the same stdio that
carried the script's output carries the answer.

`ask` **never fails.** Every way it can go wrong — a connection that drops, an old
riabuild on the far side, a malformed line, a server that answers nothing at all —
resolves to `Route::Direct`, which is precisely the behaviour riabuild had before any of
this existed. And the whole of it, the line *and* the probe *and* the exit status, is
under **one** 25-second `DECISION` timeout rather than one timeout each: bounding only the
read would move the hang one line down, onto the `wait`. What riabuild buys here is the
guarantee that asking about mosh can never be why a session did not open.

## The transport is stdio, never a port forward

The tunnel rides the ssh command's own stdio. This is the same decision the clipboard
channel made in `2026-08-13-exec-channel-transport-design.md`, and for the same reason: a
hardened server with `AllowTcpForwarding no` refuses `-L` outright, so a transport that
needs a forward works on exactly the servers that need it least. `ssh.rs`'s
`nothing_the_builder_composes_asks_for_a_forward` already forbids a forward reaching the
shared option list, and nothing here adds one.

Nothing in this feature needs anything from anyone's firewall, in fact, which is the point
of it. Every socket involved is on loopback:

```
laptop                                            server
  mosh-client ──UDP──► Udp2Tcp ═══ssh stdio═══► tcp2udp ──UDP──► mosh-server
             127.0.0.1          (the tunnel)              127.0.0.1  -i 127.0.0.1
```

`mosh-server` is started with `-i 127.0.0.1`, `tcp2udp` binds loopback (its default would
have been `0.0.0.0`), and the laptop's `Udp2Tcp` binds loopback too. The only thing that
crosses the network is the ssh connection riabuild was going to open anyway.

## Protocol lines

Each helper prints exactly one line to stdout before that stream becomes framed
datagrams: `RIABUILD-UDP-ECHO <port>` and `RIABUILD-TCP2UDP-READY`. Everything else either
half has to say goes to stderr, because **stdout is the transport** — a stray warning
wedged into that stream is a corrupted session rather than a message anybody reads. That
is why `main.rs` dispatches both subcommands before the banner, the config and the API
client exist, the same place and for the same reason as `internal askpass`.

`tcp2udp` prints its ready line only *after* it has connected to its own listener. A ready
line printed before that would tell the laptop to start sending into a listener that may
never have bound — and the first thing it would send is the session.

Two mechanical details that are load-bearing and easy to undo:

- the line is read a **byte at a time**, deliberately not through a `BufReader`. A reader
  that read ahead past the newline would swallow the first frames of the session into a
  buffer the pump never looks at.
- the pump flushes **every read** rather than using `tokio::io::copy`, which flushes only
  at the end. `std::io::Stdout` is a `LineWriter`, and framed datagrams contain no
  newline, so the unflushed spelling is a session that transfers nothing.

## What the developer is told

The warning is the third deliverable and not a footnote — the old behaviour's real cost
was that it explained nothing:

> ▲ This network blocks UDP, so mosh to `<server>` is being tunnelled over TCP.
>   Local echo and a dropped-packet-proof session still work. Roaming does not —
>   changing network or sleeping ends this one, where plain mosh would have survived it.

Both halves matter. The developer keeps the two things they notice most about mosh, and
they are told the one thing they lose, at the moment it becomes true, rather than
discovering it when they close the lid. mosh's roaming is a property of UDP — the session
survives a changed address because there is no connection to break — and a session carried
on a TCP stream inside an ssh connection ends when that connection does.

## Where it plugs in

`shell::open` is a three-way branch on `mosh::ask`:

| `Route` | what happens |
|---|---|
| `NoServer` | note, and `ssh` — unchanged |
| `Direct` | the mosh riabuild has always run — unchanged |
| `OverTcp` | `mosh::open_over_tcp`, and if *that* cannot be brought up, a warning and `ssh` |

`ask` is only reached when the laptop has a local `mosh` at all. The fallback out of
`OverTcp` is deliberate: a tunnel that cannot be established is not a reason to leave the
developer with nothing, and `ssh` is what they would have had.

`channel::Plan` gained a `binary` field to carry this, because the far end of the tunnel
is the server's own riabuild with its `RIABUILD_ROOT` prefix already on it — the same
string every other remote invocation uses.

## What this does not do

- **It does not replace mosh's UDP with TCP generally.** `Route::Direct` is the normal
  path and nothing about it changed.
- **It does not detect UDP loss mid-session.** The question is asked once, before the
  session opens. A network that starts blocking UDP halfway through is a session that dies
  the way it always did.
- **It is not a VPN or an obfuscation layer.** `udp-over-tcp` frames datagrams; it does
  not encrypt them. What is carrying them is ssh, which does.
- **It adds no requirement to any server.** No `sshd_config` line, no forward, no open
  port, no package. A server that cannot do it falls back to exactly what it did before.

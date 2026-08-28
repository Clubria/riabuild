# Many windows, one server

**Status:** implemented
**Supersedes nothing.** Amends the scope sentence in
`2026-08-12-concurrent-runs-design.md`, and the "second session" framing in
`2026-08-07-clipboard-channel-design.md` and
`2026-08-13-exec-channel-transport-design.md`.

## The sentence this design exists to change

> The codebase has thought carefully about concurrency before, but always framed
> as *two people sharing a server*, never as the same person's other window.

That is `2026-08-12-concurrent-runs-design.md` on the *local* tree, and it fixed
the local half: `~/.riabuild` is guarded by an `flock` per state file and one
provisioning lock, so two `riabuild` runs on one machine take turns instead of
racing.

Remote mode never got the same pass. Every place riabuild reasons about a second
connection to one server reasons about *a colleague*: a stranger under a shared
Unix account, whose socket must not be taken and whose session must not be cut.
That reasoning is not wrong — it is the reason the channel socket is refused
rather than stolen, and it stays — but it was the *only* reasoning, and it
produced advice and behaviour that make no sense for the case that actually
happens.

**The intended usage of `riabuild remote` is one person connecting to one server
from one or more windows.** A developer with a shell in one terminal, Claude
Code in another and a `tail -f` in a third is the ordinary shape of a working
day, not a corner. Everything below follows from writing that down.

## What it broke, in the order a developer met it

### 1. A banner that said paste was off, while paste worked

The reported symptom:

```
▲ Clipboard channel — another session on this server is still holding the channel · paste is off
```

Printed in a terminal in which Ctrl+V worked perfectly.

The chain: exactly one of a laptop's sessions to a server serves the channel and
the rest stand by (`channel::lease`). The lease is keyed by `Remote::hash` — the
login target *as typed* — while the socket it protects is keyed by the server's
`<home>/.riabuild-remote/<member-id>`. Those are not the same key, and they come
apart in two ordinary ways:

- **a handoff.** The serving window exits; a standing-by window takes the lease
  within five seconds; the server's pump has not finished dying yet.
- **two spellings of one machine.** `build-01.fly.dev` in one terminal and
  `10.0.0.5` in another are two hashes, two leases, and one socket. Both windows
  believe they are the one serving.

The second window's pump then meets `bind`'s refusal — correct, and the whole
reason `bind` probes the socket instead of trusting the file. But on the laptop
`supervise` had no vocabulary for that answer. It fell through `diagnose`, was
counted as an unrecognised failure, retried on the backoff schedule, and after
four attempts announced itself.

Three things were wrong with that and only one of them is the message:

- **it was a lie.** "Already serving" is the one answer that *proves* the
  channel is up. The shims in the reporting session's own shell were pasting
  through the very pump being complained about.
- **it retried.** An `ssh`, and an authentication against somebody's `sshd`,
  every few seconds for as long as two windows were open.
- **the remedy was destructive.** `bind`'s own message said *Close the other
  riabuild session on this server, or wait for it to finish* — advice to break a
  working session in order to fix one that was never broken.

### 2. Two windows sharing one `ssh-agent` directory

`Agent::start` put the issued-key agent's socket at
`~/.riabuild/agent/<server-hash>/sock` — one path per **server** — unlinked
whatever was already there, and on the way out `remove_dir_all`'d the directory.
Two windows to one server therefore:

- the second unlinked the first's live socket and bound its own at that name, so
  the first window's `-o IdentityAgent=…` addressed a process it never started;
- and whichever window finished first deleted the directory the other was still
  serving from.

The symptom is a session that works and then, minutes later, cannot reconnect
its clipboard channel or open a single further `ssh` — on precisely the servers
issued keys exist for, since those are the ones riabuild's own key cannot sign
in to.

### 3. Two windows minting two server sessions

`session::ensure` is a read (*is the saved token still good?*) and a write
(*mint one*) with a network round trip in between. Run concurrently against a
server whose token has expired, both windows mint, and the second's `session_id`
overwrites the first's on the record — leaving a live 90-day session on
riabuild-web that no `riabuild remote forget` can name. `session.rs` already
called that "the one state this function must never produce"; it just could not
see the other window.

### 4. `remote forget` with no idea anyone was there

`forget` revokes the server's session, takes riabuild's key back out of its
`authorized_keys` and clears the namespace. Each of those stops a shell somebody
is sitting in, and riabuild had no way to know somebody was.

## The design

### The socket is the lock; the lease is an optimisation

This is the load-bearing sentence and it is a change of *rank*, not of
mechanism. Both parts already existed:

| | asks | answers |
|---|---|---|
| `channel::lease` | an `flock` on this laptop | may I try to serve? |
| `pump::bind` | a connect to the server's socket | is anyone already serving? |

The lease was treated as authoritative and the socket's answer as an anomaly.
It is the other way round. The lease is keyed by something the developer typed
and the socket by something the server is; only the socket can be wrong about
nothing. So:

- a session that takes the lease and then finds the socket served **is not in an
  error state and never was**. It hands the lease back and stands by, exactly as
  though it had never won it;
- the lease keeps doing what it is good at, which is saving a needless `ssh` in
  the common case. It is not required to be correct for the channel to be
  correct.

This is also why the lease is *not* re-keyed onto the socket path. Two different
machines can have identical socket paths, and a lease keyed on the path alone
would make one of two unrelated servers stand by for ever. Making the loser
self-correct is both cheaper and more robust than trying to predict the
collision.

### `ALREADY_SERVED` is a wire format

`pump::ALREADY_SERVED` is one constant, used by the pump to compose the refusal
and by the supervisor to recognise it. It is matched as a substring, so the path
and the prose around it stay free — but the two ends of a channel can be a
release apart (the server's copy is upgraded by a `riabuild remote` run), so the
phrase itself is a compatibility surface. It is deliberately the substring the
*old* wording shared, so a laptop that upgraded first still understands a pump
that has not.

`supervise` answers it with `Outcome::AlreadyServed`, before `diagnose`, without
a word to anybody, and without a retry. `Option<Failure>` could say "told to
stop" and "hit a wall" and nothing else, which is why the good news had nowhere
to go.

### Standby bounces back off on `backoff`, not `STANDBY_POLL`

The two waits in `hold` cost different things. Asking whether the lease is free
is one `flock` on a local file; asking whether the *socket* is free is an `ssh`
and an authentication. So the free-lease poll stays at five seconds and the
already-served bounce uses the supervisor's own jittered `backoff`.

After `QUIET_BOUNCES` (seven, about ninety seconds — comfortably past the pump's
45-second `KEEPALIVE_DEADLINE`) it says one thing, once. Not "paste is off",
which riabuild cannot know from the laptop: only `riabuild channel status`, which
asks the socket itself, can tell a working sibling from a pump that outlived its
laptop.

### One directory per run, not per server

`~/.riabuild/agent/<server-hash>/<pid>/`, holding a `sock`, the public halves,
and a `run.lock` the `Agent` holds for its life. Liveness is asked of the kernel,
never of a pid: a `run.lock` nothing holds is a run nothing is running.
`Agent::start` sweeps the dead ones before creating its own; teardown removes
this run's directory and the server's only if it is now empty.

The failure direction is chosen deliberately. A lock riabuild cannot take leaves
a directory behind — a dead socket and a public key, both inert, both swept by
the next run that can. Deleting one it should have kept is the bug being fixed,
and no path can now do it.

### Minting a session is serialised per server

`session::ensure` takes an `flock` on
`~/.riabuild/remote-sessions/<hash>.lock` before it reads the keychain, and
re-reads the record from disk once it has it. The window that waits finds the
other's token already recorded and mints nothing. A lock of its own, not
`state_lock_file`, which the store's `persist_one` takes from inside this one.

### `windows` — this laptop's open terminals, counted

A new module. Each `riabuild remote` run holds an `flock` on
`~/.riabuild/remote-windows/<server-hash>/<pid>.lock` for the length of its
session; counting is trying to take each one, and the ones that succeed are
windows that have ended, swept as they are counted.

Only one thing reads it today: `forget`, which now says what will break before
it breaks it. It **warns and does not refuse** — `forget` is a destructive
command typed by name, usually because something about that server has gone
wrong, and one that downed tools while a window was open could not clean up the
case it is most needed for. It also runs unattended from `shared::reconcile`,
where there is no prompt to fall back on. What was wrong was not that it went
ahead; it is that it went ahead in silence.

And it claims only what riabuild can honestly know: *this laptop's* windows. A
colleague on a second laptop leaves nothing here, which is why the sentence says
"your" and why the dashboard's session list remains where the rest is visible.

## What does not change

**A colleague is still a stranger.** Every refusal written for two developers
under one Unix account stays exactly as it was: `socket::ensure_ours` still
refuses a socket owned by another uid or standing behind a symlink, `bind` still
refuses a live socket rather than taking it, and the provisioning lock is still
per developer rather than per box — a machine-wide one would let one developer
block another under the account they share. Nothing here relaxes a boundary. It
adds the case that was missing on this side of it.

**The last window out still ends the channel.** The laptop is the side that
connects, so when every session to a server has ended, a new `riabuild remote`
is what brings the channel back. That was true before and is unchanged.

**The pump still refuses.** Making the second pump *bind* — by unlinking, or by
`StreamLocalBindUnlink` — would be the one change that looks like it solves this
and does not: it takes a working session's clipboard traffic and gives it to
another process, which is the same act whether the two windows belong to one
person or two people. What changed is what the laptop does with the refusal, not
whether it is refused.

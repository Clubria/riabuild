# Picking a server, and being told how to forget one

**Date:** 2026-08-12
**Status:** Implemented

## Why

`riabuild remote` with no target has three behaviours today, chosen by how many servers
`remotes.json` holds:

| Saved servers | What happens |
|---|---|
| none | the three questions — hostname, port, username — then a name |
| one | reconnects to it, without asking |
| several | a numbered list, read with `ask_required` |

Two problems follow from the middle row. A developer with one saved server has no way to
add a second without spelling it out on the command line
(`riabuild remote ada@gpu.internal:2222`), which is precisely the form remote mode exists
to spare them. And `riabuild remote` means something different on every machine, so
nobody can be told what it does.

Nothing tells a developer that `riabuild remote forget` exists, either. `riabuild claude`
prints the commands that act on the list it just showed; the server list prints none.

## What changes

`riabuild remote` with no target becomes one prompt:

- **No saved servers** — straight into the add questions, unchanged. There is nothing to
  pick from, and a picker with one option is a worse way to ask "what is the hostname".
- **Any saved servers** — the servers box, then a question. A number connects to that
  server; the number after the last one adds a new server, running the same three
  questions the empty store asks.

A saved server the developer has connected to before is still one keystroke away: the
default is the most recently used server, so Enter reconnects exactly as today's
one-server path did.

### The unattended path is unchanged

`Ui::ask` answers `None` when there is no terminal, and taking a default there is the
crate rule. It is the wrong rule for this question: connecting is not a read — it
provisions a server, mints it a session, and lends it a GitHub sign-in — so an
unattended run must not pick a server nobody named.

So the terminal is checked before the question is put, the way `accounts::delete` checks
it before its confirmation, and both of today's non-interactive behaviours are kept
exactly:

| Saved servers | No terminal |
|---|---|
| one | reconnects to it, as today |
| several | refuses, naming the servers it could have picked from |

That keeps `riabuild remote` working in the container e2e and in any script that has one
server saved, and keeps the refusal for the case where the answer would be a guess.

### Bad input is re-asked, not fatal

Three attempts, then the default — the bound `store::ask_name` already uses, for the same
reason: a developer who cannot give a usable answer is better served by riabuild picking
one than by being asked forever. Anything that is not a number in range, and not the add
option, warns and asks again.

The answer's meaning is a pure function, so the rules are testable without a terminal:

```rust
pub enum Pick {
    /// Zero-based index into the records shown.
    Server(usize),
    Add,
}

pub fn parse_pick(answer: &str, count: usize) -> Option<Pick>
```

`n` and `new` are accepted alongside `count + 1`, because that is what a developer types
at a prompt whose last line reads "Add a server".

## The servers box

One renderer, `remote/render.rs`, used by both the picker and `riabuild remote list` —
so a server reads the same way wherever it is shown, and there is one place to change
that.

Choosing:

```
Your servers:

  1  build-01   ada@build-01.fly.dev    used 3 hours ago
  2  gpu        ada@gpu.internal:2222   never connected
  3  Add a server

  Connect without asking:  riabuild remote build-01
  Forget a server:         riabuild remote forget gpu
```

Listing:

```
Your servers:

  build-01   ada@build-01.fly.dev    used 3 hours ago
  gpu        ada@gpu.internal:2222   never connected

  Connect to one:   riabuild remote build-01
  Add a server:     riabuild remote
  Forget a server:  riabuild remote forget gpu
```

The differences are only the two things the context already answers: numbers, because
they are what the question below is read against, and `Add a server`, which is an option
in the picker and a hint in the list.

`riabuild remote list` with an empty store keeps today's single line — "No servers yet.
Run `riabuild remote` to add one." A box with no rows and hints naming no server is the
hint-that-refuses failure the next section exists to avoid.

### The hints

Modelled on `accounts/render.rs`, including the rule that matters there: **only commands
that would succeed right now**. A hint that refuses when typed reads as riabuild being
broken rather than as the developer having asked for something impossible.

Every hint names a server taken from the records being shown, never a placeholder and
never a count — the same reason `accounts::render::hints` reads its numbers off the
accounts rather than off `accounts.len()`.

Which server each hint names is deliberate:

- **Connect** names the most recently used server, which is the one the default already
  points at. Typed, it does what Enter would have done, which is what makes it a
  demonstration rather than a syntax note.
- **Forget** names the *least* recently used server. It is the likeliest one to be stale,
  and with two or more servers saved it is a different server from the connect hint — so
  the two lines together say "the name goes here" without either of them having to.

With one saved server both name it, which is honest: it is the only server there is.

### `never connected`

A record whose `last_used_at` is `0` — added, and never successfully connected to, which
is what a failed first run leaves behind — currently renders through
`duration_words(now_secs() / 60)` as something like `used 29873 days`. The shared
renderer says `never connected` instead. It is a bug in the column being rewritten, not
new behaviour.

## Layout

Two new files under `remote/`, and `store.rs` gets smaller rather than larger:

| File | What |
|---|---|
| `remote/render.rs` | the servers box and its hints — no IO, a `Theme` parameter like `accounts/render.rs` |
| `remote/pick.rs` | the no-target path: the picker, `parse_pick`, and the add questions moved out of `store.rs` |

`store::choose` keeps the `target` half — a saved name, a spelled-out spec, the
identity-match that reunites a respelt host — and delegates the rest to `pick::pick`.
`store::list` renders through `render`. `store.rs` is already at the crate's ~300-line
production budget, and the picker is the third thing that would have gone in it.

## Testing

Unit, in-crate:

- `parse_pick` — every number in range, the add number, `n`, `new`, out of range, blank,
  nonsense.
- The box — numbers only when choosing; `Add a server` numbered `count + 1`; both hints
  present, naming servers that are in the box; `never connected` for an unused record;
  no escapes under `Theme::plain()`.
- The picker, driven by `Ui::scripted`:
  - one saved server, a developer present, answering `1` — connects, and *asks*, which
    is the behaviour change; the old path asserted the opposite.
  - one saved server, no terminal — reconnects, asking nothing.
  - several saved servers, no terminal — refuses, and the failure names the servers.
  - answering the add number reaches the hostname question rather than connecting.
  - an unusable answer is re-asked and then falls back to the default.
  - Enter takes the most recently used server, not the first one saved.

`e2e/remote/run.sh` passes an explicit target and so never reaches this path; it needs no
change. It is the regression that matters most, though, which is why the non-interactive
behaviour is preserved rather than folded into the prompt.

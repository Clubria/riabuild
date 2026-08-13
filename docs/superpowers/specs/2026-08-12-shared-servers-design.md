# Servers the team shares

Date: 2026-08-12
Status: approved

## Why

Every server riabuild knows about today was typed in by the developer sitting in front of
it. `remotes.json` is the whole world: a name, an address, and what this laptop has done
with it. That is right for a box someone rented for an afternoon and wrong for the three
machines the team actually works on — every new developer is told a hostname over Slack,
types it slightly differently from the last person, and names it whatever occurs to them.

So the addresses of the team's servers move to riabuild-web, where a lead types each one
once. Nothing else moves. A shared server's SSH key pair, its saved password, and the
riabuild session minted for it stay exactly where they are — on the one laptop that made
them.

## What is shared, and what is not

| | Where it lives | Why |
|---|---|---|
| hostname, port, username | riabuild-web | one lead types it, every developer gets it |
| the local label | riabuild-web | so a server is called the same thing in every terminal |
| SSH key pair | the laptop, keyed by `Remote::hash()` | proves *this developer* to the server |
| SSH password | the laptop's keychain | it is that developer's account password |
| riabuild session | the laptop's `remotes.json`, token on the server | minted for one laptop, revocable by it |
| `home`, `lastUsedAt` | the laptop's `remotes.json` | facts about this laptop's relationship with the box |

The CLI can add and remove **local** servers, and neither add nor remove shared ones. What
`riabuild remote forget shared-gpu` removes is this laptop's own traces — see §7.

Who sees them: **developers and leads**. A candidate's list is empty.

## 1. The table and the endpoint

```ts
sharedServers: defineTable({
  name: v.string(),
  host: v.string(),
  port: v.number(),
  user: v.string(),
  createdBy: v.id("members"),
  createdAt: v.number(),
  updatedAt: v.number(),
}).index("by_name", ["name"]),
```

```
GET /api/v1/remotes/shared
  → authenticate the session, confirm the member is active, re-verify org membership
  → candidate                → { servers: [] }
  → developer | lead         → { servers: [ { id, name, host, port, user } ] }
```

A candidate gets an empty list with **200, not 403**. `riabuild remote` is how they reach
the server they set up themselves, and a 403 here would take that away to enforce a rule
about servers they were never going to see.

`id` is the Convex row id. It is what the laptop keys its own state by, so it must stay
stable across a rename *and* across an address edit — which is precisely what a row id is
and what a name or a hash is not.

The reply is an object rather than a bare array so a later field has somewhere to go, the
same shape rule the rest of `/api/v1` follows.

## 2. Both ends validate the address

A lead types this into a browser and it ends up in an `ssh` argv on someone else's laptop.

| Field | Accepted |
|---|---|
| `name` | `[A-Za-z0-9._-]{1,32}`, not beginning `shared-` (reserved, case-insensitively), unique |
| `host` | `[A-Za-z0-9.-]{1,253}`, and **never beginning with `-`** |
| `port` | an integer, 1–65535 |
| `user` | `[A-Za-z0-9._-]{1,32}` |

The leading-dash rule on `host` is the one that is not cosmetic. riabuild runs `ssh`
through `CommandRunner` with an argv, so there is no shell to inject into — but `ssh`
itself reads a leading-dash argument as an option, and `-oProxyCommand=…` in the hostname
position runs a command of the lead's choosing on the developer's laptop. The server
ships data, never logic; a hostname that is really an option is that boundary being
crossed by accident.

The CLI applies the same rules to what it receives. That is `api/org.rs`'s `version_only`
precedent: the client-side check exists so the CLI survives a server that forgets its own.
A server that fails validation is dropped from the list with a warning naming it, rather
than failing the whole fetch — one bad row must not take the other servers with it.

## 3. One list, with provenance

`store::Record` gains one persisted field:

```rust
/// Empty for a server this laptop added. Otherwise the riabuild-web row id of
/// the shared server this record holds *local state* for.
#[serde(default)]
pub shared_id: String,
```

and one in-memory field, `#[serde(skip)]`:

```rust
pub enum Origin { Local, Shared, Stale }
```

Shared records are persisted, because their `session_id`, `home` and last-known address
are the only evidence anywhere on this laptop that a live session exists on that box.
They are never *trusted*:

- `Store::load` marks every record with a non-empty `shared_id` as `Stale`.
- The fetch reconciles by id: a match is overwritten with Convex's name, host, port and
  user and becomes `Shared`; an unmatched shared server gets a fresh record with empty
  state; a `Stale` record left over is one the leads removed, and it stays on disk and
  out of sight.
- Every path that can lead to a connection — the picker, and a target being resolved —
  sees only `Local` and `Shared`.

A `Stale` record is not quite out of sight, though, or a server the leads removed would
leave a live session with no command that could reach it. `riabuild remote list` shows
those rows last, marked `no longer shared`, and `riabuild remote forget` accepts their
names. Both are reading and forgetting, never connecting, so neither is a persisted
address being trusted — one is a line on a screen, and the other is the command for
letting go of it.

"Pull from Convex every time" is therefore a property of the code rather than a promise:
there is no path from a persisted address to an `ssh` command, because a persisted
address is `Stale` until the fetch overwrites it.

`Store::save` writes every record, `Stale` ones included. Dropping them would lose the
`session_id` of a live session — the one state `forget.rs` already says this laptop must
never produce.

## 4. `shared-` is a display name, applied once

`record.name` holds Convex's bare name. `Record::display_name()` prefixes it for shared
records, and that is what:

- the servers box prints,
- `Remote.name` carries, and so what `RIABUILD_REMOTE` carries and the shell banner reads
  (`Clubria environment active on shared-gpu`),
- `riabuild remote forget` is typed with.

Nothing writes the prefix down. `remotes.json` holds `"gpu"` with a `sharedId` beside it,
and riabuild-web holds `"gpu"`; the prefix exists between them, where the collision it
prevents actually happens.

**Lookup is two passes.** A target is matched against every record's display name first,
then against every record's bare name. So a local `gpu` always wins over a shared `gpu`,
`shared-gpu` is unambiguous, and `riabuild remote gpu` still reaches the shared one when
nothing local claims that name. Two passes rather than one rule with an exception in it,
because the ordering is the whole behaviour and a `find` with a disjunction in its
predicate would resolve by whichever record happened to be saved first.

`store::ask_name` refuses a local name beginning with `shared-`, for the same reason it
refuses a name already taken: two servers a developer cannot tell apart at the prompt.

```
Your servers:

  1  build-01     ada@build-01.fly.dev    used 3 hours ago
  2  shared-gpu   ada@gpu.internal:2222   never connected
  3  Add a server

  Connect without asking:  riabuild remote build-01
  Forget a server:         riabuild remote forget build-01
```

The forget hint names a **local** server whenever there is one, and a shared server only
when there is nothing else to name. `render::hints` already only prints commands that
would succeed; this adds the rule that a hint must not *read* as something it is not, and
`forget shared-gpu` reads like deleting the team's server.

## 5. A failed fetch is a warning

```rust
/// Never fails. A server list this laptop could not load is a smaller thing
/// than a developer who cannot reach the server they set up themselves.
pub async fn fetch_or_warn(ctx: &Ctx) -> Vec<SharedServer>
```

An unreachable riabuild-web, a 500, or a body that does not parse all produce the same
note — *could not load the team's servers; showing this laptop's own* — and an empty list.
The picker and `remote list` both go through it.

`riabuild remote list` today returns before `crate::connect`, so it works with no network
at all. It keeps that: the connect it now needs is attempted inside the same soft failure,
so an offline `list` prints the local servers and the note.

A target that names a shared server after a failed fetch **fails**, naming the fetch as
the reason. The alternative is connecting to a remembered address, which is the one thing
§3 exists to prevent.

## 6. An edited address retires the old identity

Leads can edit the address of a shared server, and an address is an identity:
`Remote::hash()` is taken over `user@host:port`, so an edit orphans a key pair, a saved
password, and a **live session** on a box riabuild will no longer be pointed at.

On connect, when a shared record's stored hash no longer matches the address just fetched,
riabuild retires the old identity before setting up the new one:

1. revoke the session through the API, if one was ever minted,
2. best-effort SSH cleanup at the **old** address — which is why the last-known address is
   persisted rather than only the hash,
3. delete the local key pair and the saved password for the old hash,
4. note what happened, clear the state fields, and continue as a first connection.

This is `forget_with`'s existing steps minus the record removal, so it is extracted rather
than written twice — `forget::retire_identity(remote, record, …)`, called by both.

It runs on connect rather than during the fetch. The fetch happens on every
`riabuild remote` and every `remote list`, and an SSH round trip to a machine the
developer did not ask about is not something a listing should do. A developer who never
connects again leaves a session that expires on its own.

## 7. Forgetting a shared server

`riabuild remote forget shared-gpu` runs the same three steps it always has — revoke,
clean the server, delete what is local — and then removes the **record**, not the shared
server. The row in riabuild-web is untouched, so the server is back in the picker on the
next run, with no key, no password, and no session.

That is the honest reading of "the CLI cannot remove shared servers": it cannot take the
server away from the team, and it can always let go of its own credentials for it. The
alternative — refusing outright — leaves a live session and a saved password with no
command that clears them.

The same command is what clears a server the leads have since removed — the `Stale` rows
of §3, which `remote list` marks `no longer shared` precisely so that a developer can see
there is something left to forget.

## 8. The dashboard

A lead-only **Shared servers** section, in a new `src/components/SharedServers.tsx`
rendered by `LeadPanel`, which is already at 357 lines and is the file that would
otherwise absorb it.

It reads `useData()` like everything else in `src/` — the `Data` contract gains
`sharedServers: Loadable<SharedServer[]>` and `addSharedServer`, `updateSharedServer`,
`removeSharedServer`, with fixtures behind `?scenario=` so the empty state, a populated
list, and a rejected address are all reachable without a database in that state.

Only leads see the section. Developers get the list where they need it, which is the
picker in their terminal.

Every mutation writes an `auditLog` entry (`shared_server.add`, `.update`, `.remove`,
with the name and address in `meta`). Handing every developer a new machine to run
`claude` on is an access change, which is the rule that table exists for.

## Layout

Paths are as of the cargo workspace split (#55): `riabuild-cli/crates/<crate>/src/`.

| File | What |
|---|---|
| `crates/api/src/remotes.rs` | the fetch, the `SharedServer` type, and its validation |
| `crates/remote/src/shared.rs` | `fetch_or_warn`, and reconciling a fetch into the store |
| `crates/remote/src/store.rs` | `shared_id`, `Origin`, `display_name`, the two-pass lookup |
| `crates/remote/src/render.rs` | shared rows, and the forget hint's preference |
| `crates/remote/src/forget.rs` | `retire_identity`, extracted; forgetting a shared record |
| `riabuild-web/convex/schema.ts` | `sharedServers` |
| `riabuild-web/convex/sharedServers.ts` | list, add, update, remove, and the internal query for the endpoint |
| `riabuild-web/convex/http.ts` | `GET /api/v1/remotes/shared` |
| `riabuild-web/src/components/SharedServers.tsx` | the lead-only section |

## Testing

**Rust, in-crate.** The reconcile: a match refreshes name and address, an unmatched server
arrives with empty state, a removed one goes `Stale` and disappears from the box while
staying in the file. The two-pass lookup: local beats shared on a shared name, `shared-x`
resolves, a bare shared name resolves when nothing local claims it. Validation: a host
beginning `-`, a name beginning `shared-`, a port of 0 and of 70000 are each dropped with
the rest of the list surviving. `retire_identity` on a changed address: the session is
revoked, the old address is what the cleanup was aimed at, and the run continues. The
picker through `Ui::scripted` with a mixed box. `forget shared-x` leaves nothing local and
asks riabuild-web to remove nothing, and it reaches a `Stale` record by name — the case
that keeps a removed server's session revocable.

**Convex, vitest.** The endpoint: a candidate gets `{ servers: [] }`, a departed member
gets 403, a valid session gets the list. The mutations: lead-only, each validation rule,
the uniqueness of a name, and an `auditLog` row per change.

**Playwright.** A scenario per state of the new section, screenshots looked at, per
`.claude/skills/visual-testing/SKILL.md`.

## Out of scope

- A per-server minimum role. Visibility is one rule for the whole list.
- A description or note field on a shared server.
- Any shared *secret*. A shared server's password is still each developer's own, and the
  Infisical credential is still brokered per use and never written down.

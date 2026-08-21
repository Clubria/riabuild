# SSH keys the org issues

**Date:** 2026-08-13
**Status:** Implemented

## Why

There are servers a Clubria developer is supposed to reach that riabuild cannot get onto
at all. A managed bastion, a customer's jump host, a GPU box built from an image someone
else maintains — the account exists, the developer is entitled to it, and the machine
authenticates with a key that was handed out rather than one generated on a laptop.

Today `riabuild remote` has exactly one answer for a server its own key cannot sign in to:
ask for the account's password. On a machine with `PasswordAuthentication no` there is no
password to ask for, so the run stops with

> that server accepts keys only, so there is no password to ask you for

and the developer is told to paste a public key into an `authorized_keys` file they may
not be able to edit. That sentence is the shape of the problem. It is not a rare edge —
it is the default posture of every hardened box the team does not build itself.

So a lead can now put a private key into riabuild-web and say who it is issued to. A
developer's CLI pulls the keys issued to them, holds them in memory, and uses one to get
in — after which everything riabuild already does happens exactly as before.

## What this costs, said plainly

`../../CLAUDE.md` says secrets are **brokered, never stored**, and names two exceptions,
both local to one machine. This is a third, and it is neither brokered nor local:

**riabuild-web now stores a class of long-lived secret, in plaintext, readable by any
lead. A dump of the Convex database hands out working SSH access to whatever those keys
open.**

That is written here, and in `CLAUDE.md`, so that nobody has to reverse-engineer whether
it was deliberate. Three things make it the right trade rather than a slip:

- **There is no brokered alternative that is not a fiction.** Infisical brokering keeps
  a payload out of Convex, but a lead must still paste the key into a browser to derive
  its fingerprint, and the CLI must still receive the whole private key to use it. The
  bytes are equally recoverable at both ends; encryption at rest with a key held in the
  same deployment moves the problem rather than solving it, at the price of a second
  system that can be down.
- **The blast radius is bounded by what the key opens, not by riabuild.** These keys are
  issued by whoever administers those machines. Revoking one is something they already
  know how to do, and — see §7 — it takes effect without any laptop doing anything.
- **The alternative is worse and already happening.** A key a developer cannot get from
  riabuild arrives over Slack, and lives in their `~/.ssh` forever.

What this does **not** loosen: the Infisical credential is still minted per use and still
never written down, and a developer's own account password is still their own.

## What is issued, and what is not

| | Where it lives | Why |
|---|---|---|
| the private key | riabuild-web, in Convex | one lead pastes it, the people named get it |
| who it is issued to | riabuild-web | entitlement is an org decision |
| the public key, fingerprint, type | riabuild-web, derived | so a lead can read the row without reading the secret |
| the key while in use | the laptop's memory, and an `ssh-agent` riabuild owns | never a filesystem |
| riabuild's own key pair | the laptop, keyed by `Remote::hash()` | still proves *this developer* to the server |
| the account password | the laptop's keychain | still that developer's own |

**Naming.** The CLI already calls `~/.riabuild/ssh-identities` its own per-server keys, so
"identity" keeps meaning *this laptop's own* throughout. The org's are **issued keys**:
`issuedKeys` in Convex, `issued.rs` in Rust, "Issued keys" in the dashboard.

## 1. The table

```ts
issuedKeys: defineTable({
  label: v.string(),
  privateKey: v.string(),
  publicKey: v.string(),
  fingerprint: v.string(),
  keyType: v.string(),
  issuedTo: v.array(v.id("members")),
  createdBy: v.id("members"),
  createdAt: v.number(),
  updatedAt: v.number(),
}).index("by_label", ["label"]),
```

`publicKey`, `fingerprint` and `keyType` are **derived, never accepted**. The mutation
parses `privateKey` and writes what it found, discarding whatever the client sent for
those three. The browser derives the same values only so the fields fill in as the lead
pastes; a client that lied would produce a row the CLI then refuses (§6), which is a
broken key rather than a compromised one — but a server that trusts the client for a
displayed fingerprint is a server whose audit log is decorative.

`issuedTo` is an array on the row rather than a join table. Convex cannot index
array-contains, so "keys issued to me" is a `.take(200)` and a filter — the same bound
`sharedServers` uses, for the same reason: this list is tens of rows, typed by hand.

## 2. Deriving a public key without any crypto

An OpenSSH private key file contains its own public key **verbatim**. The container is
base64 between `-----BEGIN OPENSSH PRIVATE KEY-----` markers, and decodes to:

```
"openssh-key-v1\0"
string  ciphername        "none" when the key has no passphrase
string  kdfname
string  kdfoptions
uint32  number of keys    always 1 in practice
string  publickey         <-- the whole public blob, in the clear
string  encrypted section
```

So `convex/lib/opensshKey.ts` decodes, walks length-prefixed fields, and returns

- `keyType` — the first string *inside* the public blob (`ssh-ed25519`, `ssh-rsa`, …)
- `publicKey` — `<keyType> <base64(publickey blob)>`, an ordinary `authorized_keys` line
- `fingerprint` — `SHA256:` + unpadded base64 of the SHA-256 of the public blob, which
  is what `ssh-keygen -lf` prints and therefore what a lead can compare against

No key mathematics, one digest. The same module runs in Convex's V8 runtime and in the
browser, and is ported to Rust in §6.

**Two rejections happen at the parse, and both matter.**

A container whose `ciphername` is not `"none"` is passphrase-protected and is refused at
the paste box. `ssh-add` would prompt for that passphrase on a developer's laptop, in a
run riabuild is driving, with nobody who knows the answer — a hang with no output, which
is the failure mode `CLAUDE.md`'s "every prompt has a default" rule exists to prevent.

Anything that does not parse is refused too, rather than stored and discovered later. A
key that reaches a laptop and turns out to be a PEM certificate has already cost a
developer a failed run.

## 3. The mutations

Lead-only, through `requireLead`, in `convex/issuedKeys.ts`:

| | |
|---|---|
| `create` | label + pasted key; parses, derives, stores |
| `replaceKey` | a new private key under the same label and grants — how rotation is done |
| `setIssuedTo` | the member list |
| `remove` | deletes the row |

`list` is the dashboard's, and it returns a **projection that has no `privateKey` field
at all** rather than a document with the field stripped at the call site. The distinction
is the point: a projection cannot be forgotten by the next caller, and there is exactly
one place to read to be sure.

Every mutation writes an `auditLog` row — `issued_key.created`, `.replaced`, `.issued`,
`.removed` — carrying the label and, for `.issued`, the logins added and removed.
Deciding who can reach a machine is an access change, which is what that table is for.

## 4. The endpoint

```
GET /api/v1/issued-keys
  → authenticate the session, confirm the member is active, re-verify org membership
  → candidate                → { keys: [] }, 200
  → developer | lead         → { keys: [ { id, label, keyType, publicKey,
                                           fingerprint, privateKey } ] }
                               — only rows whose issuedTo names this member
```

A candidate gets an empty list with **200, not 403**, for the reason
`GET /api/v1/remotes/shared` does: the same command is how they reach the server they set
up themselves, and refusing it would take that away to enforce a rule about keys they were
never going to receive.

The private key travels in the same response rather than behind a second, separately
authorised fetch. A second round trip would be theatre — the same session, the same
bearer token, the same TLS connection, and the CLI needs every key it is entitled to
anyway in order to probe them.

**Every fetch is audited**, as `issued_key.served`, with the labels served in `meta`. This
is the only endpoint in riabuild that hands out a durable credential. "Who holds the
`prod-bastion` key" is the first question asked after somebody leaves, and a log of grants
answers only who was *entitled* to it. Volume is one row per `riabuild remote` against a
server riabuild's own key cannot yet sign in to, which is rare by construction (§8).

## 5. The dashboard

A lead-only **Issued keys** section in `src/components/IssuedKeys.tsx`, rendered by
`LeadPanel` beside `SharedServers` and built the same way — `useData()`, never
`useQuery`, with fixtures behind `?scenario=`.

Adding a key is a paste box. Beneath it, **type, public key and fingerprint fill in as
read-only fields while the lead pastes**, parsed in the browser by the module from §2, so
the value being stored is legible before it is stored. A key that will not parse says so
there, not after saving.

The list shows label, type, fingerprint, and the members it is issued to.

**No route returns a stored private key to a browser.** Not on edit, not behind a reveal
control, not for the lead who pasted it. Changing a key means pasting a new one
(`replaceKey`). This is why the fingerprint is stored and displayed: it is how a lead
confirms which key a row holds without the row ever handing it back.

## 6. The CLI: fetching, and checking what it fetched

`crates/api/src/issued.rs`, following `crates/api/src/remotes.rs` exactly — including its
reason for existing. Both ends validate, so the client survives a server that forgets its
own check, and one unusable row costs a developer that row rather than the whole list.

Per key: the label charset, that `publicKey` is a plausible public key line, and one check
`remotes.rs` has no analogue for —

**the private key's own embedded public half must equal the `publicKey` the server sent.**

The Rust port of §2's parser is what makes this possible, and it closes the gap left by
deriving in a mutation: if the two disagree, something has edited the row's fields apart
from each other, and riabuild refuses that key rather than probing a server with a
credential whose fingerprint it cannot vouch for. The `base64` crate arrives for this;
nothing in `riabuild-cli` does base64 today, and SHA-256 is already free through `ring`,
which rustls builds regardless.

## 7. The agent

Private key material never touches a filesystem. `crates/remote/src/issued/agent.rs`
runs an `ssh-agent` riabuild owns:

```
~/.riabuild/agent/<remote-hash>/     0700
  ├─ sock                            the agent socket   — not a secret
  └─ <key-id>.pub                    public halves      — not a secret

spawn   ssh-agent -D -a <sock>          foreground; a child riabuild can kill
run     ssh-add -t 900 -                stdin = the private key
probe   ssh -o IdentityAgent=<sock> -o IdentitiesOnly=yes -i <key-id>.pub \
            -o BatchMode=yes <target> true
stop    explicit, on success and on every error path
```

The private key reaches `ssh-add` on **stdin**, never in an argument vector, for the
reason `RunOptions.stdin` already documents: `ps` shows an argv to every process on the
machine, and on a shared server that includes every other developer.

`-D` rather than the default. `ssh-agent` without it forks and daemonises, which leaves
riabuild holding no handle to a process holding the org's keys. In the foreground it is an
ordinary child, and `ChildHandle::kill` ends it.

`-t 900` on the key, not on the agent. A `SIGKILL`ed riabuild orphans its children, and an
orphaned agent would serve those keys until the machine rebooted. A lifetime means the
worst case is fifteen minutes, and nothing legitimate needs them longer: §8 spends them
before the install step, not across the developer's shell.

**Public halves are written to disk on purpose.** They are not secret, and with an agent
loaded `-i <public-key-file> -o IdentitiesOnly=yes` is what selects *exactly one* agent
identity. That buys two things: the terminal can name which key got in, and each probe
offers a single key — where one connection offering all of them would hit sshd's
`MaxAuthTries` (6 by default) and silently stop before a developer's seventh key was ever
tried.

**No `ssh-agent` on `PATH` is a warning, not a stop.** The run continues to the password
path it would have taken before this feature existed. This crate's rule is that riabuild
stops when there is no way in, not when the convenient way in failed.

## 8. Where this joins the flow

Inside `authorise`, behind the `can_sign_in` early return that is already there:

```
can_sign_in?  ── yes ──▶ done. Nothing fetched, no agent, no keys in memory.
      │
      no
      ▼
fetch issued keys ─▶ start agent ─▶ probe each, in order
      │
  ┌───┴────────────────┐
one works          none works
  │                    │
  │                    ▼
  │          PreferredAuthentications=none probe   ── today's path, unchanged
  │          password → copy  |  keys-only → the hard failure quoted in "Why"
  ▼
ensure_key + copy::install_key, authenticated BY that issued key
stop the agent
every later ssh uses ~/.riabuild/ssh-identities/<hash>
```

Two properties of that placement are the whole design.

**A returning developer pays nothing.** `can_sign_in` succeeds on every run after the
first, so no fetch happens, no agent starts, and no org key is ever in that process's
memory. The cost of this feature is paid only on the runs that need it.

**An issued key bootstraps; it does not replace.** It authenticates exactly one
`ssh-copy-id`, and riabuild's own per-laptop key carries the remaining ~10 connections of
the run. That keeps three things that already work: the `riabuild <member-id> …` key
comment, `remote forget`'s server-side cleanup grepping for it, and a server's auth log
distinguishing one developer from another. A team sharing one key directly would give all
three up — every developer would appear as one fingerprint, and `forget` would have
nothing of its own to remove.

### …unless the server will not have it

Amended after `ssh.cloudcli.ai`. A managed SSH gateway accepts the write to
`authorized_keys` and authenticates against its own registry regardless, so riabuild's own
key is installed, refused, and refused on every run thereafter. Bootstrap-only sent that
run to the account password — meaning an issued key authorised one `ssh-copy-id` for a key
that could never work and was then discarded, which is the whole feature achieving nothing
on exactly the class of machine it was built for.

So bootstrap is the *preference*, not an absolute. All three branches where riabuild's own
key has been installed and still cannot sign in now carry the issued identity for the rest
of the run rather than falling back to the password. Concretely,
`identity::ssh_options` grows an `Option<&Working>` threaded through every `ssh` remote
mode opens, and the carried identity is offered **beside** riabuild's own `-i`, never
instead of it — `IdentitiesOnly=yes` restricts the offer to the identities named, so
dropping riabuild's own would give up the key that works everywhere else, including on
this server once whoever runs it fixes the file.

Two consequences worth stating rather than discovering:

- **Attribution is lost on those servers**, and only on those. They were never going to
  provide it: the key riabuild installed is the one being ignored.
- **The agent lives for the whole run**, so the probe's `-t 900` is wrong for a carried
  key — an interactive shell outlasts it, and the clipboard channel's reconnect would fail
  with nothing on screen explaining why. `Issued::hold` reloads that one key with no
  expiry. The exposure that trades away is narrower than it sounds: the socket is in a
  `0700` directory owned by the developer, so an orphaned agent is reachable only by them,
  which is the footing their own `ssh-agent` already runs on.

The branch that changes is the last one. A keys-only server today reaches
`if !interactive` and fails; with an issued key that signs in, it never gets there.

`--check` gains the probe and nothing else. Probing writes nothing to the server, so it is
allowed on that path, and it upgrades

> --check: riabuild's key is not authorised on that server yet

to naming the issued key that can get in — which is the difference between a developer
knowing to run without `--check` and a developer filing a ticket.

## 9. What does not change

- `remote forget`. The issued keys are not this laptop's to forget, and nothing local
  holds them. What it removes is unchanged.
- `~/.ssh`. Issued keys reach `ssh` only through riabuild's own agent, and are never
  installed for the developer's general use.
- Rotation. `replaceKey` in the dashboard is the whole procedure: no laptop stores a key,
  so none has a stale one. A developer's next run fetches the new key; their current
  session is unaffected because it is already running on riabuild's own key.
- Local provisioning. `riabuild` without `remote` neither fetches nor uses an issued key.

## Layout

| File | What |
|---|---|
| `riabuild-web/convex/schema.ts` | `issuedKeys` |
| `riabuild-web/convex/lib/opensshKey.ts` | the parser, and its two rejections |
| `riabuild-web/convex/issuedKeys.ts` | list projection, create, replaceKey, setIssuedTo, remove |
| `riabuild-web/convex/http.ts` | `GET /api/v1/issued-keys` |
| `riabuild-web/src/components/IssuedKeys.tsx` | the lead-only section |
| `riabuild-web/src/lib/opensshKey.ts` | re-export for the browser preview |
| `riabuild-cli/crates/api/src/issued.rs` | the fetch, the type, both-ends validation |
| `riabuild-cli/crates/api/src/openssh.rs` | the Rust port of the parser |
| `riabuild-cli/crates/remote/src/issued.rs` | fetch-or-warn, and the probe order |
| `riabuild-cli/crates/remote/src/issued/agent.rs` | the owned `ssh-agent` |
| `riabuild-cli/crates/remote/src/authorise.rs` | the new branch |
| `riabuild-cli/crates/paths/src/lib.rs` | `agent_dir()` |

## Testing

**Rust, in-crate.** The parser against real `ed25519` and `rsa` fixtures, an encrypted
container, a truncated one, and a PEM that is not an OpenSSH key. Validation: a bad label,
a `publicKey` that is not a key line, and — the one this feature adds — a row whose
private and public halves disagree, each dropped with the rest of the list surviving. The
agent against `FakeRunner`: the key reaches `ssh-add` on stdin and appears in no argv, the
agent is spawned with `-D`, it is killed on the success path and on every error path, and
a missing `ssh-agent` warns and returns none. `authorise`: a keys-only server with a
working issued key now installs the key and succeeds, a keys-only server with none still
fails with the paste remedy, an issued key that probes false falls through to the password
path, and `can_sign_in` succeeding fetches nothing at all.

**Convex, vitest.** The parser's rejections. `list` has no `privateKey` field on any row.
The endpoint: candidate gets `{ keys: [] }`, a departed member gets 403, a developer gets
only rows naming them and never another member's, and each fetch writes
`issued_key.served` carrying the labels. The mutations: lead-only, derived fields ignored
from the client, and an `auditLog` row per change.

**Playwright.** A scenario per state of the section — empty, populated, a key mid-paste
with its preview filled in, and a paste that will not parse — screenshots looked at, per
`.claude/skills/visual-testing/SKILL.md`.

**E2E.** A container with `PasswordAuthentication no` whose `authorized_keys` trusts an
issued key. That run is a hard failure today and is the case the feature exists for.

## Out of scope

- Per-server scoping of a key. A key is issued to people, not to addresses; the probe
  tries the developer's keys against whichever server they chose.
- Certificate authorities. A CA-signed host or user certificate is a better answer than
  handing out private keys, and it is a different feature.
- Generating a key pair in the dashboard. riabuild issues keys that already exist
  somewhere; a key it generated would have no `authorized_keys` to appear in.
- Agent forwarding to the server. Nothing about an issued key is forwarded; the server
  gets riabuild's own key and nothing else.
- Any reveal path for a stored private key. See §5.

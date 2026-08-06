# Remote mode — Design

**Date:** 2026-08-06
**Status:** Approved
**Extends:** [`2026-08-04-riabuild-design.md`](2026-08-04-riabuild-design.md),
[`2026-08-06-linux-support-design.md`](2026-08-06-linux-support-design.md)

**Depends on** the Linux support work, which is unmerged at the time of writing. Remote
mode needs its owned `gh` and `infisical` (PR A) so that provisioning a server never wants
Homebrew, and its musl builds (PR B) so that there is a Linux binary to install. The
macOS-server path depends on neither.

## Purpose

`riabuild remote` turns a server into the Clubria environment and drops the developer into
a stress-resistant shell on it. The laptop becomes a terminal: it holds the SSH identity,
mints the server's riabuild session, and owns which riabuild version the server runs.

Nothing about what riabuild *is* changes. The same task DAG runs, the same checks decide,
the same failures carry the same four parts. It runs somewhere else.

## What remote mode is, and is not

**The setup logic runs on the server, in the server's own riabuild binary.** riabuild does
not push setup steps over SSH. This is the architecture rule "the server ships data, never
logic" applied one hop further out: a laptop scripting a shell on a server is the same
remote-code-execution channel as a server scripting a laptop, pointed the other way, and it
is untestable besides.

Exactly two commands are ever pushed at a server:

| Pushed command | Why it cannot be avoided |
|---|---|
| `uname -sm` | the binary to install cannot be chosen without knowing the platform |
| `cat > … && chmod +x` | landing the binary is the bootstrap, by definition |

Everything after that is riabuild talking to itself over SSH.

**The laptop is not provisioned.** `riabuild remote` runs the `login` task locally and
nothing else. A laptop that has never been set up can still drive a server, which is the
point of a thin-client workflow.

## Shape of the change

Three pull requests, in order. The first two are invisible to a developer and are worth
separating anyway: both are pure plumbing with real test surface, and reviewing them
underneath a new command would bury them.

| PR | Contents |
|---|---|
| **A** | `members.publicId` as required schema, the backfill and its staged deploy, the `memberPayload` field, and `Member.public_id` in the CLI |
| **B** | `Paths::tools_root()` and the root override, the namespace environment on `Ctx`, remote token-store selection, a target parameter on `download.rs` |
| **C** | `riabuild remote` — identity, host trust, install, setup, shell — and the container test |

---

# The developer's experience

## First run

```
$ riabuild remote

riabuild · Clubria environment
  signed in as Ada Lovelace <ada@clubria.dev> · developer · token in your macOS Keychain

Adding a server
  Hostname   build-01.fly.dev
  Port       [22]
  Username   [ada]
  This server will be known as build-01.

Connecting to ada@build-01.fly.dev
  ● SSH key — generated for this server
    fingerprint SHA256:qKqvB…3s  ·  is that the server you expected? [y/N] y
  ● Authorised — ssh-copy-id installed the key
    ada@build-01.fly.dev's password:
  ● Reachable — key-only sign-in works
  ● riabuild 2026.08.06 — installed at ~/.riabuild on the server

Checking build-01
  ● riabuild sign-in
  ◐ GitHub CLI — gh is not signed in to GitHub
    ! First copy your one-time code: 1A2B-3C4D
      Open https://github.com/login/device in your browser
  ● GitHub CLI
  ● Node and pnpm
  ● Project checkout
  ● Project secrets
    secrets written to .env.local from the dev environment

● Clubria environment active on build-01 — type `exit` to leave
ada@build-01 ~/Clubria/ada/ai-builders-hub $
```

## Every run after that

```
$ riabuild remote
Reconnecting to build-01 · ada@build-01.fly.dev
  ● Reachable · riabuild 2026.08.06 · current
Checking build-01
  ● 9 items already correct
● Clubria environment active on build-01 — type `exit` to leave
```

## More than one server

```
$ riabuild remote
Which server?
  1  build-01   ada@build-01.fly.dev       used 2 hours ago
  2  gpu        ada@gpu.internal:2222      used 6 days ago
  [1]
```

## Command surface

| Command | Does |
|---|---|
| `riabuild remote` | reconnect to the only saved server, or offer the list |
| `riabuild remote build-01` | reconnect to one by name |
| `riabuild remote ada@host:2222` | add a server without prompts |
| `riabuild remote list` | saved servers, with when each was last used |
| `riabuild remote forget build-01` | remove the key, revoke the session, clean the server |

`--check`, `--quiet` and `--project` forward to the remote run. `--no-shell` stops after
provisioning. Everything else is the same flag it always was.

`forget` does three things and reports on each: deletes the local key pair and the
`remotes.json` entry, revokes the server's session, and connects to remove riabuild's own
`authorized_keys` line and the namespace directory. If the server is unreachable it says
exactly what it could not clean up, so that the leftovers are known rather than assumed
gone. The shared toolchain is never removed — it belongs to whoever else is on that box.

## What the laptop sets on the remote invocation

```
RIABUILD_ROOT=~/.riabuild-remote/<public-id>
RIABUILD_REMOTE=1
```

Two variables, and the server's riabuild derives the rest: `GH_CONFIG_DIR`,
`GIT_CONFIG_GLOBAL` and the Claude profile directory all hang off `root()`. Deriving them
rather than passing them means the server's own re-runs — from inside the mosh shell, with
no laptop attached — produce exactly the same environment.

`RIABUILD_REMOTE=1` means three things, and they are one idea: this riabuild is managed
from a laptop. It selects the file token store over the platform keychain, it suppresses
the self-update check because no package manager owns that binary, and it puts the server's
name in the shell banner.

The `<public-id>` is the driving developer's own, taken from the member record the laptop
already holds.

## Prompts

`ui.rs` gains `ask(label, default)` and `confirm(question)`. Both refuse to run when stdin
is not a TTY, and say which flags to pass instead. A prompt nobody can answer is a hang —
the same rule the Linux design applies to `sudo`.

---

# Identity and trust

## What the laptop keeps

```
~/.riabuild/ssh-identities/<hash>       ed25519 private key, 0600, no passphrase
~/.riabuild/ssh-identities/<hash>.pub   comment: riabuild ada@build-01.fly.dev:22
~/.riabuild/ssh/known_hosts             riabuild's own, pinned on first connect
~/.riabuild/remotes.json                name, hash, host, port, user, last used
```

```json
{ "remotes": [ {
    "name": "build-01", "hash": "9f2c…", "host": "build-01.fly.dev",
    "port": 22, "user": "ada", "addedAt": 0, "lastUsedAt": 0,
    "sessionExpiresAt": 0, "lastSeenCliVersion": "2026.08.06"
} ] }
```

Names are allocated from the hostname's first label, disambiguated with `-2` on collision,
and are a local label only — the server never sees one. As with `state.json`, a file that
cannot be read degrades to "no saved servers" rather than to an error.

`hash` is the first 16 hex characters of `sha256("<user>@<host>:<port>")`. Deterministic,
so the same three answers always resolve to the same key — which is what makes the whole
flow safe to re-run — and a different username on the same box gets a key of its own.

The hash is taken over what the developer typed, not over a resolved address. `build-01`
and `build-01.fly.dev` are therefore two servers as far as riabuild is concerned. That is
predictable, which beats being clever about it.

## Host keys

Before anything is sent to the server, `ssh-keyscan` fetches its host key, riabuild shows
the fingerprint, and the developer confirms once. It is then pinned in riabuild's own
`known_hosts` and every later connection runs with `StrictHostKeyChecking=yes` against
that file.

**riabuild never reads or writes `~/.ssh`.** No managed block, no Include, no entry in the
developer's own `known_hosts`. A bad write to those files breaks SSH for everything on the
machine, not just for riabuild.

A host key that changes later is a hard stop with `safe_to_rerun: false`. Never an
auto-accept.

## Authorising the key

`ssh-copy-id -i <hash>.pub -p <port> -o UserKnownHostsFile=…`, which is bundled with the
OpenSSH client everywhere riabuild runs — Debian and Fedora ship it in
`openssh-client`/`openssh-clients`, and Homebrew marks its own formula
`keg_only :provided_by_macos`, which is how macOS declares it ships one. It is already
idempotent: it skips keys the server has, so a second run is a no-op.

If `which("ssh-copy-id")` ever comes back empty, riabuild stops and prints the exact
`authorized_keys` line to paste, rather than failing obscurely.

**The authorisation step deliberately does not pass `IdentitiesOnly=yes`.** The common
cloud-VM case is a box that already trusts the developer's existing key and has password
authentication disabled — that existing key is what authorises the new one. Every
connection *after* authorisation does pass `IdentitiesOnly=yes`, so riabuild only ever
presents the key it owns and can never be silently working through an agent.

## When the key cannot be installed

riabuild asks the server which methods it offers, by attempting
`-o PreferredAuthentications=none`; sshd names them in its refusal.

| Server offers | What happens |
|---|---|
| password or keyboard-interactive | `ssh-copy-id` prompts. The ordinary path. |
| publickey only, and no key works | riabuild prints the public key and the `authorized_keys` line |

The second row is not a prompt riabuild declines to show. When `PasswordAuthentication` is
off, sshd never offers the method, and there is nothing a typed password could be fed to.
Saying so beats prompting for something that cannot work.

---

# The remote's riabuild session

The laptop mints it, and writes it down on the server.

1. `riabuild remote` runs the ordinary loopback-OAuth login on the laptop, labelled after
   the server — `build-01.fly.dev`. The dashboard lists it as its own device, revocable on
   its own. **No `/api/v1` change**: the label is already a parameter of the flow.
2. The token is stored on the server at `<namespace>/session.token`, mode 0600.
3. `riabuild remote forget` revokes it.

No browser on the server, no keyring on the server, no SSH forwarding, no broker process.
The server can re-run `riabuild` on its own afterwards — including re-pulling rotated
secrets mid-session — which is what makes the mosh shell self-sufficient once the laptop
disconnects.

## The invariant this amends

`riabuild-cli/CLAUDE.md` says **No secrets in `~/.riabuild/`**, and its reason is that a
token on disk outlives the machine it was meant for: backups, synced folders, tarballs
sent to support. That reasoning was written about a laptop, which has a keychain.

The amendment is narrow and is to be written into `riabuild-cli/CLAUDE.md` in this change:

> A riabuild-managed **server** may hold its own session token at
> `<namespace>/session.token`, mode 0600. It has no keyring, the token is minted for that
> server alone, it is labelled and listed in the dashboard, and `riabuild remote forget`
> revokes it.

What the invariant exists to protect is unmoved. The Infisical credential is still
brokered per use, still passed through the environment rather than an argument list, and
still never written anywhere. A laptop still keeps its own session in the platform
keychain.

The store is selected by **being a remote namespace, not by platform**. `for_platform`
today branches on `cfg!(target_os)`; it gains a prior branch for remote mode. This matters
on macOS servers, where `security find-generic-password` cannot reach a login keychain
that an SSH session has not unlocked.

---

# Getting riabuild onto the server

`uname -sm` names the platform. The laptop then **downloads the release asset for that
platform through the existing verified `download.rs` path and streams it over SSH stdin**
into `~/.riabuild/riabuild/<version>/riabuild`, with a `bin/riabuild` shim — the
versioned-directory-plus-shim pattern `gh`, `infisical` and pnpm already use.

Downloading on the laptop rather than `curl`-ing on the server keeps digest verification in
the one place that already does it properly, and requires nothing installed on the box.
`download.rs` gains a target parameter, since the laptop is now fetching an asset for a
platform it is not.

## The assets already exist

`release.yml` writes `riabuild-<version>-<target>.tar.gz` for each target and appends each
digest to `riabuild-<version>-checksums.txt`, in the `<digest>  <filename>` format
`download::digest_for` already parses for Node's `SHASUMS256.txt`. All of it is attached to
the GitHub release.

So macOS servers need **no release-pipeline change at all**. The only dependency is that
the Linux design's musl targets are added to the same loop, producing
`riabuild-<version>-{x86_64,aarch64}-unknown-linux-musl.tar.gz` and their digests, beside
the `.deb` and `.rpm`.

The darwin binaries are ad-hoc codesigned on the runner, and a file arriving over SSH gets
no `com.apple.quarantine` extended attribute, so Gatekeeper never enters the picture.

## Versions are the laptop's business

The laptop compares the server's binary against the org's `minCliVersion` and
`latestCliVersion` on every connect, and repairs drift before setup runs. The server's
riabuild therefore never self-updates: no package manager owns that binary, the laptop
does. `RIABUILD_REMOTE=1` suppresses the update check on the remote side.

No `sudo`. Nothing outside the developer's home directory. Which is also what makes the
whole flow work on a container, a hardened host, or a box the developer does not
administer.

---

# Sharing one server

Several developers use one server through **one Unix account and one namespace each**.
riabuild uses whatever account the SSH login lands in and never creates users, so nothing
in this flow needs root.

```
~/.riabuild-remote/<public-id>/
```

A single-user VPS gets the same layout with one namespace, so there is no shared-versus-solo
branch anywhere in the code.

Each developer generates their own key on their own laptop and each runs `ssh-copy-id`
against the same account, appending their own line to `authorized_keys`. No coordination.

## What namespacing the root does not fix

Rooting riabuild's own state per developer is the easy half. Three pieces of shared state
sit outside it, and the first is dangerous because it fails silently:

| Shared state | What goes wrong | Fix |
|---|---|---|
| `~/.config/gh/hosts.yml` | Bob's `gh` is authenticated as Alice: clones, PRs and the org-membership check all run as the wrong person, and nothing errors | `GH_CONFIG_DIR=<ns>/gh-config` |
| `~/.gitconfig` | commits attributed to whoever provisioned last; `gh`'s credential helper writes here too | `GIT_CONFIG_GLOBAL=<ns>/gitconfig`, written with the member's own name and email |
| `~/Clubria/<repo>` | two developers, one working tree, two sets of branches, one `.env.local` | the checkout moves to `~/Clubria/<login>/<repo>` |

`github_cli`'s `check()` illustrates why this cannot be left to individual call sites.
Today it runs `gh auth status` and trusts the answer, because a laptop has exactly one gh
configuration. Under a shared account that answer is only meaningful *relative to a
configuration directory*. Miss the variable on one invocation and the check passes against
Alice's credentials while `apply()` writes Bob's.

**So `Ctx` carries the namespace environment as one value that every task's `RunOptions`
inherits**, rather than each task remembering to add it. A task that forgets is then not a
thing that can be written.

## The checkout path is readable, the namespace is not

The namespace is keyed by an opaque immutable id; the checkout is not, because a developer
`cd`s into it every day. `<login>` throughout this section is `members.githubLogin`. `~/Clubria/ada/ai-builders-hub` reads well and
`~/Clubria/550e8400-e29b-41d4-a716-446655440000/ai-builders-hub` does not.

Nothing durable rests on the readable half. The absolute path is recorded in the
namespace's `config.json` the first time it is chosen, so a later GitHub rename changes
nothing — the directory simply keeps the name it had. If the default path already exists
and belongs to another namespace, riabuild claims `<login>-2` rather than sharing a tree.

## The trust boundary, stated plainly

**Namespaces prevent collisions, not snooping.** Every namespace is owned by the same Unix
user, so mode bits buy nothing between developers on that box: Alice can read Bob's
`session.token`, his `.env.local`, and his gh token.

Sharing an account is therefore a decision that those developers are mutually trusted —
which they largely already are, holding the same Infisical secrets. What they gain over
each other is impersonation: acting as Bob in riabuild and on GitHub.

A box shared by people who should not be able to impersonate each other gets separate Unix
accounts instead. That needs no riabuild support: the identity hash already keys on
username, so `alice@box` and `bob@box` are two servers with two keys and two namespaces.

## `owner.json`

Each namespace holds one, naming the member it belongs to — login, display name, email.
The directory name is an opaque id, and somebody with a shell on that box has to be able
to tell whose namespace they are looking at. riabuild also reads its siblings, to name who
else shares the account when that matters.

---

# Immutable user ids

The namespace is keyed by `members.publicId`, a UUID minted when the member row is
created. A namespace must outlive a GitHub rename; keying it on `githubLogin` would orphan
a developer's whole environment the day they renamed their account, silently
re-provisioning them from scratch.

## It is core schema, not an optional extra

`publicId` is a **required** field on `members` and a **required** field of every member
payload. It is not optional anywhere, and no code path tolerates its absence.

That is a deliberate break rather than the additive change the `riabuild-api` skill
prescribes by default. The skill's rule protects *old CLIs in the field* against a server
that changed underneath them, and nothing here removes or repurposes a field they read —
they ignore an unknown one and keep working. The direction being broken is the other one:
a **new** CLI against an **old** deployment, which is ordered, not accidental. riabuild-web
deploys before a CLI release ships, always.

An identifier that half the rows might not have is not an identifier. Making it optional
would put an `unwrap_or_default()` between a developer and their home directory, and the
failure it produces — a namespace named nothing, shared by everyone whose row predates the
migration — is exactly the class of bug that is expensive to find on somebody else's
laptop.

| Where | What |
|---|---|
| `convex/schema.ts` | `publicId: v.string()` on `members` — required |
| member creation | mints `crypto.randomUUID()` |
| `convex/devSeed.ts` and the dashboard scenario fixtures | every fixture member carries one |
| `convex/http.ts` | `memberPayload` always returns `publicId` |
| `api/mod.rs` | `Member` gains `#[serde(rename = "publicId")] pub public_id: String`, no `default` |

## Reaching a required field takes two deploys

Convex validates existing documents against the schema at push time, so a required field
cannot be introduced onto a populated table in one step. The sequence is a deployment
mechanic, not a design compromise, and the end state is the same either way:

1. push the field as optional, changing nothing that reads it
2. run the one-shot `internalMutation` that mints a UUID for every row without one
3. push it as required

Step 3 is the gate: it fails loudly if step 2 missed a row, which is the property worth
having. All three land in one pull request; only the deploy is staged.

No `by_publicId` index. Nothing looks a member up by it — the namespace is computed on the
CLI side from the member payload — and the Convex guidelines are explicit that indexes get
added when a caller needs one.

**Known limit:** a member deleted and re-created gets a new `publicId` and therefore a
fresh namespace, orphaning the old one. `owner.json` is what makes an orphan identifiable.
Reclaiming them is not in scope.

---

# The shared toolchain

Tools live where they always did — `~/.riabuild/` — and are shared by everyone on the
account. Only per-developer state is namespaced.

```
~/.riabuild/                          shared
  node/22.23.1/  pnpm/11.2.0/         Claude Code installs into node's global
  gh/2.97.0/  infisical/0.43.120/     prefix, so it shares without being asked to
  riabuild/2026.08.06/riabuild        two developers on two versions coexist

~/.riabuild-remote/<public-id>/       one developer's
  state.json  config.json  org-settings.json
  session.token                       0600
  claude/<uuid>/                      CLAUDE_CONFIG_DIR
  gh-config/                          GH_CONFIG_DIR
  gitconfig                           GIT_CONFIG_GLOBAL
  shell/  bin/  logs/
  owner.json
```

`Paths` gains `tools_root()`. On a laptop it equals `root()`, so nothing changes there; on
a server `root()` is the namespace and `tools_root()` is `~/.riabuild`. `node_dir`,
`pnpm_dir` and `tool_dir` move onto it. Everything else stays where it is.

Shims stay per-namespace. They are regenerated on every run and cost nothing, and two
developers rewriting one set of files concurrently is a race with no upside.

## Concurrency is the price of sharing

Two developers can now decide the same version is missing at the same moment.

**Every install extracts into a temporary sibling and `rename(2)`s into place.** A
concurrent reader sees a complete tree or nothing, never a half-extracted one. The loser of
the race finds the destination already present and treats that as success — which is
`apply()` being safe to run twice, not a special case.

**No lock files.** A stale lock on a shared box is a worse failure than a wasted download,
and it is a failure nobody can diagnose from the developer's end.

Nothing is ever overwritten or deleted, because the directories are versioned. A developer
pinned to an older Node keeps working when somebody else installs a newer one. Reclaiming
old versions is not in scope.

**Claude Code is the one exception**, arriving through `npm install -g` into the shared
Node prefix rather than through an atomic rename. npm stages and renames internally, and
`check()` is authoritative, so a collision is repaired by the re-run that follows it. Worth
knowing about rather than worth locking against.

---

# Setup, and the shell

Setup is `ssh -t … riabuild --no-shell` with the namespace environment set: the real task
DAG, riabuild's normal output, and a TTY — which `gh auth login --web` requires, since it
goes through `run_interactive` and prints a one-time code the developer must copy.

Then the shell:

```
mosh --ssh="ssh -i <identity> -p <port> -o …" <user>@<host> -- riabuild shell
```

If `mosh-server` is missing or the UDP handshake fails, riabuild falls back to `ssh -t`
with keepalives and notes the one command that would enable mosh. A blocked UDP port is a
cloud-firewall default, not a developer error, and must never be a dead end.

`riabuild shell` on the server reads only its namespace's configuration, so it needs no
session and no network. The banner names the server.

---

# macOS servers

Supported, on aarch64 and x86_64. Three things differ, and two of them make the design
simpler rather than more complicated.

**The token store stops being platform-chosen.** Over SSH the login keychain is locked, so
`security find-generic-password` fails. Remote mode already selects the file store by being
remote, so macOS validates that rule instead of complicating it.

**The checkout path stops branching per OS.** macOS's local default is
`~/Documents/Clubria/<repo>`, and `~/Documents` is TCC-protected: over SSH it returns
*Operation not permitted* unless somebody grants `sshd-keygen-wrapper` Full Disk Access in
Privacy & Security. A remote checkout is therefore always `~/Clubria/<login>/<repo>`,
on every platform, which designs out a notorious macOS failure instead of documenting a way
around it.

**Remote Login is the developer's job.** riabuild cannot enable sshd over SSH. An
unreachable Mac gets a failure naming System Settings → General → Sharing → Remote Login.

## The warning at connect

Claude Code keeps its credentials in the macOS login keychain rather than in
`CLAUDE_CONFIG_DIR`. Two consequences follow, and both are told to the developer on **every
connect to a macOS server**, naming who else is affected from the sibling `owner.json`
files, so it reads as information rather than boilerplate:

```
▲ macOS server: Claude Code keeps its credentials in this account's login
  keychain, not in your riabuild profile. You share one Claude sign-in with
  @bob and @carla, and unlocking the keychain over SSH exposes it to them.
```

- The keychain is locked over SSH, so a Claude sign-in there needs
  `security unlock-keychain`, which does prompt correctly over SSH.
- If the keychain item is keyed only by service name and not by `CLAUDE_CONFIG_DIR`, two
  namespaces in one Unix account share one Claude sign-in. This is the one collision
  namespacing cannot fix.

Whether the second is true is an **open item for the implementation plan** — it is to be
verified against a real macOS host, not assumed. If it holds, a shared macOS account
supports one Claude sign-in at a time and the warning above is the whole mitigation. Linux
servers are unaffected, Claude Code using a file there.

---

# Failure modes

Each one has its own remedy, so each one is detected separately.

| What went wrong | What riabuild says |
|---|---|
| Server unreachable | the `ssh -v` tail as detail; on macOS, how to enable Remote Login |
| Server offers password or keyboard-interactive | `ssh-copy-id` prompts — the ordinary path |
| Server is publickey-only and no key works | the public key and the `authorized_keys` line to paste |
| Host key changed since last time | hard stop, `safe_to_rerun: false` |
| Architecture with no published build | stop; linux and macOS, x86_64 and aarch64 only |
| Remote riabuild below `minCliVersion` | the laptop repairs the binary before setup runs |
| Deployment older than this CLI, so no `publicId` | the member payload fails to decode; riabuild says the dashboard needs deploying, rather than reporting a serde error as its own bug |
| `mosh-server` missing, or UDP blocked | falls back to `ssh -t`, notes the install command |
| Default checkout path owned by another namespace | claims `<login>-2` |

---

# Code layout

```
src/remote/
  mod.rs        the Remote type, the hash, the flow
  store.rs      remotes.json, name allocation
  identity.rs   keypair, ssh-keyscan and the fingerprint prompt, ssh-copy-id
  session.rs    minting the server's session and writing it down
  install.rs    uname, version comparison, streaming the binary
  shell.rs      mosh, with the ssh fallback
```

One concern per file, as `riabuild-cli/CLAUDE.md` requires, and none of them near 300
lines.

**The `Task` registry is deliberately not reused.** Those tasks describe *this machine* and
record state per task id in `state.json`; these steps are per-server and strictly
sequential. They keep the discipline that matters — every step idempotent, every step
re-verified after acting — without contorting a DAG to hold something that is not one.

Every subprocess goes through `CommandRunner`, without exception. That is what makes the
entire flow testable with no server anywhere.

---

# Testing

| Layer | Approach |
|---|---|
| Hashing and name allocation | pure functions, unit-tested |
| The flow | `FakeRunner` scripted with canned `ssh`, `ssh-keyscan`, `ssh-copy-id`, `uname` and `mosh` output, including every failure row above |
| Auth-method probing | canned sshd refusals for password, keyboard-interactive and publickey-only |
| Asset selection | `uname -sm` output per platform asserted against the exact release asset names |
| Namespace environment | a task's `RunOptions` asserted to carry `GH_CONFIG_DIR`, `GIT_CONFIG_GLOBAL` and the namespaced root — the check that stops the silent wrong-identity bug |
| `publicId` is required | a `/api/v1/me` payload without the field is a decode failure carrying the deploy-ordering message — never a default, never an unnamed namespace |
| The backfill | a fixture member row without `publicId` gains one, and the required-schema push rejects a table the backfill missed |
| Shared installs | two concurrent installs of one version, asserting one complete tree and two successes |
| End to end | CI runs `riabuild remote` against an sshd container: two namespaces in one account, asserting isolated gh configuration, git identity and checkouts, and one shared toolchain |

That last row earns its cost for the same reason the Linux design's container test does. A
namespace variable missing from one `gh` invocation produces a run that looks perfectly
healthy in every log and attributes a developer's work to somebody else.

---

# Not in scope

- Windows servers, and any architecture with no published build
- Creating Unix accounts, or anything else needing root on the server
- Provisioning the laptop and a server in one command
- Session persistence across a laptop reboot. mosh survives sleep and roaming, not client
  death; tmux is a separate change if it is wanted.
- Reclaiming orphaned namespaces or superseded tool versions
- Protecting developers sharing one Unix account from each other. See the trust boundary.
- Any change to `/api/v1` beyond the required `publicId` field

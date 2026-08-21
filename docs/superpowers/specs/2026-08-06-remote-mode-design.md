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

**The laptop runs two tasks, and no more.** `riabuild remote` runs `login` and
`github_cli` locally: the first because the laptop mints the server's riabuild session, the
second because the laptop's GitHub sign-in is what the server borrows. Everything else —
Node, pnpm, the checkout, Claude Code, secrets — happens on the server.

Running `github_cli` locally is also a pre-flight worth having. Its check re-verifies
Clubria org membership, so a suspended or departed developer fails on their own laptop
before riabuild touches a server.

## Shape of the change

Three pull requests, in order. The first two are invisible to a developer and are worth
separating anyway: both are pure plumbing with real test surface, and reviewing them
underneath a new command would bury them.

| PR | Contents |
|---|---|
| **A** | `members.memberId` as required schema, the backfill and its staged deploy, the `memberPayload` field, `DELETE /api/v1/cli/sessions/<id>`, `Member.member_id` in the CLI, and the dashboard showing member ids |
| **B** | `Paths::tools_root()` and the root override, the scoped runner, remote token-store selection, digest-verified tools, a target parameter on `download.rs` |
| **C** | `riabuild remote` — identity, host trust, install, setup, shell, the per-session GitHub credential — and the container test |

---

# The developer's experience

## First run

```
$ riabuild remote

riabuild · Clubria environment
  signed in as Ada Lovelace <ada@clubria.dev> · developer · token in your macOS Keychain

Checking this laptop
  ● riabuild sign-in
  ● GitHub CLI

Adding a server
  Hostname   build-01.fly.dev
  Port       [22]
  Username   [ada]

    Name this server [build-01]

Connecting to ada@build-01.fly.dev
  ● SSH key — generated for this server
    fingerprint SHA256:qKqvB…3s — trusting it
  ● Authorised — ssh-copy-id installed the key
    ada@build-01.fly.dev's password:
  ● Reachable — key-only sign-in works
  ● riabuild 2026.08.06 — installed at ~/.riabuild on the server

Checking build-01
  ● riabuild sign-in
  ● GitHub CLI — signed in as @ada, from this laptop
  ● Node and pnpm
  ● Project checkout
  ● Project secrets
    secrets written to .env.dev from the dev environment
    staging secrets written to .env.staging

● Clubria environment active on build-01 — type `exit` to leave
ada@build-01 ~/Clubria/ada/ai-builders-hub $
```

## Every run after that

Superseded by `2026-08-12-remote-picker-design.md`: a bare `riabuild remote` is one
prompt, whatever is saved. Enter takes the most recently used server, so this is still
the shape of a reconnect — it just asks first.

```
$ riabuild remote

Your servers:

  1  build-01    ada@build-01.fly.dev      used 3 hours ago
  2  gpu-bench   ada@gpu.internal:2222     used 5 days ago
  3  Add a server

  Connect without asking:  riabuild remote build-01
  Forget a server:         riabuild remote forget gpu-bench

    Which one? [1]
  ● Reachable · riabuild 2026.08.06 · current
Checking build-01
  ● 9 items already correct
● Clubria environment active on build-01 — type `exit` to leave
```

With nothing saved the questions come straight away, and with no terminal at all one
saved server is still reconnected to without asking — see that spec for why several are
refused instead.

## Command surface

| Command | Does |
|---|---|
| `riabuild remote` | pick a saved server or add one — `2026-08-12-remote-picker-design.md` |
| `riabuild remote build-01` | reconnect to one by name |
| `riabuild remote ada@host:2222` | add a server without prompts |
| `riabuild remote list` | saved servers, with when each was last used |
| `riabuild remote forget build-01` | remove the key, revoke the session, clean the server |

`--check`, `--quiet` and `--project` forward to the remote run. `--no-shell` stops after
provisioning. Everything else is the same flag it always was.

`forget` works from the far end inwards, because every step after the first needs the key
the last step deletes:

1. revoke the session — and stop here, loudly, if that failed
2. connect and remove this developer's `authorized_keys` line and their namespace
3. delete the local key pair and the `remotes.json` entry

Doing it in the other order would delete the key, leave `ssh -o IdentitiesOnly=yes` unable
to authenticate, silently skip the server-side cleanup, and then drop the entry that would
have let anyone retry.

Two details that matter on a shared account. The `authorized_keys` line is matched by the
**member id** in its comment, with a fixed-string `grep -vF` into a new file rather than a
`sed` pattern — on a shared account every developer's key comment contains the same
`user@host`, so matching on that would lock everybody else out of the box, and an unescaped
hostname in a regex matches more than it should. And no `.bak` is left behind, because the
whole point was removing a key rather than moving it next door.

The shared toolchain is never removed — it belongs to whoever else is on that box. If the
server is unreachable, `forget` says exactly what it could not clean up rather than
pretending.

`--check` never provisions. It reports what a run would do and stops before minting a
session, writing a token, or lending the server a GitHub sign-in — everywhere else in
riabuild that flag means *touch nothing*, and a check that ships your GitHub identity to a
server would be an unpleasant surprise.

## How a command reaches the server

**Never through the login shell, and never with a `~` in it.** `sshd` runs a remote command
string with the *user's* login shell, and this product supports fish — where
`RIABUILD_ROOT=… riabuild` is a syntax error rather than an assignment, and where a tilde in
an assignment is not expanded. csh differs again. mosh is worse: it `execvp`s the command
with no shell at all, so neither a `VAR=` prefix nor a `~` means anything.

So riabuild resolves the server's home directory once, on the first connection, and uses
absolute paths and `env` thereafter:

```
ssh … <host> /bin/sh -c 'printf %s "$HOME"'        once, cached in remotes.json
ssh … <host> env RIABUILD_ROOT=/home/dev/.riabuild-remote/<member-id> \
                 RIABUILD_REMOTE=build-01 \
                 /home/dev/.riabuild/riabuild/2026.08.06/riabuild --no-shell
mosh … -- /bin/sh -lc '<the same, quoted>'
```

Anything with more than one step — the install, writing the session file — is wrapped in an
explicit `/bin/sh -c '…'` rather than trusted to whatever shell the account happens to use.
`ssh` is always given `-F /dev/null` so a `Host` block in the developer's own
`~/.ssh/config` cannot redirect the connection or inject a `RemoteCommand`.

**Every interpolated value is single-quoted** through the `shell_quote` already in
`main.rs`, and every value that reaches a command line is validated at the boundary it
arrives on: `memberId` must be a UUID and `latestCliVersion` must be digits and dots,
both refused as a `Failure` otherwise. These arrive from a database field, and the root
`CLAUDE.md` calls the server-ships-data boundary load-bearing — a value from Convex
reaching an unquoted `ssh host "…"` is precisely the remote-execution channel that rule
exists to prevent.

### The two variables

`RIABUILD_ROOT` is an absolute path. `RIABUILD_REMOTE` carries the server's name, and any
non-empty value means remote — one variable serving both the decision and the shell banner.

`RIABUILD_REMOTE` means four things, and they are one idea: this riabuild is managed from a
laptop. It selects the file token store over the platform keychain, it puts the GitHub
configuration in a per-session runtime directory, it suppresses the self-update check
because no package manager owns that binary, and it names the server in the shell banner.

The server derives everything else from `root()` — `GIT_CONFIG_GLOBAL`, the Claude profile
directory — so a re-run from inside the mosh shell, with no laptop attached, produces
exactly the same environment.

**A `RIABUILD_ROOT` that is not absolute is a hard failure, never a default.** Silently
falling back to `~/.riabuild` would put every developer on the box in one namespace: one
`session.token`, so a candidate's riabuild would broker Infisical at a lead's role, and one
`gh` configuration, which is the silent wrong-identity bug this design exists to prevent.
The same check refuses `RIABUILD_REMOTE` set without a valid `RIABUILD_ROOT`, because
"remote but un-namespaced" must not be a state riabuild can be in.

## The git identity

`GIT_CONFIG_GLOBAL` points at `<namespace>/gitconfig`, and **riabuild writes that file** —
`user.name` and `user.email` from the member record — when it writes the namespace. Setting
the variable without creating the file would be worse than doing nothing: git stops reading
`~/.gitconfig` too, so the first commit on the server fails with *"Please tell me who you
are"* on a box where the developer never configured git in the first place.

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

A name is the developer's own label for a server, asked for once when the server is added
and offered a default: the hostname's first label, disambiguated with `-2` on collision.
The default is right when a developer connects straight to a machine and useless behind a
gateway, where every server reached through `ssh.cloudcli.ai` would otherwise be `ssh`,
`ssh-2`, `ssh-3` and `remote list` would stop telling anyone which box is which. The prompt
has a default, so an unattended run takes it rather than hanging — the crate-wide rule.

A typed name is reduced to letters, digits, dot, dash and underscore, and refused if
another server already has it: it is what `remote forget <name>` looks a server up by, and
it reaches the server as `RIABUILD_REMOTE=<name>` inside the single-quoted `env …` prefix
every remote invocation is wrapped in. Nothing else about it is sent anywhere. As with
`state.json`, a file that cannot be read degrades to "no saved servers" rather than to an
error.

`hash` is the first 16 hex characters of `sha256("<user>@<host>:<port>")`. Deterministic,
so the same three answers always resolve to the same key — which is what makes the whole
flow safe to re-run — and a different username on the same box gets a key of its own.

The hash is taken over what the developer typed, not over a resolved address. `build-01`
and `build-01.fly.dev` are therefore two servers as far as riabuild is concerned. That is
predictable, which beats being clever about it.

## Host keys

Before anything is sent to the server, `ssh-keyscan -t ed25519,ecdsa,rsa` fetches its host
keys and riabuild shows one fingerprint and pins it on sight — trust on first use, the shape
of `StrictHostKeyChecking=accept-new`. That one key goes into riabuild's own `known_hosts`,
and every later connection runs with `StrictHostKeyChecking=yes` against that file.

**There is no `[y/N]`.** There was, and what it bought was not verification: a developer with
no fingerprint to compare against can only answer yes, and an unattended run — CI, a
container, anything with no TTY — could not answer at all. The fingerprint is still printed,
so it is in the transcript for whoever does have one to check against, and
`--accept-host-key` is still compared exactly and still fails the run on a mismatch. What is
given up is the *first* connection to a server nobody named a fingerprint for; every
connection after it is verified against the pin.

**Every type is asked for, exactly one is pinned.** Displaying the first fingerprint and
pinning them all would have the developer approve, typically, the RSA key while the ed25519
and ecdsa keys they never saw were pinned alongside it — and the fingerprint a cloud console
gives them to compare against is usually the ed25519 one. So riabuild picks one key from the
answer, best type first (ed25519, then ecdsa, then RSA), and shows and pins that one. That
pin is the trust anchor for everything after it: the next thing that happens is a GitHub
token and a riabuild session going to whatever answered.

Asking for one type was the earlier design, and it was wrong in a way no test on a normal
OpenSSH box could show. A single-type scan cannot tell a server that offers only some
*other* type from a server that is not there: both come back empty, and riabuild has one
wording for empty — "reaching `<host>` on port `<port>`", which sends the developer off to
check their hostname, their port and whether SSH is running at all. SSHPiper, which fronts
several hosted SSH gateways, offers an RSA host key and nothing else, so every server behind
one was unreachable by that message and reachable by every other means.

Pinning one type is enough for `ssh` itself: OpenSSH reorders the host key algorithms it
offers to prefer what `known_hosts` already holds for a host, so a server offering both keys
is asked for the pinned one.

Matching an existing entry is on the exact host field, not a prefix. `build-01` and
`build-01.fly.dev` are deliberately two different servers here, and a `starts_with` would
treat one as already trusted, skip the prompt, and then fail under
`StrictHostKeyChecking=yes` with nothing explaining why.

**riabuild never writes `~/.ssh`, and ignores `~/.ssh/config`.** No managed block, no
Include, no entry in the developer's own `known_hosts`. Every invocation passes
`-F /dev/null`, because a `Host` block with `ProxyCommand`, `RemoteCommand` or `Hostname`
would otherwise change what riabuild connects to and what runs there. Saying "riabuild never
reads `~/.ssh`" without that flag would have been false: `ssh` reads it whether or not
riabuild names it.

A host key that changes later is a hard stop with `safe_to_rerun: false`. Never an
auto-accept.

For automation and the container test, `--accept-host-key <fingerprint>` supplies the answer
the prompt would have asked for; it matches or it fails. There is no "accept anything" flag.

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

1. `riabuild remote` asks riabuild-web for a session labelled after the server —
   `build-01.fly.dev` — through `POST /api/v1/cli/sessions`, authenticated with the
   laptop's own token. The dashboard lists it as its own device.
2. The token is stored on the server at `<namespace>/session.token`, mode 0600, and a copy
   stays in the laptop's own keychain under the account `remote:<hash>`.
3. `riabuild remote forget` **revokes** it — see below.

## The laptop asks; nobody approves anything twice

Step 1 originally ran the whole device-code flow a second time: a second code printed, a
second trip to `riabuild.clubria.com/cli`, a second approval. It proved nothing. The
laptop had signed in minutes earlier and was holding a live bearer token, and that token
is stronger evidence than a code the same person types into the same browser on the same
machine. So the laptop asks on the server's behalf instead:

```
POST /api/v1/cli/sessions   { deviceLabel }
  → authenticate the caller's session, confirm the member is active
  → re-verify org membership against GitHub
  → refuse if the caller's own session was itself delegated
  → mint, audit `cli.session_delegated`, return { token, sessionId, expiresAt }
```

Nothing here gives a *server* a way to sign itself in; it gives its laptop a way to ask.
Every session still traces back to exactly one browser approval by a human.

**One hop, and that is the security boundary.** A delegated session cannot delegate. Its
token sits on a server's disk under a Unix account several developers share, so it must be
assumed readable by a co-tenant — and a token that mints tokens turns one leaked
credential into an unlimited supply, including replacements minted *after*
`riabuild remote forget` revoked the original. That would make "its blast radius is that
server" false, which is the sentence the whole on-disk-token amendment rests on. The rule
lives in `sessions.delegate`, with the row, not in the endpoint that calls it.

The dashboard says which sessions arrived this way, on the row: it is the only place a
developer would notice a delegation they did not perform.

**There is no fallback.** A riabuild-web too old to have this endpoint answers 404, and
the CLI says so and stops — naming the deploy as the fix rather than reporting the generic
"HTTP 404" that reads as an outage. Quietly falling back would mean the two-approval dance
reappearing on a rolled-back dashboard with nothing explaining why.

## Revocation has to be real

The whole case for writing a bearer token to a server's disk is that it can be taken back.
`/api/v1` had no way to do that, so this design adds one:

```
DELETE /api/v1/cli/sessions/<id>
  → authenticate the session, confirm the member is active, re-verify org membership
  → refuse unless the session belongs to this member, or the caller is a lead
  → set revokedAt, write an auditLog entry
```

That is a second `/api/v1` change beyond `memberId`, and it is deliberate. Without it,
`forget` deletes the laptop's copy of a credential that remains live on the server, readable
by every co-tenant, and usable from any machine until it expires — so "its blast radius is
that server" would be false, and the amendment to the no-secrets invariant would rest on
something that does not happen. **`forget` fails loudly if revocation did not succeed**,
rather than reporting a tidy-up it did not perform.

The laptop also checks the stored token before reusing it: `sessionExpiresAt` from
`remotes.json` — riabuild-web's own `expiresAt`, recorded rather than recomputed here — and
a `GET /api/v1/me` under that token when it is close to expiry or has been revoked
elsewhere. A server cannot re-mint for itself: it has no browser for the device flow, and
delegation refuses a token that was itself delegated. So handing it a dead token strands it
with a 401 and no way forward, which is why the check happens before the write and not
after.

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

`release.yml` writes `riabuild-<version>-<target>.tar.gz` for each target, and
`riabuild-<version>-checksums.txt` carries a digest for each, in the `<digest>  <filename>`
format `download::digest_for` already parses for Node's `SHASUMS256.txt`. All of it is
attached to the GitHub release.

This paragraph described an intention rather than the workflow for longer than it should
have. The checksums file was built in the **macOS** job, so it listed the two darwin
tarballs and neither musl one, and because `ensure_riabuild` fetches checksums before the
tarball and refuses without a digest, every Linux server failed closed at the install step.
It is now assembled in `publish`, where all three build jobs' artifacts are already merged
into one directory — the reason it cannot simply be appended to per-job is that
`merge-multiple: true` makes three writers a race — and it names the four expected targets
rather than globbing, because the failure being fixed is a *missing* entry and a check that
only hashes what it finds agrees with itself no matter how little that is.

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
does. `RIABUILD_REMOTE` suppresses the update check on the remote side.

No `sudo`. Nothing outside the developer's home directory. Which is also what makes the
whole flow work on a container, a hardened host, or a box the developer does not
administer.

---

# Sharing one server

Several developers use one server through **one Unix account and one namespace each**.
riabuild uses whatever account the SSH login lands in and never creates users, so nothing
in this flow needs root.

```
~/.riabuild-remote/<member-id>/
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
| `~/.config/gh/hosts.yml` | Bob's `gh` is authenticated as Alice: clones, PRs and the org-membership check all run as the wrong person, and nothing errors | `GH_CONFIG_DIR` into a per-session runtime directory — see below |
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

This is a change to the `project` task, not a free function nobody calls: on a server it
asks for the namespaced path instead of `default_project_dir`. Left unwired, a macOS server
would put the checkout back in `~/Documents` — the TCC-protected directory this rule exists
to avoid — and two developers sharing an account would land in one working tree.

## The trust boundary, stated plainly

**Namespaces prevent collisions, not snooping.** Every namespace is owned by the same Unix
user, so mode bits buy nothing between developers on that box: Alice can read Bob's
`session.token`, his `.env.local`, and — while his session is live — his gh token. Making
the GitHub credential per-session narrows *when* that last one is there; it does not make
it private.

Sharing an account is therefore a decision that those developers are mutually trusted —
which they largely already are, holding the same Infisical secrets. What they gain over each
other is impersonation: acting as Bob in riabuild and on GitHub.

**And, without care, more than impersonation.** The toolchain is shared, so a co-tenant can
pre-create `~/.riabuild/riabuild/<next-version>/riabuild`, or a `node` or a `gh`, as a script
of their choosing — and every other developer would then execute it, with their own session
token in the environment. That is arbitrary code execution as each other, which is a
different thing from reading each other's files, and it is not something a shared account
should be assumed to have conceded.

What holds it shut is the rule in *the shared toolchain*, below: **riabuild verifies the
digest of what is on disk, not the version it claims**. A planted binary fails that check
and is replaced. This is the one place where the convenience of a shared toolchain has a
sharp edge, and it is worth knowing that the digest check is what dulls it.

A box shared by people who should not be able to impersonate each other gets separate Unix
accounts instead. That needs no riabuild support: the identity hash already keys on
username, so `alice@box` and `bob@box` are two servers with two keys and two namespaces.

## The direction this opens, and what keeps it shut

Everything above describes traffic in one direction: the laptop reaches into the server.
The clipboard channel
(`docs/superpowers/specs/2026-08-07-clipboard-channel-design.md`) opens the other one — an
`ssh -N -R` forward carrying a unix socket from the server back to the laptop, so that a
paste inside a remote Claude Code session reads the developer's own clipboard and a link
opens in their own browser.

**What makes a reverse tunnel defensible is that the server can only ask.** The operation
set is compiled into the laptop's binary. A server can request `clipboard.read`,
`clipboard.write`, `browser.open` and `channel.ping`, and nothing else: it cannot push work,
extend the operation set, or execute anything. That is *the server ships data, never logic*
applied to the one direction remote mode had not opened, and it is the same argument as the
task manifest — a channel whose operation list the server chose would be the manifest again
under another name.

**The socket is namespaced, for the reason the root is.** It lives at
`<namespace>/channel.sock`, not in the shared runtime directory. Without that, every
developer on the box resolves the same `$XDG_RUNTIME_DIR/riabuild/channel.sock` — they
share one Unix account, so they share one uid and one runtime directory — and Alice's
`xclip` would read Ben's laptop. Its parent is created **at** mode 0700 rather than created
and then chmod'd, so there is no window in which it exists at the umask.

**And the honest limit, which is the same limit as everything else in this section.** Mode
bits buy nothing between developers sharing a uid. Alice cannot reach Ben's socket by
accident, but she can reach it on purpose, and what that gets her is a genuine step past
reading his files: his laptop's clipboard, in both directions, and a URL opened on his
laptop. The answer is unchanged: people who should not impersonate each other get separate
Unix accounts.

"Refused rather than unlinked" is worth stating precisely, because it is **one side's rule,
not a property of the channel**. It is the *laptop's* create path — `socket_path_for_create`
refuses a path that is a symlink or owned by another uid, so a different account cannot
squat the name and be handed the channel. The server end does the opposite on purpose: the
forward carries `StreamLocalBindUnlink=yes`, without which a socket left by a killed
session blocks the rebind and the channel comes up permanently dead. Inside a per-developer
namespace the only thing that could be squatting is a same-uid co-tenant, which is the
limit conceded above — but a reader who takes the phrase as a global guarantee will believe
this design defends against something it does not.

**Two terminals into one server share one channel**, refcounted by `<pid>` markers on the
laptop with a `kill -0` sweep, the way the server's gh sessions already are. One caveat is
worth writing down rather than discovering: the supervisor is a task *inside* the owning
process, so "the last to exit tears down" holds only when the owner is last. An owner that
exits first takes the tunnel with it and a sibling terminal's paste stops — recovered by
reconnecting, and not otherwise, because the alternative is a daemon outliving the shell
and remote mode does not have one.

**The channel is optional, and its failure is not the session's failure.** A laptop that
closes its lid leaves a session that still runs setup, still re-pulls rotated secrets, and
still opens a shell. Only paste stops. Nothing in the connect flow may treat a tunnel that
would not start as a reason to withhold a shell.

## `owner.json`

Each namespace holds one, naming the member it belongs to — login, display name, email.
The directory name is an opaque id, and somebody with a shell on that box has to be able
to tell whose namespace they are looking at. riabuild also reads its siblings, to name who
else shares the account when that matters.

---

# The GitHub credential lives only as long as a session

A `gh` OAuth token is the developer's whole GitHub account, and a shared box is the last
place it should sit at rest. So it is the one piece of state that is **not** namespaced onto
disk:

```
GH_CONFIG_DIR = <runtime>/riabuild-gh-<member-id>/
```

`<runtime>` is the first of `$XDG_RUNTIME_DIR`, `$TMPDIR`, `/tmp` that exists and is a
directory this user can write. The order matters: `$XDG_RUNTIME_DIR` is a per-uid tmpfs that
logind clears, so on a systemd host the token never touches a disk at all; `$TMPDIR` is the
per-user directory macOS provides; `/tmp` is the floor that always exists. If none qualifies,
riabuild **stops** rather than falling back into the namespace — the property was the point.

### Creating it safely

The name is predictable — a member id is public, printed in the dashboard and visible as a
directory name on the box — and `/tmp` is world-writable and sticky. `create_dir_all`
followed by `chmod` is therefore wrong twice over: it succeeds on a directory another user
pre-created, and it leaves a window in which the directory exists at `0755` before `gh`
writes an OAuth token into it.

So the directory is created with `DirBuilder::mode(0o700).create()`, which fails if anything
is already there. If it does already exist, riabuild `lstat`s it and refuses unless it is a
real directory — not a symlink — owned by this uid, at mode `0700`. Anything else is a
`Failure`, not something to repair: on the documented `/tmp` floor, the alternative is
handing a developer's whole GitHub account to whichever local user got there first.

## What this is, and is not

**It is:** no GitHub credential at rest between sessions. A backup, a disk image, a
snapshot, or somebody who gets a shell on that box tomorrow finds nothing.

**It is not** protection from a co-tenant during a live session. Alice and Bob share a uid,
so while Alice's session is up her `hosts.yml` is as readable to Bob as anything else of
hers. `0700` is doing nothing between them. See *the token is the developer's own*, below —
on a shared box this is the sharpest edge in the design.

**It is not revocation.** `gh` deletes the local credential; the OAuth grant stays valid on
GitHub's side. A token captured during a live session keeps working afterwards. Only
revoking it on github.com stops that.

**And the token is the developer's own.** Because the server borrows the laptop's sign-in
rather than minting a separate one, what sits in that runtime directory during a session is
the credential that developer uses everywhere. GitHub revokes OAuth App authorizations per
app rather than per token, so the remedy for a captured one is revoking "GitHub CLI" for
the whole account and signing in again — on the laptop as well as the server.

That is the trade the seamless path makes, and it is only a trade at all on a **shared**
account. A server the developer has to themselves exposes the token to nobody but root.

Calling this "per-use" the way the Infisical brokering is per-use would be a lie. It
narrows a window; it does not close one.

## Why the riabuild session is treated differently

`session.token` stays in the namespace, persistent, while the GitHub token does not. That
asymmetry is deliberate: the riabuild session is minted for one server, labelled after it,
listed in the dashboard and revocable in a click, so its blast radius is that server. A
GitHub token is the developer's identity everywhere. The weaker credential can afford to
persist so that the server stays self-sufficient; the stronger one cannot.

`.env.local` is the other secret at rest on that box, and it stays — the application reads
it at runtime, so it cannot be ephemeral without breaking the thing riabuild exists to set
up. Naming it here makes it a decision rather than an oversight.

## Who holds the credential open

The obvious design — every riabuild process on the server registers on start and wipes when
the last one leaves — does not work, and the reason is worth writing down because it is not
obvious until it is: **each SSH invocation is a separate process**. Seeding, provisioning
and the shell are three of them. A per-process refcount would have the seeding process write
`hosts.yml`, exit, find itself the last one out, and delete the credential it had just
written. Milliseconds.

So the lifetime belongs to the one process that is actually long-lived: **the environment
shell**.

```
<runtime>/riabuild-gh-<member-id>/
  hosts.yml            gh's own
  sessions/<pid>       one marker per live environment shell
```

| Who | Does |
|---|---|
| the laptop, before seeding | asks the server to sweep — `riabuild internal gh-sweep` |
| the seeding run | writes `hosts.yml` through `gh`. No marker, no wipe. |
| the setup run | reads it. No marker, no wipe. |
| **the shell run** | writes its marker at start, removes it at exit, and wipes the tree if it was the last |
| a `riabuild` run *inside* that shell | reads it. No marker, no wipe. |

Sweeping **before** seeding rather than at the start of every run is what keeps the ordering
honest: a sweep between seeding and the shell would delete the credential that was just put
there.

Two terminals into one server share one sign-in, because the second shell finds a live
sibling marker and the laptop skips re-seeding.

**The sweep.** A marker whose process is gone — `kill -0`, through `CommandRunner` like
everything else — is removed. A directory left with no live marker at all is wiped, and only
*then* does the 24-hour age cap apply, to a tree whose markers all look dead. Applying the
cap to a live marker would delete a running developer's credential out from under them:
mosh sessions older than a day are the normal case, not the exception.

**Signals.** `SIGTERM`, `SIGHUP` and `SIGINT` wipe too. This is not redundant with the
sweep, and the earlier draft of this design was wrong to call it so. The reasoning was that
the shell is riabuild's child, so its death returns through the ordinary path — but mosh
exists precisely to keep a session alive when the client goes away, so a laptop that never
comes back leaves the shell up, and what eventually ends it is a signal from an
administrator or a reboot script. `SIGKILL` still cannot be caught, which is what the sweep
is for.

## What "at rest" honestly means

Even with all of the above, a credential survives as long as its session does, and a mosh
session can outlive a laptop indefinitely by design. If a developer never returns to that
server, nothing runs there to clean up until somebody connects again or the machine reboots.

So the property is: **no GitHub credential outlives the session that created it, and a
session that died without cleaning up is cleaned up before the next one starts.** That is
worth having. It is not "the token is gone within N minutes", and the spec should not be
read as promising that.

## Seeded from the laptop, not signed in on the server

The server never runs a device-code dance. Before the setup run, riabuild sweeps whatever a
dead session left behind, then opens one non-interactive SSH connection and seeds the
credential:

```
server:  riabuild internal gh-sweep            first, so it cannot delete what we are about to write
laptop:  gh auth token                         the laptop's own sign-in
    ↓    ssh … riabuild internal seed-github   token on stdin, never in argv
server:  gh auth login --with-token            gh writes its own hosts.yml, 0600
```

Three things make this the shape it is. The token travels on **stdin**, because an argument
list is world-readable through `ps` — the same rule `env_local` already follows for the
Infisical token. It is a **separate, non-interactive connection**, because the setup run
that follows needs `ssh -t` and a TTY leaves no clean stdin to write to. And the write goes
through **`gh auth login --with-token`** rather than riabuild hand-writing `hosts.yml`, so
gh owns its own file format and permissions while riabuild owns only the environment it
runs in.

`riabuild remote` is therefore unattended after the first key exchange: no device code, no
prompts, clone and secrets and all.

If seeding fails — a token the server rejects, an SSH hiccup — nothing special happens.
`github_cli`'s existing `check()` finds gh not signed in and its existing `apply()` runs the
device-code login over the TTY that setup already has. The fallback costs no new code
because it is the behaviour the task always had.

## What the developer sees

```
  ● GitHub CLI — signed in as @ada, from this laptop
```

Once per server per session, and once per *server* rather than once per terminal, which is
what the refcounting buys. `riabuild remote --check` reports GitHub CLI as satisfied, since
the seed happens before the check runs.

`GIT_CONFIG_GLOBAL` stays in the namespace. What `gh auth setup-git` writes there is a
credential *helper* line delegating to `gh`, not a credential, so it holds nothing worth
wiping — and when the gh config is gone, `git push` fails until the next sign-in, which is
the intended behaviour rather than a bug to work around.

A laptop is untouched by all of this: `GH_CONFIG_DIR` is only set in remote mode, and a
developer's own machine keeps the gh sign-in it has always had.

---

# Immutable user ids

The namespace is keyed by `members.memberId`, a UUID minted when the member row is
created. A namespace must outlive a GitHub rename; keying it on `githubLogin` would orphan
a developer's whole environment the day they renamed their account, silently
re-provisioning them from scratch.

## It is core schema, not an optional extra

`memberId` is a **required** field on `members` and a **required** field of every member
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
| `convex/schema.ts` | `memberId: v.string()` on `members` — required |
| member creation | mints `crypto.randomUUID()` |
| `convex/devSeed.ts` and the dashboard scenario fixtures | every fixture member carries one |
| `convex/http.ts` | `memberPayload` always returns `memberId` |
| `api/mod.rs` | `Member` gains `#[serde(rename = "memberId")] pub member_id: String`, no `default` |

**Not to be confused with `cliSessions.memberId`**, which already exists and holds a Convex
`v.id("members")` — the document reference. `members.memberId` is the durable public
identifier this design keys namespaces on. Two different values, one word; the schema
comments have to say so.

**Validated where it is decoded, not where it is used.** `member_id` is refused unless it is
a lowercase UUID, through a `serde` deserializer rather than a check somewhere downstream.
Two failures ride on this. It reaches a remote command line, so a value from the database is
otherwise a shell-injection channel into every developer's server. And an *empty* one makes
`~/.riabuild-remote/<member-id>` collapse to `~/.riabuild-remote`, which would put every
developer in one namespace and, worse, make `forget`'s cleanup `rm -rf` the directory
holding all of them. "An identifier that half the deployments might not send is not an
identifier" has to mean shape as well as presence.

## Reaching a required field takes two deploys

Convex validates existing documents against the schema at push time, so a required field
cannot be introduced onto a populated table in one step. The sequence is a deployment
mechanic, not a design compromise, and the end state is the same either way:

1. push the field as optional, changing nothing that reads it
2. run the one-shot `internalMutation` that mints a UUID for every row without one
3. push it as required

Step 3 is the gate: it fails loudly if step 2 missed a row, which is the property worth
having. All three land in one pull request; only the deploy is staged.

No `by_memberId` index. Nothing looks a member up by it — the namespace is computed on the
CLI side from the member payload — and the Convex guidelines are explicit that indexes get
added when a caller needs one.

## Shown in the dashboard

An id that names a directory on a shared server has to be readable by a human somewhere.
`owner.json` answers "whose namespace is this?" for somebody with a shell on the box; the
dashboard answers it for everybody else, and answers the reverse — "which directory is
mine?" — for the developer.

| Where | What |
|---|---|
| `Profile` | a `KeyValue` row, `member id`, so a developer can find their own |
| `LeadPanel`'s member table | an `id` column, `priority: "wide"` so it drops on a narrow viewport before `github` does |

A 36-character UUID is not a table cell. Both render through a new `Copyable` in
`src/ui/` — monospace, truncated to its first segment, full value in the accessible name,
with the copy affordance `Command` already implements. It is a new component rather than a
`Command` prop because `Command`'s `$` prompt means *this is a shell command*, and an
identifier is not one; the `riabuild-ui` rule to generalize rather than fork applies to
components that almost fit, and that one does not.

Per `visual-testing`, `Copyable` gets a `/__ui` gallery entry, and the `overflow` scenario
gains a member id — a 36-character unbroken string with no spaces is precisely the
adversarial case that scenario exists for.

**Known limit:** a member deleted and re-created gets a new `memberId` and therefore a
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

~/.riabuild-remote/<member-id>/       one developer's
  state.json  config.json  org-settings.json
  session.token                       0600
  claude/<uuid>/                      CLAUDE_CONFIG_DIR
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

The temporary name carries the pid and a random suffix. A fixed `.part` name would have two
developers installing the same version at the same moment writing into one file and renaming
the interleaved result into place — and "an ordinary situation on a shared box" is exactly
how this design describes that race.

**A tool is trusted by its digest, never by what it says its version is.** `check()` hashes
the binary on disk and compares it with the digest riabuild downloaded it against; a version
string is something any script can print. This is what stops a co-tenant planting an
executable that every other developer on the box then runs — see the trust boundary above.
On a laptop it costs one hash of a file that is already in the page cache; on a shared server
it is the difference between a toolchain and a foothold.

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

Supported, on aarch64 and x86_64. Three things differ, and all three make the design simpler
rather than more complicated — the fourth, a Claude Code sign-in shared across a Unix
account, turned out not to exist.

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

## Claude Code needs no special handling here

**There is no warning here, and an earlier draft was wrong to plan one.** That draft
asserted Claude Code keeps its credentials in the macOS login keychain rather than in
`CLAUDE_CONFIG_DIR`, and specified a connect-time warning saying everyone sharing a Mac
account shares one Claude sign-in.

Tested directly: `CLAUDE_CONFIG_DIR=/tmp/asd claude` on macOS **prompts for a fresh login**.
The credential is keyed to the config directory, not to the Unix account, so two developers
sharing an account on a Mac server get separate Claude sign-ins exactly as they do on Linux.
Namespacing already covers it, and there is no collision to warn anybody about.

The claim had also survived three adversarial reviews, because it was written as a
hedge — "an open item, to be verified against a real macOS host" — and a stated uncertainty
reads as diligence rather than as an unsupported premise doing load-bearing work. It had
already produced a warning, a trust-boundary paragraph, and a scope caveat by the time it
was checked.

Two smaller things remain true and are worth keeping:

- **The profile feature depends on this behaviour**, and it is undocumented — `shims/mod.rs`
  says so. Task 0 in the plan turns the observation above into an assertion, so a future
  Claude Code release that changes it fails a test rather than silently merging two
  developers' sign-ins.
- **Whether the store is a file or a per-directory keychain item is still unknown**, and it
  only matters in one narrow case: if it is a keychain item, a Claude sign-in *on a macOS
  server over SSH* may need `security unlock-keychain` first. That is one command in a
  failure message, not a design constraint, and the first person to use a Mac as a server
  will find out. Linux servers cannot hit it at all.

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
| A shared tool fails its digest | replaced, not trusted, whatever version it claims to be |
| `RIABUILD_ROOT` missing or not absolute on a server | hard stop; never a silent fall back to the shared root |
| `memberId` is not a lowercase UUID | hard stop at decode, before any command is built |
| Runtime directory exists but is not ours, or is not 0700 | hard stop; a GitHub token is not written into somebody else's directory |
| Session revocation failed during `forget` | hard stop, naming what is still live and where |
| Server's login shell is fish or csh | nothing to handle: no command riabuild sends is parsed by it |
| Deployment older than this CLI, so no `memberId` | the member payload fails to decode; riabuild says the dashboard needs deploying, rather than reporting a serde error as its own bug |
| `mosh-server` missing, or UDP blocked | falls back to `ssh -t`, notes the install command |
| Default checkout path owned by another namespace | claims `<login>-2` |
| No writable runtime directory for the gh configuration | stop rather than fall back to the namespace; the property was the point |
| The laptop's own gh is not signed in | fixed on the laptop by the `github_cli` task, before the server is touched |
| Seeding the credential failed | `github_cli` on the server falls back to its own device-code sign-in over the setup TTY |

---

# Code layout

```
src/remote/
  mod.rs        the Remote type, the hash, the flow
  store.rs      remotes.json, name allocation
  identity.rs   keypair, ssh-keyscan and the host-key pin, ssh-copy-id
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
| Runtime directory choice | `$XDG_RUNTIME_DIR`, then `$TMPDIR`, then `/tmp`, with the directory asserted 0700 |
| Session refcounting | two sessions share one sign-in; the first to exit wipes nothing; the second wipes the tree |
| Crash cleanup | a marker naming a dead pid is swept and its directory wiped on the next run, with no clean exit anywhere in the test |
| Seeding the credential | the token reaches `gh auth login --with-token` on stdin and appears in no recorded argument list — the same assertion `keychain.rs` already makes about `secret-tool` |
| Seeding failure | a rejected token leaves `github_cli` to sign in for itself, and the run still completes |
| `memberId` is required | a `/api/v1/me` payload without the field is a decode failure carrying the deploy-ordering message — never a default, never an unnamed namespace |
| The backfill | a fixture member row without `memberId` gains one, and the required-schema push rejects a table the backfill missed |
| Shared installs | two concurrent installs of one version, asserting one complete tree and two successes |
| Digest over version string | a planted binary that prints the right `--version` is replaced, not trusted |
| Remote command construction | every interpolated value single-quoted; a value containing `'`, `;`, `$(`, a space and a newline round-trips inertly |
| Namespace refusal | a relative or empty `RIABUILD_ROOT` on a server is a `Failure`, never the shared root |
| Runtime directory safety | a pre-existing directory owned by another uid, and a symlink, are both refused |
| Credential lifetime | seed, setup and an inner run leave `hosts.yml` in place; the shell run's exit removes it |
| Revocation | `forget` against a server whose revoke call fails stops and says so, leaving the entry in place |
| End to end | CI runs `riabuild remote` against an sshd container: two namespaces in one account, asserting isolated gh configuration, git identity and checkouts, one shared toolchain, and no GitHub credential anywhere on the filesystem once both sessions have ended |

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
- Revoking a GitHub token on sign-out. `gh` deletes the local credential and GitHub keeps
  the grant; revocation is a github.com action riabuild does not drive.
- Any change to `/api/v1` beyond the required `memberId` field and
  `DELETE /api/v1/cli/sessions/<id>`

# riabuild

Provisioning tool that gets a Clubria developer from "accepted a GitHub org invite" to
"running Claude Code against our codebase with working secrets" without them making a
single environment decision.

Two things are the exception, and riabuild *offers* both rather than imposing them.
**Which repository** they are working on: every run opens with the list they are authorized
to see and takes Enter for `ai-builders-hub`. And **where its source code lives**: first
setup for a repository shows the path it would use and takes Enter for yes, with
`riabuild move-project` changing it later. Everything else stays riabuild's decision — a
developer who presses Enter has still decided nothing.

Neither is a platform feature creeping in. A developer whose work is in a second Clubria
repository had no path through riabuild at all: they cloned by hand, and the toolchain, the
brokered `.env` files and the trusted Claude directory all stopped at the edge of the first
one. See `docs/superpowers/specs/2026-08-18-repository-picker-design.md`.

Design: `docs/superpowers/specs/2026-08-04-riabuild-design.md`. Read it before changing
anything structural.

## Layout

| Path | What |
|---|---|
| `riabuild-cli/` | Rust CLI — a cargo workspace of fifteen crates under `crates/`, the binary in `crates/cli/`. Shipped via Homebrew, apt, and dnf |
| `riabuild-web/` | Convex + Vite + React + Tailwind dashboard at `riabuild.clubria.com` |
| `e2e/` | the CLI and the backend tested together — `run.sh` on macOS, and `remote/` driving `riabuild remote` against a real Debian container. `e2e/README.md` |
| `packaging/` | the Homebrew, deb, and rpm templates — edit these, never the rendered copies; `ngrok/` and `grok/` hold the mirror scripts for the two tools nobody publishes a digest for |
| `Formula/riabuild.rb` | the rendered formula `brew tap` reads — written by the release workflow, never by hand |
| `docs/superpowers/specs/` | design specs |
| `docs/superpowers/plans/` | the implementation plans those specs were built from. History: read one to find out why something is the way it is, never as instructions for what to do now |
| `docs/releasing.md` | cutting a CLI release |
| `docs/deploying.md` | putting riabuild-web on the domain |
| `shared-build/` | the one cargo build directory every checkout and worktree compiles into. Untracked, created by a `SessionStart` hook — `riabuild-cli/CLAUDE.md` is the authority |
| `.claude/skills/` | repo skills — read the relevant one before the work it covers |

`riabuild-web/e2e/` is a different thing: the dashboard's Playwright suite. The
`e2e/` directory at the root tests the two deployables against each other.

Each subproject has its own `CLAUDE.md` with conventions specific to it.

## Workflow — not optional

**All work goes through a pull request. Work is not finished until PR CI has completed.**

```sh
git checkout -b <type>/<short-description>
# ... work ...
gh pr create --fill
gh pr checks --watch          # wait for completion — this is part of the task
```

Do not push to `main`. Do not report work as done while checks are queued, running, or
failing. If CI fails, fixing it is part of the same task, not a follow-up.

**Turn on `rerere`, once per clone.** Several branches are usually in flight at a time,
each rebasing on `main` as it moves, which means resolving the *same* conflict on every
rebase. `rerere` records a resolution the first time and replays it after that.

```sh
git config --local rerere.enabled true
git config --local rerere.autoUpdate true
```

`--local` writes to `.git/config` and the recorded resolutions live in `.git/rr-cache`,
both of which sit in the shared git directory rather than in any one worktree — so a
single run covers this clone *and* every worktree under `.claude/worktrees/`. Git cannot
carry a config value in a commit, which is why this is written down rather than shipped.

## Architecture rules

**The server ships data, never logic.** Setup tasks are compiled into the Rust binary —
versioned, auditable, distributed through signed Homebrew releases. riabuild-web provides
the org Claude settings JSON, the *default* repo slug, version floors, and brokered
tokens. It does not provide the list of repositories a developer may pick from: the CLI
asks GitHub for that through the developer's own `gh`, so GitHub does the authorizing and
riabuild holds no permission logic that could be wrong about it. A
server-driven task manifest would be a remote code execution channel onto every
developer's laptop. Do not cross this boundary for convenience.

The org settings may **name** a program and never **carry** one. The default status line
is `node ~/.riabuild/claude-statusline.js`; the script lives in
`riabuild-cli/crates/tasks/assets/`,
is compiled in with `include_str!`, and is installed by the `claude_statusline` task.
Editing that string in the dashboard cannot change what runs on a laptop — only an
upgrade can. A settings key whose value the server chose the *contents* of would
be the manifest again under another name.

**riabuild owns every tool it installs.** Node, pnpm, Claude Code, the Codex CLI, Grok
Build, `gh`, `infisical` and `ngrok` are downloaded by riabuild and verified against a
digest. No task
shells out to Homebrew, apt, or dnf to install a dependency — those exist to distribute
riabuild itself, nothing else. A provisioner that needs a package manager already set up cannot be the
first thing a developer runs.

Nor does riabuild run **another project's install script**, which is the same rule seen
from the other side. `x.ai/cli/install.sh` is the case that forces it into words: it is a
provisioner in its own right, and it downloads an unverified floating build, symlinks into
`/usr/local/bin`, and appends a `PATH` line to the developer's `.bashrc`, `.zshrc` or
`config.fish`. That last one silently demotes `~/.riabuild/bin` from the front of `PATH`,
which is where the `claude` launcher, the clipboard shims and the `xdg-open` that carries
links to the laptop all live. Nothing errors; the developer's own `claude` simply starts
instead of riabuild's. A one-line `curl | bash` in a task is a second provisioner fighting
this one.

**Where a project publishes no digest, riabuild republishes the artifact rather than
lowering the bar.** ngrok is the one that forces this: Equinox serves a single floating
build per platform, the version in the URL is decorative — `ngrok-v9.99.9-…` returns the
same bytes as `ngrok-v3-stable-…` — and there is no checksum file anywhere. So
`packaging/ngrok/mirror.sh` uploads the exact bytes we verified to a `Clubria/riabuild`
release and `tools.rs` pins the URL beside a `Checksum::Pinned` digest, which is what
`Formula/riabuild.rb` already does to riabuild itself. The two things this must never
become are a floating download nobody verifies, and a digest the *server* supplies —
which would let riabuild-web choose which bytes execute on a laptop, and is the task
manifest under another name.

**Grok Build is the second, and it fails a different half of the same rule.** xAI's URLs
are honest — `x.ai/cli/grok-1.0.5-linux-x86_64` names a real version and one nobody
published is a 404 — so the *pin* is not the problem. The digest is: no checksum file
exists at any spelling beside the artifact, and `install.sh`'s entire integrity check is
that the download runs. "It runs" is not "it is the right bytes", so
`packaging/grok/mirror.sh` republishes the four builds the same way. Mirroring rather than
pinning a digest against xAI's own URL is the conservative choice, not the paranoid one: a
version re-cut under the same name would become a checksum mismatch and a hard install
failure on every laptop at once, for bytes nobody can fetch any more.

That artifact is also the first riabuild owns that arrives in **no container at all** — an
uncompressed executable — which is what `archive::Kind::Raw` is for. It is mirrored
byte-for-byte and renamed to `.bin` rather than repacked into a tarball: a repack would
make the pinned digest describe riabuild's own output instead of the bytes xAI served,
putting an unverifiable step between what a maintainer checked and what a laptop runs. See
`docs/superpowers/specs/2026-08-21-grok-build-design.md`.

**pnpm is the tool a mirror cannot serve, and the answer is a second publisher rather than
a lower bar.** Its version is read out of the checkout's `packageManager` at *runtime*, so
no `Checksum::Pinned` constant in this repository can describe the bytes — pinning one to
make a mirror possible would turn a `packageManager` bump into a fleet-wide install failure
until a riabuild release caught up. pnpm's GitHub releases carry no checksum file at any
spelling, so for a while riabuild read the per-asset digest GitHub's REST API records. That
is a real digest served on a budget a provisioner cannot depend on: **sixty unauthenticated
requests an hour per address**, which one office behind one NAT exhausts, after which
nobody there can provision anything. Both e2e jobs stopped at exactly that.

So pnpm comes from the **npm registry** instead — the unscoped `pnpm` package, verified
against the `dist.integrity` sha512 npm recorded over the stored tarball, the field every
`npm install` already checks, with an SLSA provenance attestation beside it and no API
ceiling. The rule is unchanged and this is what obeying it looked like here: a digest the
*publisher* records, checked against the complete buffer before anything is unpacked, and a
version whose integrity cannot be established is an error rather than an unverified
download. What must never come back is the third option nobody proposes out loud —
downloading it because the transfer completed.

**And pnpm is the tool that shows what "riabuild owns every tool it installs" costs when it
is meant.** riabuild takes pnpm's *JavaScript* and runs it on the Node it downloaded itself,
rather than either of the executables pnpm publishes, because neither of those runs on the
machines this exists to provision: `@pnpm/linux-x64` needs `libatomic.so.1`, which stock
Debian, Ubuntu and Fedora do not ship, and `@pnpm/linuxstatic-<arch>` is musl and wants a
loader that is not there either. Node links neither, so the symptom was Node installing
fine and pnpm exiting 127 beside it — read as "pnpm is not installed", re-installed
perfectly, and reported as not having taken effect, for ever. The one-line fix is
`apt-get install libatomic1` on the machine, and it is refused everywhere including in
`e2e/remote/Dockerfile`'s own comments: a provisioner that needs a package manager already
set up cannot be the first thing a developer runs, and a green CI job bought that way is
one every developer on a stock server pays for silently. See
`riabuild-cli/crates/fetch/src/download/assets.rs`.

**Secrets are brokered, never stored.** riabuild-web holds the Infisical org credential
and mints short-lived access tokens on demand. No long-lived Infisical credential is ever
written to a developer's machine. Infisical service tokens are deprecated — use machine
identities with universal auth.

**A shared server shares an address, never a credential.** Leads enter the team's
servers in the dashboard and every developer's CLI reads them from
`GET /api/v1/remotes/shared` on every run — hostname, port, username, and nothing else.
The SSH key pair, the saved password and the riabuild session for one of those servers
belong to the single laptop that made them, because a session minted for one laptop is
not shareable. The CLI can neither add nor remove a shared server; what
`riabuild remote forget shared-<name>` removes is this laptop's own traces. See
`docs/superpowers/specs/2026-08-12-shared-servers-design.md`.

Two secrets riabuild *does* keep are named exceptions, both local to one machine, and
neither is brokered: **this machine's own riabuild session token**, and the SSH password
for a server riabuild's key cannot sign in to. Both go in the OS keychain where there is
one and a 0600 file where there is not — and "where there is not" includes a headless
Linux box whose `secret-tool` is installed but has no D-Bus session bus to talk to, which
is an ordinary machine to run a provisioner on and not a misconfiguration. `riabuild
remote forget` deletes a server's copies of both. See "No secrets in `~/.riabuild/`" in
`riabuild-cli/CLAUDE.md` for the reasoning and the storage rules. Nothing here loosens the
sentence above it: the Infisical credential is still minted per use and still never
written down.

**One secret riabuild-web keeps is neither brokered nor local: an issued SSH key.** A lead
pastes a private key into the dashboard and names the members it is issued to; their CLIs
pull it to reach servers riabuild's own key cannot sign in to — a managed bastion, a
hardened box with `PasswordAuthentication no`, anything whose `authorized_keys` the
developer does not administer. Say the cost out loud rather than discovering it later:
**it is stored in Convex in plaintext, it is readable by any lead, and a dump of that
database hands out working SSH access to whatever those keys open.** It is here because
the alternative is not a brokered key — it is that key arriving over Slack and living in
someone's `~/.ssh` forever. Bounding it is the whole design: the key is **derived** into a
public half and fingerprint so a lead never needs the secret back, no route returns a
stored private key to a browser, every fetch is audited by label, the CLI holds it only in
an `ssh-agent` riabuild owns and never on a filesystem, and it **bootstraps rather than
replaces** — it authenticates one `ssh-copy-id`, after which this laptop's own key carries
the run and `remote forget` still has exactly one developer's line to remove.

Bootstrapping is the preference, not an absolute: a managed SSH gateway accepts the write
to `authorized_keys` and then authenticates against its own registry regardless, so
riabuild's own key can never work there. Where riabuild's key has been installed and
*still* cannot sign in, the issued identity carries the rest of the run instead of the
account password — which is what it is for. Attribution is what that costs, and it is only
lost on the servers that were never going to provide it. See
`docs/superpowers/specs/2026-08-13-issued-ssh-keys-design.md`. Nothing here loosens the
two sentences above it either: the Infisical credential is still minted per use, and a
developer's own account password is still their own.

**The team's ngrok authtoken is a third server-held secret, and it lands on no
filesystem.** A lead sets one token in the dashboard and every developer tunnels with it.
Like an issued SSH key it is long-lived and stored in Convex in plaintext, so the same
sentence has to be said out loud: **a dump of that database hands out the team's ngrok
account.** What bounds it is that it never comes to rest anywhere else — no `ngrok.yml`,
no rcfile, no keychain entry. `~/.riabuild/bin/ngrok` is a shim that fetches the token
from `GET /api/v1/org/ngrok-token` on every invocation, puts it in that one process's
environment, and execs ngrok; no route returns it to a browser, the dashboard shows only
its last four characters, and every fetch is audited. Exporting it into the environment
shell instead would be cheaper and is deliberately not done: every process in that shell
inherits it, and one of them is Claude Code. The cost is attribution — ngrok sees one
account for the whole team, so `auditLog` is the only record of who opened what. See
`docs/superpowers/specs/2026-08-18-ngrok-design.md`.

**Identity is GitHub, authorization is Convex.** Membership in the Clubria GitHub org
gates access at all; `members.role` decides how much. Every secret-brokering request
re-verifies org membership, so the Convex role is never the sole gate.

## Scope

riabuild is a provisioner, not a platform. No agent session sharing, cost tracking, or
review flows. If a proposed feature does not shorten or de-risk the onboarding path, it
does not belong here.

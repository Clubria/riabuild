# riabuild

Provisioning tool that gets a Clubria developer from "accepted a GitHub org invite" to
"running Claude Code against our codebase with working secrets" without them making a
single environment decision.

The one exception is where their own source code lives, which riabuild offers rather than
imposes: first setup shows the path it would use and takes Enter for yes, and
`riabuild move-project` changes it later. Everything else stays riabuild's decision — a
developer who presses Enter has still decided nothing.

Design: `docs/superpowers/specs/2026-08-04-riabuild-design.md`. Read it before changing
anything structural.

## Layout

| Path | What |
|---|---|
| `riabuild-cli/` | Rust CLI — a cargo workspace of thirteen crates under `crates/`, the binary in `crates/cli/`. Shipped via Homebrew, apt, and dnf |
| `riabuild-web/` | Convex + Vite + React + Tailwind dashboard at `riabuild.clubria.com` |
| `e2e/` | the CLI and the backend tested together on macOS — `e2e/README.md` |
| `packaging/` | the Homebrew, deb, and rpm templates — edit these, never the rendered copies |
| `docs/superpowers/specs/` | design specs |
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
the org Claude settings JSON, the repo slug, version floors, and brokered tokens. A
server-driven task manifest would be a remote code execution channel onto every
developer's laptop. Do not cross this boundary for convenience.

The org settings may **name** a program and never **carry** one. The default status line
is `node ~/.riabuild/claude-statusline.js`; the script lives in
`riabuild-cli/crates/tasks/assets/`,
is compiled in with `include_str!`, and is installed by the `claude_statusline` task.
Editing that string in the dashboard cannot change what runs on a laptop — only an
upgrade can. A settings key whose value the server chose the *contents* of would
be the manifest again under another name.

**riabuild owns every tool it installs.** Node, pnpm, Claude Code, `gh`, and `infisical`
are downloaded by riabuild and verified against a published digest. No task shells out to
Homebrew, apt, or dnf to install a dependency — those exist to distribute riabuild itself,
nothing else. A provisioner that needs a package manager already set up cannot be the
first thing a developer runs.

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

**Identity is GitHub, authorization is Convex.** Membership in the Clubria GitHub org
gates access at all; `members.role` decides how much. Every secret-brokering request
re-verifies org membership, so the Convex role is never the sole gate.

## Scope

riabuild is a provisioner, not a platform. No agent session sharing, cost tracking, or
review flows. If a proposed feature does not shorten or de-risk the onboarding path, it
does not belong here.

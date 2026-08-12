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

Two secrets riabuild *does* keep are named exceptions, both local to one machine and
one server, and neither is brokered: a server's own session token, and the SSH password
for a server riabuild's key cannot sign in to. Both go in the OS keychain where there is
one and a 0600 file where there is not, and `riabuild remote forget` deletes both. See
"No secrets in `~/.riabuild/`" in `riabuild-cli/CLAUDE.md` for the reasoning and the
storage rules. Nothing here loosens the sentence above it: the Infisical credential is
still minted per use and still never written down.

**Identity is GitHub, authorization is Convex.** Membership in the Clubria GitHub org
gates access at all; `members.role` decides how much. Every secret-brokering request
re-verifies org membership, so the Convex role is never the sole gate.

## Scope

riabuild is a provisioner, not a platform. No agent session sharing, cost tracking, or
review flows. If a proposed feature does not shorten or de-risk the onboarding path, it
does not belong here.

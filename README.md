# riabuild

Gets a Clubria developer from "accepted a GitHub org invite" to "running Claude Code
against our codebase with working secrets" without them making a single environment
decision.

```
Lead      → invites the developer to the Clubria GitHub org
Developer → riabuild.clubria.com → "Sign in with GitHub" → confirm profile
          → brew install clubria/tap/riabuild
          → riabuild
```

## Install

```sh
brew tap clubria/tap https://github.com/Clubria/riabuild
brew install clubria/tap/riabuild
riabuild
```

macOS, Apple silicon or Intel. riabuild keeps itself current: it learns the
published version from the dashboard and runs `brew upgrade` on its own when a
newer one exists.

This repository *is* the tap — the formula is `Formula/riabuild.rb`, written by
the release workflow. The explicit `brew tap` line is what the second command
needs: Homebrew only auto-taps `clubria/tap` when it can guess the repository
name, and it guesses `Clubria/homebrew-tap`.

## Layout

| Path | What |
|---|---|
| `riabuild-cli/` | Rust CLI, shipped via Homebrew tap `clubria/tap` |
| `riabuild-web/` | Convex + Vite + React + Tailwind dashboard at `riabuild.clubria.com` |
| `packaging/homebrew/` | the formula template — edit this one |
| `Formula/riabuild.rb` | the rendered formula `brew tap` reads — generated, do not edit |
| `docs/superpowers/specs/` | design specs |
| `docs/deploying.md` | putting it on the domain |
| `docs/releasing.md` | cutting a CLI release |

## What `riabuild` does to a machine

Nine setup tasks, compiled into the binary, run in dependency order. Each one checks
whether the machine is *currently* correct, repairs it if not, and then re-checks —
a task never records a success it has not verified.

| # | Task | Checks |
|---|---|---|
| 1 | `login` | a live riabuild session, refreshed before it expires |
| 2 | `github_cli` | `gh` present, signed in, and able to read your Clubria membership |
| 3 | `infisical_cli` | `infisical` present — **no token is stored** |
| 4 | `toolchain` | riabuild-owned Node and pnpm at the versions the repo pins |
| 5 | `project` | the checkout exists and `origin` really is our repo |
| 6 | `repo_status` | reports ahead/behind and dirty state — **never pulls** |
| 7 | `claude_profiles` | Claude Code installed, with a profile of your own |
| 8 | `org_settings` | the team's Claude settings, cached and current |
| 9 | `env_local` | `.env.local`, freshly brokered, parseable, and git-ignored |

Then it drops you into your own shell with the environment applied.

## Working on a server

The same nine tasks, run over SSH against a Linux or macOS box instead of your laptop —
for a build machine, a GPU box, or anything else with more resources than a laptop has.
SSH is only the transport; the feature is **remote mode**.

```sh
riabuild remote                     # add or reconnect to a server, interactively
riabuild remote build-01            # by the name you gave it last time
riabuild remote ada@build-01.fly.dev:2222   # or spelled out, first time
riabuild remote list                # every server this laptop knows about
riabuild remote forget build-01     # undo everything below
```

The first connection generates an SSH key just for that server, shows you its host
key fingerprint once, and asks you to confirm it — same as `ssh` would the first time,
except riabuild remembers the answer in its own `known_hosts` rather than yours. From
there it authorises the key, installs its own binary on the server, mints the server a
session of its own (separate from your laptop's), lends it your GitHub sign-in for the
one setup run that needs it, and runs the same nine tasks against a namespace of its
own — `~/.riabuild-remote/<your-member-id>` — so several developers can share one Unix
account on the box without colliding. It finishes by opening a shell there, over `mosh`
where that is reachable and `ssh` otherwise, so a dropped connection or a laptop that
goes to sleep does not end your session.

`riabuild remote forget <name>` undoes all of it: it revokes the server's session on
riabuild-web first (so a network hiccup never leaves a live token nobody has a record
of), then removes what it left on the server — its namespace and its line in
`authorized_keys` — and only then deletes your local key and the saved entry. A server
riabuild cannot currently reach is still forgotten locally; what it could not clean up
on the server is reported, not silently dropped.

## Two rules that shape everything

**The server ships data, never logic.** Setup tasks live in the signed binary. A
server-driven task manifest would be a remote code execution channel onto every
developer's laptop.

**Secrets are brokered, never stored.** riabuild-web holds the Infisical machine
identity and mints short-lived tokens on demand; the CLI pipes them straight into
`infisical export`. Nothing long-lived is ever written to a laptop.

## Development

```sh
cd riabuild-web  && pnpm dev            # convex + vite
cd riabuild-web  && pnpm ui:check       # Playwright: every UI state × 3 viewports
cd riabuild-cli  && cargo test          # 112 unit tests, no machine state needed
```

Point the CLI at a local backend with `RIABUILD_API_URL` and `RIABUILD_WEB_URL`.

The dashboard is a fake TUI — one framed terminal, dark only, built from the
component library in `riabuild-web/src/ui/`. Any data state renders without a
backend via `?scenario=<name>`, and `/__ui` is the component gallery. See
`.claude/skills/riabuild-ui/` and `.claude/skills/visual-testing/`.

Versions are release dates — `2026.08.04`, plus a fourth component for a second
release on one day. Shipping is one command, with nothing to bump first:

```sh
git tag "v$(date -u +%Y.%m.%d)" && git push origin "v$(date -u +%Y.%m.%d)"
```

The tag is the only place a version is written down; `riabuild-cli/Cargo.toml`
holds a placeholder. `docs/releasing.md` covers why, and the rest.

All work goes through a pull request, and is not finished until CI has passed. See
`CLAUDE.md`.

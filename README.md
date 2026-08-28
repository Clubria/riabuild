# riabuild

Gets a Clubria developer from "accepted a GitHub org invite" to "running Claude Code
against our codebase with working secrets" without them making a single environment
decision.

```
Lead      → invites the developer to the Clubria GitHub org
Developer → riabuild.clubria.com → "Sign in with GitHub" → confirm profile
          → install riabuild (below)
          → riabuild
```

## Install

macOS on Apple silicon or Intel, Linux on x86_64 or aarch64. The dashboard shows
the block for your platform; all three are here.

**macOS**

```sh
brew install clubria/tap/riabuild
```

**Debian, Ubuntu**

```sh
curl -fsSL https://clubria.github.io/riabuild/clubria.gpg \
  | sudo tee /usr/share/keyrings/clubria.gpg >/dev/null
echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/clubria.gpg] https://clubria.github.io/riabuild/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/clubria.list >/dev/null
sudo apt update && sudo apt install riabuild
```

**Fedora, RHEL**

```sh
sudo curl -fsSL -o /etc/yum.repos.d/clubria.repo \
  https://clubria.github.io/riabuild/rpm/clubria.repo
sudo dnf install riabuild
```

Then `riabuild`.

riabuild keeps itself current: it learns the published version from the
dashboard and upgrades through whichever package manager installed it — asking
first on Linux, where that needs sudo. A copy no package manager owns is never
sudoed over; it prints the command instead.

This repository is the apt and the dnf repository: their indexes are rebuilt onto
GitHub Pages on every release. The Homebrew tap is
[`Clubria/homebrew-tap`](https://github.com/Clubria/homebrew-tap), a separate
repository holding nothing but the formula, because that is the name Homebrew
guesses from the tap name `clubria/tap` — which is what lets `brew install`
tap it without being told to. The release workflow writes the formula to both
that repository and `Formula/riabuild.rb` here; see
[docs/releasing.md](docs/releasing.md#the-tap-is-a-separate-repository).

The Linux binaries are statically linked against musl, so there is no
distribution or glibc requirement beyond what the packages declare.

## Layout

| Path | What |
|---|---|
| `riabuild-cli/` | Rust CLI — a cargo workspace of thirteen crates under `crates/` |
| `riabuild-web/` | Convex + Vite + React + Tailwind dashboard at `riabuild.clubria.com` |
| `e2e/` | the CLI and the backend tested against each other, and `riabuild remote` against a real container |
| `packaging/homebrew/` | the formula template — edit this one |
| `packaging/debian/` | the `.deb` control template |
| `packaging/rpm/` | the `.rpm` spec template and the dnf `.repo` file |
| `packaging/pages/` | the landing page for the apt and dnf repositories |
| `packaging/ngrok/`, `packaging/grok/` | the mirror scripts that republish the two tools whose projects publish no digest |
| `Formula/riabuild.rb` | the rendered formula, kept for laptops still tapped against this repository — generated, do not edit; the copy `brew install` reads is in `Clubria/homebrew-tap` |
| `docs/superpowers/specs/` | design specs |
| `docs/superpowers/plans/` | the implementation plans those specs were built from — history, not instructions |
| `docs/deploying.md` | putting it on the domain |
| `docs/releasing.md` | cutting a CLI release |
| `shared-build/` | the one cargo build directory every checkout and worktree compiles into — untracked, see `riabuild-cli/CLAUDE.md` |

## Which repository

Every run opens with the repositories you are authorized to see and takes Enter for
`ai-builders-hub`. That list comes from GitHub through your own `gh`, never from the
dashboard — GitHub does the authorizing, so riabuild holds no permission logic that could
be wrong about it. `--repo owner/repo` skips the question, which is what an unattended run
or a script wants.

The first setup for a repository also shows the path its checkout would take and takes
Enter for yes; `riabuild move-project` moves it later, and each repository keeps its own
checkout, its own brokered `.env` files, and its own trusted Claude directory. These two
are the only choices riabuild offers — a developer who presses Enter has still decided
nothing.

## What `riabuild` does to a machine

Eighteen setup tasks, compiled into the binary, run in dependency order. Each one checks
whether the machine is *currently* correct, repairs it if not, and then re-checks —
a task never records a success it has not verified.

They are listed here in declaration order, which is the order they read in; the engine
sorts by declared dependencies, so it is not necessarily the order they run in.

| # | Task | Checks |
|---|---|---|
| 1 | `login` | a live riabuild session, refreshed before it expires |
| 2 | `github_cli` | `gh` present, signed in, and able to read your Clubria membership |
| 3 | `git_credentials` | your own `git push` uses your GitHub sign-in, with no password to type |
| 4 | `infisical_cli` | `infisical` present — **no token is stored** |
| 5 | `ngrok` | `ngrok` present, and the shim that authenticates it — **no authtoken is stored**, it is fetched per invocation |
| 6 | `toolchain` | riabuild-owned Node and pnpm at the versions the repo pins |
| 7 | `project` | the checkout exists, where you said it should, and `origin` really is the repository you picked |
| 8 | `repo_status` | reports ahead/behind and dirty state — **never pulls** |
| 9 | `codex_cli` | the Codex CLI installed with riabuild's Node, and its nine profiles and ten launchers |
| 10 | `grok_cli` | Grok Build installed from riabuild's own mirror, and its nine profiles and ten launchers |
| 11 | `claude_accounts` | Claude Code installed, and at least one account of your own, signed in |
| 12 | `org_settings` | the team's Claude settings, cached and current |
| 13 | `claude_trust` | every account trusts the checkout, so no modal on first launch |
| 14 | `claude_onboarding` | every account past Claude Code's first-run questions |
| 15 | `claude_agents_view` | the agents view offered as each account's default, and never imposed on one that answered |
| 16 | `env_local` | one `.env.<environment>` per environment you may see — `.env.dev`, plus `.env.staging` for developers and leads — freshly brokered, parseable, and git-ignored |
| 17 | `claude_statusline` | the status line script the org settings name |
| 18 | `claude_plugins` | the marketplaces and plugins the checkout's own settings declare, installed before your first session rather than during it |

Then it drops you into your own shell with the environment applied, opening with a box
listing your Claude Code accounts and who is signed into each.

## Claude Code accounts

You can have up to nine, each with its own sign-in, its own sessions, and its own history.
In the environment shell, `claude` starts Claude Code on your primary account and
`claude-1` … `claude-9` start a particular one. All of them get the org's Claude settings
and trust the checkout.

| Command | What |
|---|---|
| `riabuild claude list` | your accounts and who is signed into each |
| `riabuild claude new` | adds an account and signs it in |
| `riabuild claude delete <n>` | signs it out and removes it; later accounts move up a number |
| `riabuild claude primary <n>` | makes account `<n>` the one `claude` runs |

`riabuild claude` on its own is `list`.

Each account lives in a config directory of its own, and `riabuild paths` prints which is
which — every Claude Code account against its `CLAUDE_CONFIG_DIR`, every Codex and Grok
Build profile against its `CODEX_HOME` and `GROK_HOME`, and riabuild's own tree beneath
them. You need it when something riabuild did not write has to be pointed at one of those
logins; the launchers set the variable themselves, so `claude`, `codex-3` and the rest
never do.

## Other agents, tunnels, and keys

`codex` and `codex-1` … `codex-9` are the Codex CLI, and `grok` and `grok-1` … `grok-9`
are Grok Build, each with nine sign-ins kept apart the same way Claude Code's are. riabuild
signs you in to neither: those are your own OpenAI and xAI accounts, and nothing about
onboarding waits on them.

`ngrok` is riabuild's own copy, and the team's authtoken never lands on your machine — the
shim fetches it from the dashboard for the one process it starts. Your lead sets it once
for everybody. Note what that costs: ngrok sees a single account for the whole team, so
the dashboard's audit log is the only record of who opened what.

A lead can also issue an SSH key to named members, for a bastion or a hardened box whose
`authorized_keys` you do not administer. Your CLI holds it in an `ssh-agent` riabuild owns
and never on disk, and it **bootstraps** — it authenticates one `ssh-copy-id`, after which
this laptop's own key carries the run.

## Working on a server

The same eighteen tasks, run over SSH against a Linux or macOS box instead of your laptop
— for a build machine, a GPU box, or anything else with more resources than a laptop has.
SSH is only the transport; the feature is **remote mode**.

A server needs nothing installed in advance. riabuild downloads and verifies its own
binary for whatever the box turns out to be — macOS or Linux, on x86_64 or arm64 — from
the same signed release your laptop installed from, and refuses to install one whose
digest is not in that release's checksums file.

```sh
riabuild remote                     # add or reconnect to a server, interactively
riabuild remote build-01            # by the name you gave it last time
riabuild remote ada@build-01.fly.dev:2222   # or spelled out, first time
riabuild remote list                # every server this laptop knows about
riabuild remote forget build-01     # undo everything below
```

The first connection generates an SSH key just for that server, shows you its host
key fingerprint once, and trusts it — the same trust-on-first-use `ssh
-o StrictHostKeyChecking=accept-new` does, except riabuild pins it in its own
`known_hosts` rather than yours, and checks every later connection against it.
Pass `--accept-host-key SHA256:…` when you have a fingerprint to hold it to: it has
to match exactly, or the run stops. From
there it authorises the key, installs its own binary on the server, mints the server a
session of its own (separate from your laptop's), lends it your GitHub sign-in for the
one setup run that needs it, and runs the same tasks against a namespace of its
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
`infisical export`. No long-lived Infisical credential is ever written to a laptop.

The exceptions are named rather than quiet, because a rule with unlisted exceptions stops
being a rule. This machine's own riabuild session goes in the OS keychain, or a 0600 file
where there is no keychain to answer, as does the password for a server riabuild's key
cannot sign in to. An issued SSH key lives only in an `ssh-agent`, and the team's ngrok
authtoken lands on no filesystem at all. Every one of those is local to one machine, and
none of them is the Infisical credential.

## Development

```sh
cd riabuild-web  && pnpm dev            # convex + vite
cd riabuild-web  && pnpm ui:check       # the whole Playwright suite
cd riabuild-cli  && cargo test          # unit tests only, no machine state needed
```

`pnpm ui:check` runs every Playwright spec, not only the visual one: each UI state at
380, 768 and 1440, and a smoke run that signs in against a local Convex deployment. A
test tagged `@viewport-agnostic` runs once, at 768 — running one at 380 and again at
1440 asserts the same thing about the same DOM.

Point the CLI at a local backend with `RIABUILD_API_URL` and `RIABUILD_WEB_URL`.

The dashboard is a fake TUI — one framed terminal, dark only, built from the
component library in `riabuild-web/src/ui/`. Any data state renders without a
backend via `?scenario=<name>`, and `/__ui` is the component gallery. See
`.claude/skills/riabuild-ui/` and `.claude/skills/visual-testing/`;
`.claude/skills/writing-setup-tasks/` and `.claude/skills/riabuild-api/` cover the
other two halves.

Versions are release dates — `2026.08.04`, plus a fourth component for a second
release on one day. Shipping is one command, with nothing to bump first:

```sh
git tag "v$(date -u +%Y.%m.%d)" && git push origin "v$(date -u +%Y.%m.%d)"
```

The tag is the only place a version is written down: the workspace root is a
virtual manifest with no version at all, and `riabuild-cli/crates/cli/Cargo.toml`
holds a permanent `0.0.0` placeholder. `docs/releasing.md` covers why, and the rest.

All work goes through a pull request, and is not finished until CI has passed. See
`CLAUDE.md`.

MIT licensed — see `LICENSE`. The `.deb` carries it as
`/usr/share/doc/riabuild/copyright` and the `.rpm` as `rpm -qL riabuild`, so the
notice travels with the binary rather than only with the source.

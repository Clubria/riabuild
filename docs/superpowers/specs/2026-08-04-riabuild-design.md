# riabuild — Design

**Date:** 2026-08-04
**Status:** Approved
**Scope:** Provisioner only. See [Non-goals](#non-goals).

## Purpose

riabuild gets a developer from "accepted a GitHub org invite" to "running Claude Code
against the Clubria codebase with working secrets" without them making a single
environment decision.

Everything in this document serves that sentence. A feature that does not shorten or
de-risk that path does not belong in v1.

One decision is deliberately offered rather than made: where the checkout goes. See
`2026-08-06-project-path-choice-design.md`. It is a decision developers reliably have an
opinion about, and the only one riabuild cannot quietly correct afterwards — but the
default is still riabuild's, and Enter accepts it.

## Non-goals

riabuild is a **provisioner**, not a platform. Out of scope for v1:

- Agent session sharing, visibility, or replay
- Cost or usage tracking
- Code review flows or team dashboards beyond member administration
- Anything that runs after the developer is in the shell

The name describes the outcome — developers building with us — not a feature set.

## System shape

Two deployables and one contract.

| Component | Stack | Distribution |
|---|---|---|
| `riabuild-web` | Convex + Vite + React + Tailwind | `riabuild.clubria.com` |
| `riabuild-cli` | Rust | Homebrew tap `clubria/tap` |
| Contract | HTTP, versioned at `/api/v1` | Convex HTTP actions (`convex/http.ts`) |

### The server ships data, never logic

Setup tasks are compiled into the Rust binary. They are versioned, auditable, and
distributed through signed Homebrew releases.

A server-driven task manifest would be more flexible, and would also be a remote code
execution channel onto every developer's laptop. The server provides the org Claude
settings JSON, the repo slug, version floors, and brokered tokens. It never provides
anything executable.

This boundary is load-bearing. Do not cross it for convenience.

## Onboarding flow

```
Lead      → invites the developer to the Clubria GitHub org (GitHub's own flow)
Developer → accepts, visits riabuild.clubria.com
          → "Sign in with GitHub"
          → server checks GET /user/memberships/orgs/Clubria
               not a member → "Ask your team lead for a GitHub invite."
               member       → profile screen (first / last / email, prefilled from GitHub)
          → install instructions: brew install clubria/tap/riabuild
Developer → $ riabuild
```

GitHub org membership **is** the invite. There is no invite link, no invite token, no
TTL, and no single-use bookkeeping — the org membership was always the real trust
boundary, and a second weaker copy of it is only more surface to get wrong.

The profile screen's three fields are prefilled from the GitHub OAuth profile and its
verified email list. The developer confirms or corrects them.

## riabuild-web

### Data model

Convex tables. All tokens are stored **hashed** — a data leak must not hand out live
sessions.

| Table | Fields |
|---|---|
| `authTables` | from `@convex-dev/auth`, GitHub provider only |
| `members` | `userId`, `githubLogin`, `githubId`, `firstName`, `lastName`, `email`, `role`, `status` |
| `cliSessions` | `memberId`, `tokenHash`, `deviceLabel`, `cliVersion`, `lastUsedAt`, `expiresAt`, `revokedAt` |
| `orgConfig` | `claudeSettings`, `repoSlug`, `minCliVersion`, `latestCliVersion`, `secretsUpdatedAt` |
| `auditLog` | `actorId`, `action`, `subjectId`, `meta`, `at` |

`members.role` is one of `candidate` | `developer` | `lead`, defaulting to `candidate`.
`members.status` is `active` | `suspended`.

### Authorization

Identity lives in GitHub. Authorization lives in Convex. Two rules keep them from
drifting:

1. **Every secret-brokering request re-verifies GitHub org membership.** Removed from
   the org means no secrets, regardless of what `members.role` says. The Convex role is
   never the sole gate.
2. **First-lead bootstrap** comes from a `RIABUILD_BOOTSTRAP_LEADS` environment variable
   listing GitHub logins. Without it, nobody can promote anybody and the dashboard is
   permanently inert.

Role promotion happens in the riabuild dashboard, performed by a `lead`, and writes an
`auditLog` entry.

### CLI login — loopback OAuth

The same shape `gh` uses. Chosen over device-code flow because the target is a macOS
desktop with a browser, and because it matches the intended feel: the dashboard sends
the developer straight back to their terminal.

```
CLI  → binds 127.0.0.1:<ephemeral>, generates `state` + PKCE verifier
     → opens https://riabuild.clubria.com/cli/authorize?state=…&challenge=…&port=…
Dash → developer is signed in via GitHub (or signs in now)
     → mutation records a one-time code, redirects the browser to
       http://127.0.0.1:<port>/callback?code=…&state=…
CLI  → POST /api/v1/cli/token { code, verifier }  →  session token
     → stores it in the macOS Keychain
```

The session token never touches disk. `state` is verified on the CLI side to reject
callbacks it did not initiate.

### Secret brokering

`POST /api/v1/secrets/token` authenticates the CLI session, confirms `status == active`,
re-verifies GitHub org membership, then performs universal-auth login against Infisical
using a **per-role machine identity**:

| `members.role` | Infisical machine identity | Scope |
|---|---|---|
| `candidate` | `mi-candidate` | subset of dev paths |
| `developer`, `lead` | `mi-developer` | all dev paths |

It returns a short-lived Infisical access token. The CLI pipes it directly into
`infisical export` and never persists it.

Three properties this buys:

- Path scoping is enforced by Infisical's own RBAC, not by riabuild code.
- riabuild-web never touches the secret payload — it brokers auth only.
- Revoking someone is one field flip in Convex, effective at their next token request.

> **Infisical service tokens are deprecated.** Infisical announced deprecation of Service
> Tokens and API Keys in April 2024 with a July 2024 migration deadline, in favor of
> Machine Identities. The CLI's non-interactive path is
> `infisical login --method=universal-auth --client-id … --client-secret …`, which returns
> a short-lived token. Do not design against service tokens.

### HTTP contract

All CLI requests carry `Authorization: Bearer <session token>`. The handler hashes it,
looks it up in `cliSessions`, rejects revoked or expired sessions, and updates
`lastUsedAt`.

| Endpoint | Purpose |
|---|---|
| `POST /api/v1/cli/token` | exchange one-time code for a session token |
| `GET /api/v1/me` | member profile, role, status |
| `GET /api/v1/org/config` | `repoSlug`, `minCliVersion`, `latestCliVersion` (plus a frozen `defaultProjectPath`, retired — see below) |
| `GET /api/v1/org/claude-settings` | org Claude Code settings JSON + `updatedAt` |
| `POST /api/v1/secrets/token` | short-lived Infisical access token |

### Where the checkout goes

The CLI decides, not the server. The default depends on the operating system —
`~/Documents/Clubria/<repo>` on macOS, `~/code/<repo>` elsewhere — and a single stored
string cannot be right on both at once. It lives in `paths.rs`, the one file allowed to
know which platform it is on. A developer who wants somewhere else passes
`riabuild --project <path>`, which is remembered in `~/.riabuild/config.json`.

`orgConfig.defaultProjectPath` used to carry this and is retired. `/api/v1/org/config`
still emits a frozen value for CLIs released before the change, because they cannot
deserialize a response without it and `/api/v1` is add-only. It can be dropped once no
installed CLI predates the change.

### Dashboard routes

| Route | Audience |
|---|---|
| `/` | developer: profile, active CLI sessions, revoke buttons, install instructions |
| `/` | lead: the above plus member list, role assignment, suspend, audit log |
| `/cli/authorize` | loopback approval screen |

### Auth stack note

`@convex-dev/auth` supports GitHub OAuth via `@auth/core` providers, so the existing
scaffold works — swap `Password` for `GitHub` in `convex/auth.ts`. Convex's own
documentation still labels Convex Auth **beta** and points new projects toward
third-party providers, and `@convex-dev/better-auth` is the actively maintained
component. Staying on `@convex-dev/auth` is the low-churn choice for v1; revisit if beta
instability costs us time.

## riabuild-cli

### Disk layout

```
~/.riabuild/
  state.json              task state: { id: { version, last_ok_at, last_reason } }
  config.json             project path and chosen defaults
  org-settings.json       cached org Claude Code settings
  node/22.23.1/bin/       riabuild-owned Node
  bin/  pnpm  c           standalone pnpm, profile launcher
  claude/<uuid>/          CLAUDE_CONFIG_DIR profiles
  shell/zsh/.zshrc        generated rcfiles
  logs/riabuild.log
```

The riabuild session token lives in the Keychain, never in this tree.

### Crate layout

Files stay small and single-purpose. When one grows past roughly 300 lines, that is a
signal it is doing too much.

```
riabuild-cli/src/
  main.rs              top-level flow
  cli.rs               clap definitions
  config.rs            ~/.riabuild layout, state.json load/save
  paths.rs             path resolution behind a trait (macOS now, Linux-shaped)
  keychain.rs          secret storage behind a trait (macOS impl, Linux stub)
  runner.rs            CommandRunner trait — every external process goes through this
  update.rs            version check, brew upgrade, re-exec
  ui.rs                terminal output and prompts
  api/                 riabuild-web client: mod, auth, org, secrets
  tasks/               mod (trait + registry + DAG runner) + one file per task
  shell/               mod, zsh, bash, fish
  shims/               generation of ~/.riabuild/bin entries
```

### The task engine

```rust
pub enum Reason {
    NeverRun,
    VersionChanged { from: u32, to: u32 },
    UpstreamChanged(TaskId),
    CheckFailed(String),
}

pub enum Status {
    Satisfied,
    Needs(Reason),
}

pub trait Task: Send + Sync {
    fn id(&self) -> TaskId;
    fn title(&self) -> &str;
    fn version(&self) -> u32;
    fn depends_on(&self) -> &[TaskId];
    fn check(&self, ctx: &Ctx) -> Result<Status>;
    fn apply(&self, ctx: &mut Ctx) -> Result<()>;
}
```

The runner topologically sorts by `depends_on`, then for each task in order:

1. No record in `state.json` → `Needs(NeverRun)`
2. Recorded version ≠ `version()` → `Needs(VersionChanged)`
3. Any dependency applied this session → `Needs(UpstreamChanged)`
4. Otherwise call `check()`

If the result is `Needs(_)`, run `apply()`, **then re-run `check()`**. If it still
reports `Needs(_)`, that is a hard error and the reason is surfaced to the developer. A
task that records success without verifying is worse than no task, because half the
value of a provisioner is telling the truth about the machine.

Only then is `{ version, last_ok_at, last_reason }` written to `state.json`.

**`check()` is authoritative.** `version()` exists solely as a forced-rerun escape hatch
for drift `check()` genuinely cannot observe. Every `apply()` must be safe to run twice.

A unit test asserts the dependency graph is acyclic and that every declared `depends_on`
names a registered task.

### Setup tasks

| # | Task | Depends on | Check |
|---|---|---|---|
| 1 | `login` | — | Keychain token present; `/api/v1/me` returns 200 with `status == active`. Refreshes proactively when expiring within 7 days. |
| 2 | `github_cli` | — | `gh --version` ≥ floor; `gh auth status` exits 0; `/user/memberships/orgs/Clubria` reports an active membership. The capability is tested, never the scope string: GitHub accepts five scopes there and folds `read:org` into `admin:org`. |
| 3 | `infisical_cli` | — | `infisical --version` ≥ floor. **No token is installed** — credentials are brokered per use. |
| 4 | `toolchain` | — | `~/.riabuild/node/<pinned>/bin/node -v` matches the repo's `.nvmrc`; `~/.riabuild/bin/pnpm -v` matches the repo's `packageManager` field. |
| 5 | `project` | 2 | configured directory exists, is a git repo, `origin` is `Clubria/ai-builders-hub`. |
| 6 | `repo_status` | 5 | **Reports only.** Ahead/behind counts and dirty-tree state. Never pulls. |
| 7 | `claude_profiles` | — | at least one UUID-named profile directory exists; `claude --version` ≥ floor. |
| 8 | `org_settings` | 1 | `org-settings.json` is valid JSON and matches the server's `updatedAt`. |
| 9 | `env_local` | 1, 3, 5 | `.env.local` exists, parses, is newer than `orgConfig.secretsUpdatedAt`, and is gitignored. |
| 10 | `claude_trust` | 5, 7 | the profile's `.claude.json` records `projects[<checkout>].hasTrustDialogAccepted == true`, under both the literal and the resolved path. |
| 11 | `claude_statusline` | — | `~/.riabuild/claude-statusline.js` is byte-identical to the copy compiled into this binary. Comparing contents rather than existence is what makes a script that changes in a release repair itself, so `version()` never has to move. |

Notes on specific tasks:

**4 — `toolchain`.** riabuild downloads the official Node tarball, verifies it against
the published `SHASUMS256.txt`, and extracts it to `~/.riabuild/node/<version>/`. pnpm is
installed as a standalone binary at the version named by the repo's `packageManager`
field. No nvm, no corepack.

*Why not nvm:* nvm is a bash function, not a binary. Rust cannot drive it without
spawning a login shell, it does not work in fish, and sourcing it costs every shell
start 200 ms to 1 s. *Why not corepack:* it was removed from Node.js 25+ distributions.
Owning the tarball is roughly 80 lines of Rust and removes an entire class of
works-in-my-shell failures.

**6 — `repo_status`.** `git pull` on every launch fails loudly on dirty trees, detached
HEAD, and conflicts. Startup is the worst possible moment for that. riabuild reports
drift and lets the developer decide.

**7 and 8 — Claude Code profiles.** Task 7 creates `~/.riabuild/claude/<uuid>/` if no
profile exists. Task 8 caches the org settings JSON. **Neither merges anything into a
developer's `settings.json`.** Instead, the `c` launcher injects org policy at launch:

```sh
CLAUDE_CONFIG_DIR=~/.riabuild/claude/<uuid> \
  claude --settings ~/.riabuild/org-settings.json
```

`--settings` layers over the profile's own settings. Org policy is always current,
removals take effect, developer edits survive, and there is no merge code to write. A
recurring deep-merge into `settings.json` cannot express removal, cannot distinguish org
keys from developer keys after the first run, and silently clobbers developer edits.

> `CLAUDE_CONFIG_DIR` is present in the Claude Code binary (verified against 2.1.221) but
> is **not** in the public settings documentation. Undocumented means unpromised: a smoke
> test must pin this behavior so an upstream change surfaces as a test failure rather
> than as broken developer machines.
>
> For policy a developer genuinely cannot bypass, Claude Code's managed settings at
> `/Library/Application Support/ClaudeCode/managed-settings.json` take highest precedence.
> That is a deliberate escalation requiring sudo, not v1 scope.

The org settings ship the first-run experience the team wants: `theme: "auto"`,
`permissions.defaultMode: "bypassPermissions"`, and `skipDangerousModePermissionPrompt:
true`. The last one is not decoration — Claude Code silently downgrades bypass mode to
default unless the disclaimer has been accepted, so the mode alone produces a developer
who believes permissions are off and gets prompted anyway. All three are read from a
`--settings` file: Claude Code treats it as a trusted source (`flagSettings`), alongside
user and policy settings and unlike repo-controllable project settings.

**10 — `claude_trust`.** The one piece of Claude Code state riabuild cannot express as
settings data. Trust is `projects[<absolute path>].hasTrustDialogAccepted` in
`.claude.json`, and until it is set, the first `c` in a fresh checkout opens a modal and
holds the org's settings back as untrusted. The task read-modify-writes only the
riabuild-owned profile's `.claude.json` — never `~/.claude.json` — preserving every key
it does not own, and swaps the file in atomically because Claude Code may be running
against it. It writes the key under both the literal and the resolved checkout path,
since a symlinked checkout makes those different strings and trust under one is invisible
under the other.

**11 — `claude_statusline`.** The org settings ship a `statusLine` naming
`node ~/.riabuild/claude-statusline.js`, a context-window bar. The two halves are
deliberately split across the trust boundary: the server sends the *pointer*, and the
*script* is compiled into the binary with `include_str!` and installed by this task.
Serving the script body instead would be a one-key remote code execution channel — the
task manifest this design already rejected, wearing a different name. `node` resolves
because `path_with_riabuild` puts riabuild's Node and `~/.riabuild/bin` on `PATH`
together, so the interpreter is present wherever the `c` launcher is.

### Startup update check

`GET /api/v1/org/config` returns `minCliVersion` and `latestCliVersion` — a request
riabuild already makes. Only when a newer version actually exists does it shell out to
`brew upgrade clubria/tap/riabuild`, then `exec` itself with the original arguments and
`RIABUILD_UPDATED=1` set to prevent loops. Running below `minCliVersion` makes the
upgrade mandatory.

Unconditionally running `brew update` on every launch would cost 5–30 s of a full tap
fetch before the developer sees anything.

### The environment shell

After all tasks report satisfied, riabuild spawns `$SHELL` with the environment injected
and a banner:

```
● Clubria environment active — type `exit` to leave
```

PATH becomes `~/.riabuild/bin:~/.riabuild/node/<version>/bin:$PATH`, which supplies
`node`, `pnpm`, and `c`.

Per-shell handling is explicit work, not an implementation detail:

| Shell | Mechanism |
|---|---|
| bash | `--rcfile ~/.riabuild/shell/bash/rc`, which **sources the user's `~/.bashrc` first** |
| zsh | `ZDOTDIR=~/.riabuild/shell/zsh`, whose `.zshrc` **sources the user's real `~/.zshrc` first** |
| fish | generated config sourced via `XDG_CONFIG_HOME` shim |

`bash --rcfile` replaces the user's `.bashrc` rather than adding to it, and zsh has no
`--rcfile` at all. Getting this wrong means every developer silently loses their prompt,
aliases, and history configuration, which reads as *riabuild broke my shell*.

Two mitigations for the known limitation that GUI editors launched outside the subshell
do not inherit the environment:

- riabuild refuses to nest: if `RIABUILD_SHELL=1` is already set, it reports the existing
  session instead of spawning another.
- The banner instructs developers to launch their editor with `code .` **from inside** the
  shell, since GUI applications inherit the environment of the terminal that launched them.

## Error handling

Every task failure prints four things:

1. What was being attempted, in the developer's words rather than the code's
2. The exact command run and its stderr
3. One concrete next action
4. Whether re-running `riabuild` is safe

No failure may leave the machine in a state that a re-run cannot repair. This is a direct
consequence of the idempotency requirement on `apply()`.

## Testing

**Every external process goes through a `CommandRunner` trait.** This is the single
decision that determines whether this codebase is testable. With it, each `check()` is a
pure unit test against canned `gh`, `git`, `node`, and `claude` output. Without it, every
test needs a real machine in a real state, and the suite will be abandoned.

| Layer | Approach |
|---|---|
| Task engine | fake tasks; asserts DAG order, status computation, state persistence, apply-then-recheck. No I/O outside a tempdir. |
| Individual tasks | `check()` against fixture `~/.riabuild` trees and injected command output. |
| Convex functions | `convex-test`. |
| `CLAUDE_CONFIG_DIR` | smoke test pinning the undocumented behavior. |
| End to end | full flow against a local Convex backend and a temporary `HOME`. |

## Platform

macOS for v1, Linux-shaped code. Path resolution and keychain access sit behind traits
from the first commit so Linux support is an addition rather than a rewrite. The Node
tarball target is selected at runtime and already supports both.

## Decisions and open questions

**Decided: stay on `@convex-dev/auth`** with the GitHub provider. Convex documents the
package as beta and points new projects at third-party providers, but migrating to
`@convex-dev/better-auth` is churn we should not pay before there is evidence of actual
instability. Revisit only if beta breakage costs us time.

One open question, not blocking implementation:

- Whether `candidate` scoping in Infisical is fine-grained enough in practice, or whether
  path subsets need to become per-person rather than per-role.

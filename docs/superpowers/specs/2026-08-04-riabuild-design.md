# riabuild — Design

**Date:** 2026-08-04
**Status:** Implemented
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

> **Superseded on 2026-08-07** by
> `2026-08-07-device-code-login-design.md`. The reasoning below assumed the terminal and
> the browser share a machine, which is false over SSH — the CLI bound a port on the
> server while the browser resolved `127.0.0.1` on the laptop. riabuild now polls a device
> code. None of the following describes shipped code; it is kept for the record.

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
`lastUsedAt`. The two sign-in routes are the exception that proves it: `cli/device` and
`cli/token` are how a laptop with no session gets one, so there is nothing to send, and
the version floor is the only gate they can enforce.

The table below is the whole of `convex/http.ts` as it stands, not the five routes this
document originally proposed. `/api/v1` is add-only, so it has only ever grown — device
authorisation replaced the loopback exchange, remote mode added a way for a laptop to sign
a server in and for the dashboard to hand out addresses and issued keys, and ngrok added
one route that mints nothing and merely hands back a token a lead set.

| Endpoint | Purpose |
|---|---|
| `POST /api/v1/cli/device` | start a device authorisation |
| `POST /api/v1/cli/token` | poll a device code, eventually for a session token |
| `POST /api/v1/cli/sessions` | a signed-in laptop mints a session for a server it is provisioning |
| `DELETE /api/v1/cli/sessions/<id>` | revoke a session |
| `GET /api/v1/me` | member profile, role, status |
| `GET /api/v1/org/config` | `repoSlug`, `minCliVersion`, `latestCliVersion`, `secretsUpdatedAt`, `secretEnvironments`, `ngrokAuthTokenUpdatedAt` (plus a frozen `defaultProjectPath`, retired — see below) |
| `GET /api/v1/org/claude-settings` | org Claude Code settings JSON + `updatedAt` |
| `GET /api/v1/org/ngrok-token` | the team's ngrok authtoken, fetched by the `ngrok` shim on every invocation and never written to a filesystem |
| `GET /api/v1/remotes/shared` | the addresses of the team's shared servers — hostname, port, username, and nothing else |
| `GET /api/v1/issued-keys` | the SSH private keys a lead issued to this developer, held only in an `ssh-agent` riabuild owns |
| `POST /api/v1/secrets/token` | short-lived Infisical access token |

The three routes that hand out a live credential — `org/ngrok-token`, `issued-keys` and
`secrets/token` — each write an `auditLog` row, because a team-held secret with no record
of who fetched it is the attribution this design already accepted losing, lost twice.
`org/config` carries `ngrokAuthTokenUpdatedAt` rather than the token for the same reason:
it lets the CLI say "your lead has not set one" without brokering anything.

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
| `/cli` | the device-code approval screen |
| `/__ui` | the component gallery, dev builds only — a 404 anywhere else |

There are three paths and no router library: `src/app/route.ts` is one `route(pathname)`
function returning a discriminated union, which is what makes a 404 an outcome the tests
can name rather than a blank page. The approval screen is `/cli`, not the
`/cli/authorize` the loopback section above proposed; that section is superseded, and so
was its URL.

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
  bin/  pnpm  claude  claude-1…N   standalone pnpm, one launcher per account
  claude/<uuid>/          CLAUDE_CONFIG_DIR account directories
  shell/zsh/.zshrc        generated rcfiles
  logs/riabuild.log
```

The riabuild session token lives in the Keychain, never in this tree.

### Crate layout

> **Superseded on 2026-08-12** by
> [`2026-08-12-cargo-workspace-design.md`](2026-08-12-cargo-workspace-design.md). The flat
> `riabuild-cli/src/` this section used to list does not exist: `riabuild-cli/` is now a
> virtual cargo workspace, every file it named moved into one of thirteen crates under
> `crates/`, and the binary is `crates/cli/`. The current crate table — and the dependency
> order that turns what used to be prose about module boundaries into things that fail to
> compile — lives in `riabuild-cli/CLAUDE.md` under *Layout*. It is deliberately not
> copied here, because a second copy is the one that goes stale.

Files stay small and single-purpose. When one grows past roughly 300 lines, that is a
signal it is doing too much. That rule survives the workspace unchanged; what changed is
that a file now sits in a crate that cannot reach the crates below it.

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

Eighteen tasks, listed in the order `riabuild_tasks::registry()` declares them — which is
the order they are read in, not the order they run in, because the engine sorts by
`depends_on`. Dependencies are given by task id rather than by number: the id is what the
code names, the numbers are only the ones the task modules' own headers assert, and three
tasks added after this document was written never took one.

| # | Task | Depends on | Check |
|---|---|---|---|
| 1 | `login` | — | a live session: `ctx.member` was populated by asking `/api/v1/me` at startup, and `status == active`. A suspended account is a hard stop rather than a check failure, because signing in again would succeed and change nothing. Refreshes proactively when the session expires within 7 days. |
| 2 | `github_cli` | — | the `gh` **riabuild owns** exists under `~/.riabuild`, reports a usable version, `gh auth status` exits 0, and `/user/memberships/orgs/Clubria` reports an active membership. The capability is tested, never the scope string: GitHub accepts five scopes there and folds `read:org` into `admin:org`. |
| 15 | `git_credentials` | `github_cli` | git's effective credential helper for `https://github.com` delegates to the `gh` riabuild owns, by absolute path. The sign-in path already runs `gh auth setup-git`, but only when riabuild performs the sign-in — a `gh` already signed in (by the developer, by an older riabuild, or by `internal seed-github` on every managed server) satisfies `github_cli` on its first check, so nothing writes the helper and the developer can clone but not push. Matching the *path* rather than "a helper exists" is what rejects a signed-out system `gh` answering for git. |
| 3 | `infisical_cli` | — | the `infisical` riabuild owns exists and reports a usable version. **No token is installed** — credentials are brokered per use. |
| — | `ngrok` | — | the ngrok riabuild owns is installed, and `~/.riabuild/bin/ngrok` is byte-identical to the shim this binary writes. **No authtoken is installed**: the shim fetches the team's from `/api/v1/org/ngrok-token` on every invocation and puts it in one process's environment. Comparing the shim's text is what catches one written by an older riabuild whose own path has since moved. |
| 4 | `toolchain` | `project` | `~/.riabuild/node/<pinned>/bin/node -v` matches the repo's `.nvmrc`; `~/.riabuild/bin/pnpm -v` matches the repo's `packageManager` field. The edge on `project` is not in this document's original table and was added because it has to be: a check that reads files out of the checkout cannot run before the checkout exists. |
| 5 | `project` | `github_cli` | the chosen directory exists, is a git checkout, and its `origin` matches **the repository this run is about** — which the developer picked from the list `gh` says they are authorized to see, defaulting to `orgConfig.repoSlug`. This document's original row named `Clubria/ai-builders-hub` outright; see [`2026-08-18-repository-picker-design.md`](2026-08-18-repository-picker-design.md). |
| 6 | `repo_status` | `project` | **Reports only.** Ahead/behind counts and dirty-tree state. Never pulls. |
| — | `codex_cli` | `toolchain` | the Codex CLI is installed with riabuild's Node and reports a usable version, and all nine `CODEX_HOME` profile directories exist. Declared *ahead* of `claude_accounts` deliberately: nothing about Codex depends on the Claude sign-in, and a task that waits on a browser must not be able to strand one that does not. **Nobody is signed in** — a Codex sign-in is the developer's own OpenAI account. |
| — | `grok_cli` | — | Grok Build is installed from riabuild's own mirror against a committed digest, and all nine `GROK_HOME` profile directories exist. No `toolchain` edge, because it is a static binary and needs no Node. **Nobody is signed in**, for the same reason as Codex. |
| 7 | `claude_accounts` | `toolchain` | at least one account directory exists, account 1 is signed in, `claude --version` ≥ floor. |
| 8 | `org_settings` | `login` | `org-settings.json` is valid JSON and matches the server's `updatedAt`. |
| 10 | `claude_trust` | `claude_accounts`, `project` | *every* account's `.claude.json` records `projects[<checkout>].hasTrustDialogAccepted == true`, under both the literal and the resolved path. |
| 12 | `claude_onboarding` | `claude_accounts` | *every* account's `.claude.json` records `hasCompletedOnboarding == true`. Deliberately not on `project`: unlike trust, nothing here needs a checkout. |
| 14 | `claude_agents_view` | `claude_accounts` | *every* account's `.claude.json` carries the `defaultToAgentsView` key. Presence, not truth — a developer who turned the view off has answered the question, and re-asking it every run is how riabuild would keep overruling them. |
| 9 | `env_local` | `login`, `infisical_cli`, `project` | one `.env.<environment>` per environment in `orgConfig.secretEnvironments` exists, parses, is newer than `orgConfig.secretsUpdatedAt`, and is gitignored. A developer or lead gets `.env.dev` and `.env.staging`; a candidate gets `.env.dev`. The task id is historical — it wrote a single `.env.local` before environments were plural. |
| 11 | `claude_statusline` | — | the status line script is byte-identical to the copy compiled into this binary. Comparing contents rather than existence is what makes a script that changes in a release repair itself, so `version()` never has to move. The path is `Paths::claude_statusline_file`, and it is `~/.riabuild/claude-statusline.js` on a server as well as on a laptop: the shared tools root, not the per-developer namespace, because the org settings name it through a `~` the shell expands to the account's home. A copy in the namespace is a status line whose command silently fails, which is what remote mode had until 2026-08-17 with the task reporting satisfied throughout. |
| 13 | `claude_plugins` | `claude_accounts`, `project` | every marketplace and plugin the *checkout's own* `.claude/settings.json` declares is installed, once per account. Satisfied with zero subprocesses when the checkout declares none. **Nothing here decides what to install**: an org setting naming a marketplace would be the server-driven task manifest under another name. |

> **Superseded for Claude Code.** This document's single-profile model — one
> `~/.riabuild/claude/<uuid>/` reached by a `c` launcher — was replaced by an ordered list
> of up to nine accounts, each with its own launcher. Where the two disagree about Claude
> Code, `2026-08-06-claude-accounts-design.md` is the design; everything else here still
> holds. The paragraphs below are kept as the reasoning that produced the layering approach,
> which the accounts model inherits unchanged.

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

**7 and 8 — Claude Code accounts.** Task 7 creates `~/.riabuild/claude/<uuid>/` if no
account exists. Task 8 caches the org settings JSON. **Neither merges anything into a
developer's `settings.json`.** Instead, each account's launcher injects org policy at
launch:

```sh
CLAUDE_CONFIG_DIR=~/.riabuild/claude/<uuid> \
  claude --settings ~/.riabuild/org-settings.json
```

`--settings` layers over the account's own settings. Org policy is always current,
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
`.claude.json`, and until it is set, the first `claude` in a fresh checkout opens a modal
and holds the org's settings back as untrusted. The task read-modify-writes only the
riabuild-owned accounts' `.claude.json` — never `~/.claude.json` — preserving every key
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
together, so the interpreter is present wherever the account launchers are.

**12 — `claude_onboarding`.** The second piece of Claude Code state that is not settings
data. Signing an account in does not complete Claude Code's first-run setup: `claude auth
login` writes the credentials and leaves `hasCompletedOnboarding` unset, and Claude Code
gates the whole flow on that one key in `.claude.json`. So an account riabuild created,
signed in, and trusted still opened a theme picker on first launch and then asked the
developer to log in — to the account they had just logged into, because the login step is
offered whenever OAuth is *available* rather than when the account is signed out. The task
writes that one boolean into every riabuild-owned account, through the same
read-modify-write as `claude_trust`. It writes no preferences: the theme and the
permission mode are org policy and arrive through the settings file, and answering them
here would put riabuild's answer where the org's cannot reach.

### Startup update check

`GET /api/v1/org/config` returns `minCliVersion` and `latestCliVersion` — a request
riabuild already makes. Only when a newer version actually exists does it shell out to
`brew upgrade clubria/tap/riabuild`, then `exec` itself with the original arguments and
`RIABUILD_UPDATED=1` set to prevent loops. Running below `minCliVersion` makes the
upgrade mandatory.

Unconditionally running `brew update` on every launch would cost 5–30 s of a full tap
fetch before the developer sees anything.

The check runs on **every command**, not only the setup flow it started in. A developer
whose day is `riabuild remote` and `riabuild claude` would otherwise never run the one
command that updates riabuild, and would go on driving servers from a build months old —
with `remote::install::version_for_server` handing each server a *newer* riabuild than
the laptop, which is the pairing that section forbids.

Four commands are excepted, and the rule for them is that riabuild updates on every
command whose stdout is a terminal a human is reading:

| Command | Why not |
|---|---|
| `internal …` | Plumbing the laptop runs on a server over SSH. `ssh` reads `internal askpass`'s stdout *as the password* |
| `channel …` | The clipboard and browser shims — stdout is a payload, and they run on every Ctrl+V |
| `env` | Prints `export` lines for a shell to evaluate |
| `reset` | Runs before the tree is read, because that tree may be why it was asked for |

Which is also why the check cannot precede argv parsing: telling those four apart from
`riabuild status` is what parsing argv is for. A managed server never self-updates
either — no package manager put its binary there.

### The environment shell

After all tasks report satisfied, riabuild spawns `$SHELL` with the environment injected
and a banner:

```
● Clubria environment active — type `exit` to leave
```

PATH becomes `~/.riabuild/bin:~/.riabuild/node/<version>/bin:$PATH`, which supplies
`node`, `pnpm`, and one `claude` launcher per account.

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

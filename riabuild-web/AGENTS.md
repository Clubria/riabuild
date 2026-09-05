# riabuild-web

Convex backend and dashboard at `riabuild.clubria.com`. Serves the onboarding flow, member
administration, the CLI device-approval screen, and the `/api/v1` contract the CLI
depends on.

Root conventions and the PR workflow rule are in `../AGENTS.md`. Design is in
`../docs/superpowers/specs/2026-08-04-riabuild-design.md`.

## Commands

```sh
pnpm dev       # convex dev + vite
pnpm lint      # tsc -b + convex typecheck + eslint, zero warnings tolerated
pnpm test      # vitest — Convex functions
pnpm ui:check  # the whole Playwright suite — see below
pnpm build
```

**`tsc -b` does not typecheck `convex/`, and `pnpm lint` runs a second `tsc` because of
it.** The root `tsconfig.json` references `tsconfig.app.json`, `tsconfig.node.json` and
`tsconfig.e2e.json` — `convex/tsconfig.json` is in none of them, and it is the config
Convex's own deploy typechecks against. So for the whole life of this repository, a type
error in `convex/**` passed every pull-request gate and failed at **`convex deploy`**,
which runs on `main` *after* the merge — in `deploy.yml`, whose failure blocks nothing and
which nobody is watching.

That is not a hypothetical. `orgConfig.test.ts` asserted `settings.model` when the opus
/sonnet split landed, `ClaudeSettings` in `testing.fixtures.ts` never gained the key, and
production Convex stopped updating on 2026-09-02 and stayed stale for two days while every
PR went green. The dashboard and the functions behind it simply stopped moving, and the
next change to notice was one whose whole point was a value the *server* sends.

`typecheck:convex` is that gate moved to where a pull request can fail on it. Do not drop
it back out of `lint` on the grounds that `tsc -b` looks like it already covers the
directory.

`pnpm ui:check` runs **every** Playwright spec, not only the visual one: each `src/dev`
scenario at 380, 768 and 1440, and `smoke.spec.ts` signing in for real against a local
Convex deployment. A test tagged `@viewport-agnostic` runs once, at 768 — running one at
380 and again at 1440 asserts the same thing about the same DOM. The smoke suite skips
itself when no deployment answers; `RIABUILD_E2E_BACKEND=1` turns that skip into a
failure, which is what CI sets after standing one up.

## The dashboard

The UI is a fake TUI: one framed terminal, dark only, built entirely from the component
library in `src/ui/`. Two skills are not optional here:

- **`.agents/skills/riabuild-ui/SKILL.md`** — read before building or changing any UI.
  The visual system, the component library, and the rule that the page handles no
  keystrokes and never advertises a key it does not handle.
- **`.agents/skills/visual-testing/SKILL.md`** — read before claiming any UI works. The
  scenario fixtures and the look-at-every-screenshot loop.

Design: `../docs/superpowers/specs/2026-08-05-tui-console-design.md`.

**Components never call `useQuery`.** `src/data/convexProvider.tsx` is the only file in
`src/` that may *use* `convex/react`; everything else reads `useData()`. That boundary is
what lets `?scenario=<name>` render any data state from fixtures. `src/main.tsx` is the
one other file that imports the module at all — it constructs the `ConvexReactClient` and
chooses between the live provider and the fixtures, which is a decision nothing
downstream can make. Both are named in the check, so it comes back empty:

```sh
grep -rn "convex/react" src/ --include=*.tsx \
  | grep -Ev '^src/(data/convexProvider|main)\.tsx:'   # must be empty
```

Run it from `riabuild-web/`. Anchoring each exception to the start of the line and to a
whole filename is the point: `grep -v data/convexProvider` also excused any future file
whose *contents* happened to mention it.

## Local development

Two deployment environment variables unlock local testing. **Production sets neither**,
and each is checked on the deployment rather than in the client:

| Variable | Effect |
|---|---|
| `RIABUILD_DEV_AUTH=1` | registers an `Anonymous` sign-in provider and makes the GitHub org check return `member` |
| `RIABUILD_DEV_SEED=1` | allows `devSeed:seedForE2e` and `devSeed:seedOrgForDev` |

`RIABUILD_DEV_AUTH` adds a way to authenticate and no way to authorize — role still comes
from `RIABUILD_BOOTSTRAP_LEADS`, and `members.role` gates everything that matters.

## Convex conventions

Follow `convex/_generated/ai/guidelines.md` — it is generated for the pinned Convex
version and is the authority on function syntax, validators, and schema rules. In
particular: always declare `args` and `returns` validators, and use `internalQuery` /
`internalMutation` / `internalAction` for anything not called from a client.

## Invariants

**Tokens are stored hashed.** `cliSessions.tokenHash`, never a raw token. A Convex data
leak must not hand out live sessions.

**Every secret-brokering request re-verifies GitHub org membership.** `members.role` is
never the sole gate. Someone removed from the Clubria GitHub org gets nothing, regardless
of what their Convex row says. Identity lives in GitHub; only authorization lives here.

**riabuild-web never touches the secret payload.** It performs universal-auth login
against Infisical with a per-role machine identity and returns a short-lived access
token. The CLI fetches the actual secrets. Path scoping is enforced by Infisical's RBAC,
not by our code.

**Two responses carry a durable credential, and both are write-only from a browser.**
`GET /api/v1/issued-keys` hands out a private SSH key; `GET /api/v1/org/ngrok-token` hands
out the team's ngrok authtoken. Neither expires on its own, so the GitHub org re-check is
doing the whole job on both, and both audit the fetch rather than the change — for ngrok
that row is the *only* attribution there is, since one account carries the whole team. In
the dashboard a lead sets the ngrok token and gets back its last four characters and a
date: `org.get` returns `publicConfigView`, `org.forApi` returns the value, and they are
two validators on purpose. One validator serving a browser and the CLI is how a secret
reaches a browser by omission instead of by decision.

**Anything that changes access writes an `auditLog` entry.** Role promotion, suspension,
session revocation, delegation.

**Only a browser-approved session may mint another one.** `POST /api/v1/cli/sessions` is
how a laptop signs a *server* in without sending the developer to `/cli` a second time,
and `cliSessions.origin` is what stops the result being a delegation chain: a session
minted that way cannot mint. The check lives in `sessions.delegate`, next to the row it
reads, not in the endpoint — an endpoint that forgot to ask would otherwise reopen it. A
server's token is readable by every co-tenant sharing that Unix account, so one that could
mint would let a leaked credential be replaced indefinitely, including after `riabuild
remote forget` revoked it. Absent `origin` means `device`: every row predating the field
was a browser approval.

**Changing `DEFAULT_CLAUDE_SETTINGS` reaches nobody on its own.** `loadConfig` serves it
only to a deployment with *no* `orgConfig` row, and a row appears the first time anyone
saves org config — including the release workflow publishing a CLI version. On every
deployment past that moment the stored row wins forever, so a new key ships to fresh
deployments and to nowhere else. This has already stranded developers once: `theme`,
`permissions.defaultMode` and `skipDangerousModePermissionPrompt` were added together and
never reached a laptop, and Claude Code started in the default permission mode with
nothing anywhere reporting a problem. Adding a key is therefore two steps — edit the
constant, then run the backfill:

```sh
npx convex run org:backfillClaudeDefaults --prod
```

It is additive only and safe to re-run: a key the org already answered is left alone,
whatever its value.

**"A key the org already answered" includes every array.** `fillMissing` fills keys that
are *absent*, and `permissions.deny` is present on every stored row — so editing the
entries inside that array reaches fresh deployments and nowhere else, and the backfill
above reports "every default is already answered" while doing nothing. This has already
been load-bearing once: riabuild stopped writing a single `.env.local` and started writing
`.env.dev` and `.env.staging`, and `Read(./.env)` is an exact path, so on every existing
deployment the secrets riabuild had just brokered were readable by every Claude Code
account, with the backfill reporting success. Changing an entry *inside* a default array
therefore needs its own named migration:

```sh
npx convex run org:denyEveryDotenvFile --prod
```

`denyEveryDotenvFile` is the model for that shape. It appends and never removes, and it
fires only on an org whose deny list still carries a dotenv entry — an org that removed
them all keeps that choice, because an emptied deny list is a decision and a migration
that puts entries back one element at a time undoes it. Teaching an org a new *filename*
is in scope; re-arguing whether the file should be denied is not.

**The server ships data, never logic.** No endpoint returns anything the CLI will execute,
and since 2026-09-05 it does not even *name* the one program it used to. `statusLine` is a
command Claude Code runs on every render; the CLI now writes its own — pointing at the file
its `claude_statusline` task installed on that machine — and drops whatever this server
sends. So `DEFAULT_CLAUDE_SETTINGS` carries no `statusLine`, `org.update` refuses one, and
the settings screen has no row for it. See `../AGENTS.md` and
`../docs/superpowers/specs/2026-09-05-statusline-in-rust-design.md`.

**`org.update` refuses a settings blob that names a program, and that is a usability lock
rather than a security control.** Ten top-level keys are refused outright — `hooks`,
`mcpServers`, `apiKeyHelper` and the rest of `EXECUTES_A_PROGRAM`; `statusLine` is refused
too, because riabuild installs the status line and writes the key on each machine, so one
stored here would be dropped on every laptop and believed in the dashboard; and `env` is
refused for the names
that decide what a session executes — `NODE_OPTIONS`, `PATH`, `LD_PRELOAD` and the rest of
`INJECTS_A_PROGRAM`, which are the quietest way left to run code once `hooks` is gone.
Say the limit out loud rather than trusting the check: the real gate is the CLI's
`riabuild-cli/crates/tasks/src/org_settings/vetting.rs`, which is the **authority for both
lists**, and it lives there because the laptop treats this server as untrusted — a
compromised deployment, a hand-edited `orgConfig` row, or a proxy between the two would
all sail past anything written here. What the copy buys is a lead being told at *save*
time, instead of the dashboard accepting a blob the whole fleet then refuses and every
developer finding out at once on their next run, from a hard failure naming a key they did
not write. If the two drift, the CLI's list wins and this one is the bug.

It is deliberately only the **first** of the CLI's two tiers. A key riabuild does not
recognise is stripped on the laptop with a note, and refusing one here would make this
server the thing that decides what a lead may write — Claude Code adds settings keys on a
faster clock than riabuild cuts releases, so a lead would be locked out of a new inert
preference until one shipped.

## The `/api/v1` contract

CLI-facing endpoints live in `convex/http.ts` and are versioned. Breaking one strands
every developer on an older Homebrew build until they upgrade — add fields, do not change
or remove them, and bump the version prefix for anything incompatible.

Four endpoints answer differently for different roles, and in every case the dependence
is a **smaller list rather than a refusal** — a candidate gets a 200 with less in it,
never a 403. `GET /api/v1/secrets/scope` is the fourth, and it narrows the same way the
other three do: a candidate is offered the base environment alone, which is the same
narrowing `identityForRole` already makes rather than a second copy of Infisical's RBAC.

`GET /api/v1/remotes/shared` gives a candidate `{ servers: [] }`. The same command is how
they reach the server they set up themselves, and refusing the request would take that
away in order to enforce a rule about servers they were never going to see. It ships an
address and nothing else — the key, the password and the session for one of those servers
stay on the laptop that made them.

`POST /api/v1/secrets/token` and `GET /api/v1/org/config` both carry
`environmentsForRole(member.role)`: `["dev", "staging"]` for a developer or lead,
`["dev"]` for a candidate, which the CLI turns into one `.env.<name>` per entry. The list
appears on **both** because they answer different questions — the broker says what the
credential it just minted may reach, and config says which files the CLI's `check()`
should expect to find. `check()` runs on every `riabuild --check` and must not broker a
token to learn that, since brokering calls Infisical and writes an audit row.

The names are environment names, never filenames. Deciding that `dev` becomes `.env.dev`
is the CLI's job, so a value chosen on the server can never name a location on a laptop.

**A CLI that names a repository is answered from `repoSecretPaths` instead, and both
those lists become fallbacks.** `POST /api/v1/secrets/token` takes an optional `repo`,
and `GET /api/v1/secrets/scope` answers the same question without minting anything —
which is what `check()` reads, for the reason in the paragraph above. A CLI that names no
repository gets exactly what it always got, because that is what the add-only rule means
applied to a field.

Three things about that table are load-bearing, and each of them is a thing a reasonable
change would undo:

- **No row means no environment files.** Not "fall back to `INFISICAL_SECRET_PATH`" —
  that is the whole point of the feature, and a fallback would fill an unmapped
  repository from another repository's folders with nothing said on the terminal.
  `/api/v1/secrets/scope` therefore answers **200 with `configured: false`**, never a
  404: a 404 is what an older deployment returns, and the CLI reads that as "this
  deployment has no table" and uses the org-wide list.
- **The environments come from Infisical, and are never stored.** `discoverEnvironments`
  lists the folders and keeps an environment only when it holds **every** folder the
  repository names — every, not any, because `env_local` exports them as a fold and one
  missing folder fails the whole pull. `infisicalEnvCache` holds the answer for five
  minutes, keyed by the *question* (role plus the ordered folder list), so a lead's edit
  invalidates it without anything having to remember to purge.
- **`repoSecretPaths.updatedAt` is a second kind of staleness beside a rotation.** A
  `.env.dev` filled from the folder a row named yesterday is as wrong as one filled
  before the team rotated, and the file cannot tell the difference. `secretPaths:set`
  therefore returns early when the list is unchanged rather than touching `updatedAt` —
  a lead pressing save on a row they did not edit must not restage the whole team's
  files.

The order of `secretPaths` is dotenv's own contract: a key two folders both hold takes
the value of the folder named **later**. So the validator drops a duplicate keeping its
*last* mention, and re-ordering a list is a change even though the set is the same.

Design: `../docs/superpowers/specs/2026-09-04-per-repository-secret-paths-design.md`.

Read `.agents/skills/riabuild-api/SKILL.md` before adding an endpoint. It covers session
authentication, org re-verification, audit logging, and the error shape the CLI expects.

## Bootstrap

`RIABUILD_BOOTSTRAP_LEADS` is a Convex environment variable listing GitHub logins granted
the `lead` role on first sign-in. Without it nobody can promote anybody and the dashboard
is permanently inert.

## Auth

`@convex-dev/auth` with the GitHub provider only — no passwords. Convex documents this
package as beta and points new projects at third-party providers; if its instability
starts costing time, `@convex-dev/better-auth` is the migration target. Requires the
`read:org` scope so membership checks work.

**Sign-in is served from this origin, and that is load-bearing rather than tidy.**
`functions/api/auth/[[path]].ts` is a Cloudflare Pages Function proxying `/api/auth/*` to
the Convex deployment, and `CUSTOM_AUTH_SITE_URL` is what makes the library name
`riabuild.clubria.com` in both legs of the round trip. The reason is in
`functions/_proxy.ts` and the runbook is in `../docs/deploying.md` §2; the short version is
that the OAuth `state` and PKCE cookies used to belong to `convex.site`, a third-party
domain nobody renders, which is what browser tracking prevention is built to strip — and
Safari's decision to strip it lives per browser profile, outside cookies and outside local
storage, so it cannot be cleared and a second profile disagrees forever.

Three things here are one mistake seen from different sides, and each looks harmless:

- **Widening `public/_routes.json`** past `/api/auth/*`, or widening the `_redirects`
  catch-all. They partition the same space: Pages invokes a Function only for the paths
  `_routes.json` lists, and the SPA fallback gets the rest.
- **Proxying `/api/v1` too.** The CLI calls it directly, holds no cookies and is not a
  browser. None of this applies to it, and a second hop under every provisioning run buys
  nothing.
- **Unsetting `CUSTOM_AUTH_SITE_URL`** — which reintroduces the original bug exactly.
  The proxy refuses that with a 500 naming the variable rather than letting it bounce
  developers silently, and `functions/_proxy.test.ts` is what keeps the refusal working.

**This whole area fails silently by default, which is why it has been diagnosed from
scratch three times.** A failed OAuth callback answers a bare redirect to `SITE_URL` with
no `code` — byte-identical to somebody typing the address in. `functions/_proxy.ts` marks
that redirect, `src/lib/authFailure.ts` reads the mark, and `SignIn` renders it; the
`signin-round-trip-failed` scenario is what it looks like. If you find yourself changing
any of those, keep the property rather than the code: a sign-in that fails must say so.

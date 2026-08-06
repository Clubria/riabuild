# riabuild-web

Convex backend and dashboard at `riabuild.clubria.com`. Serves the onboarding flow, member
administration, the CLI login callback, and the `/api/v1` contract the CLI depends on.

Root conventions and the PR workflow rule are in `../CLAUDE.md`. Design is in
`../docs/superpowers/specs/2026-08-04-riabuild-design.md`.

## Commands

```sh
pnpm dev       # convex dev + vite
pnpm lint      # tsc -b + eslint, zero warnings tolerated
pnpm test      # vitest — Convex functions
pnpm ui:check  # Playwright visual suite, all scenarios × 3 viewports
pnpm build
```

## The dashboard

The UI is a fake TUI: one framed terminal, dark only, built entirely from the component
library in `src/ui/`. Two skills are not optional here:

- **`.claude/skills/riabuild-ui/SKILL.md`** — read before building or changing any UI.
  The visual system, the component library, and the rule that the page handles no
  keystrokes and never advertises a key it does not handle.
- **`.claude/skills/visual-testing/SKILL.md`** — read before claiming any UI works. The
  scenario fixtures and the look-at-every-screenshot loop.

Design: `../docs/superpowers/specs/2026-08-05-tui-console-design.md`.

**Components never call `useQuery`.** `src/data/convexProvider.tsx` is the only file in
`src/` that may import from `convex/react`; everything else reads `useData()`. That
boundary is what lets `?scenario=<name>` render any data state from fixtures. Check it:

```sh
grep -rn "convex/react" src/ --include=*.tsx | grep -v data/convexProvider   # must be empty
```

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

**Anything that changes access writes an `auditLog` entry.** Role promotion, suspension,
session revocation.

**The server ships data, never logic.** No endpoint returns anything the CLI will
execute. The Claude settings may name a program riabuild installs from its own binary —
`statusLine` names `node ~/.riabuild/claude-statusline.js` — but never carry the program.
See `../CLAUDE.md`.

## The `/api/v1` contract

CLI-facing endpoints live in `convex/http.ts` and are versioned. Breaking one strands
every developer on an older Homebrew build until they upgrade — add fields, do not change
or remove them, and bump the version prefix for anything incompatible.

Read `.claude/skills/riabuild-api/SKILL.md` before adding an endpoint. It covers session
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

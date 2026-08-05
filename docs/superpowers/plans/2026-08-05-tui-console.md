# riabuild console Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the `riabuild-web` UI as a single framed fake-terminal, on a reusable component library, verifiable by an exhaustive Playwright visual loop.

**Architecture:** A `src/ui/` component library supplies every visual primitive. A `DataProvider` context sits between Convex and the presenter components so any data state can be rendered from fixtures. Routing becomes an explicit `route()` function, which makes a 404 possible. Dev-only scenario fixtures plus a dev-only Convex `Anonymous` provider give a test harness access to every page.

**Tech Stack:** React 19, Vite 8, Tailwind 4 (`@theme` tokens), Convex 1.42, `@convex-dev/auth` 0.0.91, Playwright, axe-core, Vitest.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-05-tui-console-design.md`. Read it before any task.
- **The page never handles keystrokes.** No `keydown` listeners for navigation or commands. Every affordance is a `<button>` or `<a>`. Never render a hint for a key we do not handle.
- **Dark only.** No `prefers-color-scheme` branch, no light tokens.
- **No box-drawing characters in the DOM as structure.** Frames are CSS borders; corner glyphs go on pseudo-elements.
- `pnpm lint` is `tsc && eslint --max-warnings 0`. Zero warnings tolerated — this gates every commit.
- Convex functions always declare `args` and `returns` validators (`convex/_generated/ai/guidelines.md`).
- Dev bypasses are gated on **deployment environment variables** (`RIABUILD_DEV_AUTH=1`), never on a client flag. Client-side dev UI is additionally gated on `import.meta.env.DEV`.
- No changes to `/api/v1`, the Rust CLI, or the auth/authorization model.
- Work happens on branch `worktree-feat+tui-ui`; all work lands through a PR with green CI.

**Testing note.** UI has no jsdom/RTL setup and this plan does not add one — the spec's verification mechanism is Playwright against the `/__ui` gallery and `?scenario=` pages. The TDD cycle for a UI task is therefore: add the gallery/scenario entry → screenshot it → observe it is missing or wrong → implement → screenshot → observe it is right. Vitest continues to cover Convex functions.

---

## File Structure

**Created**

| Path | Responsibility |
|---|---|
| `src/ui/tokens.css` | `@theme` design tokens + terminal base styles |
| `src/ui/Screen.tsx` | outer frame: title bar + body + status bar |
| `src/ui/TitleBar.tsx` | wordmark, tab strip, window dots |
| `src/ui/StatusBar.tsx` | pinned bottom line, left/right slots |
| `src/ui/Panel.tsx` | titled box with notched legend |
| `src/ui/Button.tsx` | every action, 3 variants + pending |
| `src/ui/Badge.tsx` | state chip |
| `src/ui/Dot.tsx` | `●` status indicator |
| `src/ui/Alert.tsx` | inline message |
| `src/ui/Empty.tsx` | empty state |
| `src/ui/Loading.tsx` | skeleton line with blinking cursor |
| `src/ui/Field.tsx` | labelled input |
| `src/ui/TextArea.tsx` | labelled multiline |
| `src/ui/Select.tsx` | labelled select |
| `src/ui/Command.tsx` | `$ cmd  [copy]` |
| `src/ui/KeyValue.tsx` | mono definition grid |
| `src/ui/DataTable.tsx` | rows + actions + empty + responsive column drop |
| `src/ui/index.ts` | barrel |
| `src/app/route.ts` | `route(pathname)` → discriminated union |
| `src/app/ErrorBoundary.tsx` | class boundary, "core dumped" panel |
| `src/routes/NotFound.tsx` | 404 |
| `src/data/types.ts` | the data contract shared by real and fixture providers |
| `src/data/DataProvider.tsx` | context + `useData()` |
| `src/data/convexProvider.tsx` | real implementation |
| `src/dev/scenarios.ts` | fixture data, one entry per scenario |
| `src/dev/DevDataProvider.tsx` | fixture implementation, dev builds only |
| `src/routes/Gallery.tsx` | `/__ui` component gallery, dev builds only |
| `e2e/playwright.config.ts` | Playwright config |
| `e2e/helpers.ts` | shared page assertions |
| `e2e/visual.spec.ts` | scenario × viewport sweep |
| `e2e/smoke.spec.ts` | real dev-login walk |
| `.claude/skills/riabuild-ui/SKILL.md` | UI skill |
| `.claude/skills/visual-testing/SKILL.md` | testing skill |

**Modified**

| Path | Change |
|---|---|
| `src/index.css` | drop print aesthetic, import `ui/tokens.css` |
| `src/App.tsx` | route dispatch, providers, error boundary |
| `src/main.tsx` | mount provider tree |
| `src/routes/Dashboard.tsx` | rebuild on library + `useData()` |
| `src/routes/CliAuthorize.tsx` | rebuild on library |
| `src/components/*` | rebuild on library, data via props |
| `src/useOrgMembership.ts` | folded into the data layer |
| `convex/auth.ts` | dev-only `Anonymous` provider |
| `convex/github.ts` | dev-only membership bypass |
| `convex/devSeed.ts` | seed a full fixture org |
| `package.json` | Playwright deps, `ui:check` / `e2e` scripts |
| `.github/workflows/ci.yml` | Playwright job |
| `riabuild-web/CLAUDE.md` | point at the two new skills |

---

## Task 1: Design tokens and the terminal frame

**Files:**
- Create: `src/ui/tokens.css`, `src/ui/Screen.tsx`, `src/ui/TitleBar.tsx`, `src/ui/StatusBar.tsx`, `src/ui/index.ts`
- Modify: `src/index.css`

**Interfaces:**
- Consumes: nothing
- Produces:
  ```ts
  type Tab = { id: string; label: string; href: string };
  function Screen(p: {
    title: string; tabs?: Tab[]; activeTab?: string;
    statusLeft?: ReactNode; statusRight?: ReactNode; children: ReactNode;
  }): JSX.Element;
  function TitleBar(p: { title: string; tabs?: Tab[]; active?: string }): JSX.Element;
  function StatusBar(p: { left?: ReactNode; right?: ReactNode }): JSX.Element;
  ```

- [ ] **Step 1: Write `src/ui/tokens.css`**

Tailwind 4 `@theme` block. Exact token names (used verbatim by every later task):

```css
@theme {
  --color-bg: #0b0e0c;
  --color-bg-sunk: #070907;
  --color-bg-raised: #121613;
  --color-fg: #d6ded4;
  --color-fg-dim: #8b968a;
  --color-fg-faint: #5a655a;
  --color-accent: #58d6c8;
  --color-ok: #7fd67f;
  --color-warn: #e0b155;
  --color-danger: #ff6a5e;
  --color-rule: #27302a;
  --font-mono: "JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
```

Plus: `:root { color-scheme: dark }`, body forced to `--font-mono`, a `.cursor` blinking block keyframe, and `@media (prefers-reduced-motion: reduce) { .cursor { animation: none } }`.

Delete the Archivo/Newsreader Google Fonts import; keep only JetBrains Mono.

- [ ] **Step 2: Write `Screen`, `TitleBar`, `StatusBar`**

`Screen` is `grid-rows-[auto_1fr_auto] min-h-dvh`, body scrolls, status bar pinned. Tabs render as `<a>` elements. The status bar shows state only — never a keybinding hint.

- [ ] **Step 3: Wire `App.tsx` to render existing content inside `Screen`**

Temporary: keep all current page content, just wrap it. This isolates "does the frame work" from "does the content work".

- [ ] **Step 4: Verify**

Run: `pnpm lint && pnpm build`
Expected: clean. Then `pnpm vite --port 5199` and load `/` — a framed dark terminal with the old content inside.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src/ui riabuild-web/src/index.css riabuild-web/src/App.tsx
git commit -m "Frame the dashboard as a terminal"
```

---

## Task 2: Core display primitives

**Files:**
- Create: `src/ui/Panel.tsx`, `Button.tsx`, `Badge.tsx`, `Dot.tsx`, `Alert.tsx`, `Empty.tsx`, `Loading.tsx`
- Modify: `src/ui/index.ts`

**Interfaces:**
- Consumes: tokens from Task 1
- Produces:
  ```ts
  type Tone = "default" | "accent" | "ok" | "warn" | "danger" | "muted";

  function Panel(p: {
    title?: string; index?: string; subtitle?: ReactNode; tone?: Tone;
    actions?: ReactNode; dense?: boolean; children: ReactNode;
  }): JSX.Element;

  function Button(p: {
    children: ReactNode; variant?: "primary" | "quiet" | "danger";
    onClick?: () => void; href?: string; type?: "button" | "submit";
    disabled?: boolean; pending?: boolean; pendingLabel?: string;
    title?: string; "aria-label"?: string;
  }): JSX.Element;

  function Badge(p: { tone?: Tone; children: ReactNode }): JSX.Element;
  function Dot(p: { tone?: Tone; label: string }): JSX.Element;
  function Alert(p: { tone?: Tone; title: string; children?: ReactNode }): JSX.Element;
  function Empty(p: { glyph?: string; title: string; children?: ReactNode; action?: ReactNode }): JSX.Element;
  function Loading(p: { label?: string }): JSX.Element;
  ```

- [ ] **Step 1: Implement `Panel` with a notched title**

Border on the container; title absolutely positioned at `top-0 -translate-y-1/2 left-4`, `bg-bg px-2`, so it sits *on* the rule. Corner glyphs via `::before`/`::after` are optional polish — the border alone reads correctly. `index` renders as a dim prefix (`01 ·`).

- [ ] **Step 2: Implement `Button`**

`pending` shows a spinner cycling `| / - \` on a 100ms interval and forces `disabled`. `href` renders an `<a>` with identical styling. Focus ring is a 1px outline in `--color-accent` with 2px offset — never `outline: none`.

- [ ] **Step 3: Implement the remaining five**

`Loading` renders `label` plus a blinking `█`. `Empty` centres a dim glyph over a title.

- [ ] **Step 4: Verify**

Run: `pnpm lint`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src/ui
git commit -m "Add the console's display primitives"
```

---

## Task 3: Form and value primitives

**Files:**
- Create: `src/ui/Field.tsx`, `TextArea.tsx`, `Select.tsx`, `Command.tsx`, `KeyValue.tsx`
- Modify: `src/ui/index.ts`

**Interfaces:**
- Consumes: `Tone`, `Button` from Task 2
- Produces:
  ```ts
  function Field(p: {
    label: string; value: string; onChange: (v: string) => void;
    type?: string; hint?: ReactNode; error?: string | null;
    placeholder?: string; autoComplete?: string; disabled?: boolean;
    required?: boolean; spellCheck?: boolean;
  }): JSX.Element;

  function TextArea(p: { /* as Field */ rows?: number }): JSX.Element;

  function Select(p: {
    label: string; value: string; disabled?: boolean;
    options: { value: string; label: string }[];
    onChange: (v: string) => void;
  }): JSX.Element;

  function Command(p: { command: string; prompt?: string; multiline?: boolean }): JSX.Element;

  function KeyValue(p: {
    rows: { label: string; value: ReactNode; tone?: Tone }[];
  }): JSX.Element;
  ```

- [ ] **Step 1: Implement `Field`, `TextArea`, `Select`**

Each generates an id via `useId()` and wires `htmlFor`, plus `aria-describedby` to the hint and `aria-invalid` + error text when `error` is set. Inputs render as `[ value________ ]`-style wells: inset background, 1px rule, bracket glyphs on pseudo-elements.

- [ ] **Step 2: Implement `Command`**

Keeps the existing clipboard behaviour from `primitives.tsx:76-100`, but the copy control is a `Button variant="quiet"`. Long commands scroll horizontally inside the well — they never widen the page.

- [ ] **Step 3: Implement `KeyValue`**

`<dl>` with `grid-cols-[minmax(0,7rem)_minmax(0,1fr)]`. Values get `break-words` so a 200-char unbroken string wraps instead of blowing out the grid.

- [ ] **Step 4: Verify**

Run: `pnpm lint`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src/ui
git commit -m "Add the console's form and value primitives"
```

---

## Task 4: DataTable

**Files:**
- Create: `src/ui/DataTable.tsx`
- Modify: `src/ui/index.ts`

**Interfaces:**
- Consumes: `Empty`, `Tone` from Task 2
- Produces:
  ```ts
  type Column<T> = {
    key: string;
    header: string;
    width?: string;                       // CSS grid track, e.g. "minmax(0,1fr)"
    align?: "start" | "end";
    priority?: "always" | "wide";         // "wide" columns hide below 640px
    render: (row: T) => ReactNode;
  };

  function DataTable<T>(p: {
    columns: Column<T>[];
    rows: T[];
    rowKey: (row: T) => string;
    renderActions?: (row: T) => ReactNode;
    empty: ReactNode;
    caption: string;                      // visually hidden, for screen readers
  }): JSX.Element;
  ```

- [ ] **Step 1: Implement it**

Real `<table>` for semantics, `display: grid` on rows for layout control. `priority: "wide"` columns get `hidden sm:table-cell`. Zero rows renders `empty`. The wrapper is `overflow-x-auto` so a wide table scrolls itself rather than the document.

- [ ] **Step 2: Verify**

Run: `pnpm lint`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add riabuild-web/src/ui
git commit -m "Add DataTable, collapsing three hand-rolled lists"
```

---

## Task 5: Routing, 404 and the error boundary

**Files:**
- Create: `src/app/route.ts`, `src/app/ErrorBoundary.tsx`, `src/routes/NotFound.tsx`
- Modify: `src/App.tsx`

**Interfaces:**
- Consumes: `Panel`, `Button`, `Alert` from Task 2
- Produces:
  ```ts
  type Route =
    | { kind: "dashboard" }
    | { kind: "authorize" }
    | { kind: "gallery" }
    | { kind: "notFound"; path: string };
  function route(pathname: string): Route;

  function ErrorBoundary(p: { children: ReactNode; label?: string }): JSX.Element;
  ```

- [ ] **Step 1: Write `route()`**

Trailing slashes normalised. `/__ui` returns `{kind:"gallery"}` only when `import.meta.env.DEV`, otherwise `notFound` — so the gallery 404s in production even if someone guesses the path.

- [ ] **Step 2: Write `ErrorBoundary`**

Class component with `getDerivedStateFromError` + `componentDidCatch`. Renders a `danger`-toned `Panel` titled `core dumped`. The error message and component stack are shown **only** under `import.meta.env.DEV`; production shows a fixed line. A `[ reload ]` `Button` calls `window.location.reload()`.

- [ ] **Step 3: Write `NotFound`**

`command not found: <path>` with the path escaped as text, plus `[ cd / ]` linking home.

- [ ] **Step 4: Verify**

Run: `pnpm lint`, then load `/nope` in dev.
Expected: the 404 screen inside the terminal frame.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src/app riabuild-web/src/routes/NotFound.tsx riabuild-web/src/App.tsx
git commit -m "Add explicit routing, a 404 screen and an error boundary"
```

---

## Task 6: The data layer

**Files:**
- Create: `src/data/types.ts`, `src/data/DataProvider.tsx`, `src/data/convexProvider.tsx`
- Modify: every file in `src/components/`, `src/routes/Dashboard.tsx`, `src/App.tsx`, `src/main.tsx`
- Delete: `src/useOrgMembership.ts` (folded in)

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces:
  ```ts
  type Loadable<T> = { state: "loading" } | { state: "ready"; value: T } | { state: "error"; message: string };

  type Member = {
    _id: string; githubLogin: string; firstName: string; lastName: string;
    email: string; role: "candidate" | "developer" | "lead";
    status: "active" | "suspended";
  };
  type Session = {
    _id: string; deviceLabel: string; cliVersion: string;
    lastUsedAt: number; expiresAt: number; revokedAt: number | null;
  };
  type AuditEntry = {
    _id: string; at: number; action: string;
    actorLogin: string | null; subjectLogin: string | null;
    meta: Record<string, string>;
  };
  type OrgConfig = {
    repoSlug: string; defaultProjectPath: string; claudeSettings: string;
    minCliVersion: string; latestCliVersion: string; secretsUpdatedAt: number;
  };
  type Membership = { org: string; status: "member" | "not_member" | "unavailable" | "signed_out" | "checking" };

  type Data = {
    auth: "loading" | "signed-in" | "signed-out";
    viewer: Loadable<Member | null>;
    membership: Membership;
    sessions: Loadable<Session[]>;
    members: Loadable<Member[]>;
    auditLog: Loadable<AuditEntry[]>;
    orgConfig: Loadable<OrgConfig>;
    now: number;
    updateProfile(p: { firstName: string; lastName: string; email: string }): Promise<void>;
    setRole(p: { memberId: string; role: Member["role"] }): Promise<void>;
    setStatus(p: { memberId: string; status: Member["status"] }): Promise<void>;
    revokeSession(p: { sessionId: string }): Promise<void>;
    updateOrg(p: Partial<OrgConfig> & { markSecretsRotated?: boolean }): Promise<void>;
    signIn(): Promise<void>;
    signOut(): Promise<void>;
    authorizeCli(p: { challenge: string; deviceLabel: string; cliVersion: string }): Promise<{ code: string }>;
  };

  function useData(): Data;
  ```

- [ ] **Step 1: Write `types.ts` and the context**

`useData()` throws a clear error when used outside a provider.

- [ ] **Step 2: Write `convexProvider.tsx`**

Every current `useQuery`/`useMutation`/`useAction` call in the app moves here — this is the *only* file that imports from `convex/react`. Lead-only queries (`members.list`, `members.auditLog`) are skipped with `"skip"` when the viewer is not a lead, so a developer's console shows no failed queries. `useOrgMembership`'s effect logic moves in verbatim. `now` ticks from the existing `useNow()` in `src/lib/time.ts`.

- [ ] **Step 3: Convert components to props/`useData()`**

Leaf presenters (`Sessions`, `Members`, `Profile`, `AuditLog`, `OrgSettings`) take data as props. The route components read `useData()` and pass down. No visual change in this task.

- [ ] **Step 4: Verify**

Run: `pnpm lint && pnpm build`, then `grep -rn "convex/react" src/ --include=*.tsx | grep -v data/convexProvider`
Expected: lint clean; grep returns nothing.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src
git commit -m "Put a data layer between Convex and the components"
```

---

## Task 7: Rebuild every page on the library

**Files:**
- Modify: `src/routes/Dashboard.tsx`, `src/routes/CliAuthorize.tsx`, `src/components/SignIn.tsx`, `Profile.tsx`, `Sessions.tsx`, `LeadPanel.tsx`
- Delete: `src/components/primitives.tsx`

**Interfaces:**
- Consumes: all of `src/ui`, `useData()` from Task 6
- Produces: no new exports

- [ ] **Step 1: Rebuild the dashboard**

Manifest becomes a `KeyValue`-style boot log. Steps become `Panel`s with `index`. `Sessions`, `Members`, `AuditLog` become `DataTable`s. Lead panels are individually wrapped in `ErrorBoundary`.

- [ ] **Step 2: Rebuild `CliAuthorize` and `SignIn`**

Device details become a `KeyValue`. `readParams` validation logic (`CliAuthorize.tsx:133-149`) is unchanged — it is a security boundary, not styling.

- [ ] **Step 3: Delete `primitives.tsx`**

Every consumer now imports from `src/ui`.

- [ ] **Step 4: Verify**

Run: `pnpm lint && pnpm build`
Expected: clean, and no import of `components/primitives` remains.

- [ ] **Step 5: Commit**

```bash
git add -A riabuild-web/src
git commit -m "Rebuild every page on the console component library"
```

---

## Task 8: Scenario fixtures and the gallery

**Files:**
- Create: `src/dev/scenarios.ts`, `src/dev/DevDataProvider.tsx`, `src/routes/Gallery.tsx`
- Modify: `src/main.tsx`

**Interfaces:**
- Consumes: `Data` from Task 6, all of `src/ui`
- Produces:
  ```ts
  const SCENARIOS: Record<string, () => Data>;
  function scenarioFromLocation(): string | null;   // reads ?scenario=, DEV only
  ```

- [ ] **Step 1: Write the fixtures**

One entry per scenario named in the spec: `loading`, `signed-out`, `candidate`, `developer`, `lead`, `suspended`, `not-member`, `org-unavailable`, `sessions-empty`, `sessions-one`, `sessions-many`, `session-expired`, `session-revoked`, `audit-empty`, `audit-full`, `mutation-error`, `overflow`, `authorize`, `authorize-bad-params`, `authorize-done`, `authorize-error`, `boom`.

All timestamps are fixed constants relative to a frozen `now` (`1785000000000`) so screenshots are deterministic — never `Date.now()`.

`overflow` is adversarial: a 300-character device label, a 200-character unbroken string, emoji, CJK, RTL Arabic, and `""` in every optional field.

`mutation-error` returns mutations that reject with a realistic Convex error string. `boom` throws during render to exercise the error boundary.

- [ ] **Step 2: Write `DevDataProvider`**

Selected in `main.tsx` only when `import.meta.env.DEV && scenarioFromLocation() !== null`; otherwise the real provider is used. The dev module is behind a dynamic import so it is absent from production bundles.

- [ ] **Step 3: Write the gallery**

`/__ui` renders every component from `src/ui` in every prop combination, grouped by component, each labelled with the props being shown.

- [ ] **Step 4: Verify**

Run: `pnpm build && grep -rl "300-character\|scenarios" dist/assets/*.js`
Expected: no match — fixtures are not in the production bundle.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src
git commit -m "Add scenario fixtures and a component gallery"
```

---

## Task 9: Dev sign-in on the backend

**Files:**
- Modify: `convex/auth.ts`, `convex/github.ts`, `convex/devSeed.ts`

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces: no client-visible API change

- [ ] **Step 1: Gate an `Anonymous` provider**

```ts
import Anonymous from "@convex-dev/auth/providers/Anonymous";
const devProviders = process.env.RIABUILD_DEV_AUTH === "1" ? [Anonymous] : [];
```

- [ ] **Step 2: Gate the membership bypass**

In `github.viewerOrgMembership`, return `{ org: "Clubria", status: "member" }` when `process.env.RIABUILD_DEV_AUTH === "1"`, before any GitHub call.

- [ ] **Step 3: Extend `devSeed`**

Seed a lead, a developer, a candidate and a suspended member, plus active, expired and revoked sessions. Keep the existing `RIABUILD_DEV_SEED=1` gate and keep storing tokens hashed.

- [ ] **Step 4: Verify**

Run: `pnpm test`
Expected: existing Convex tests still pass. Then confirm the production path: with neither env var set, `auth.ts` exposes GitHub only.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/convex
git commit -m "Add an env-gated dev sign-in for local testing"
```

---

## Task 10: Playwright

**Files:**
- Create: `e2e/playwright.config.ts`, `e2e/helpers.ts`, `e2e/visual.spec.ts`, `e2e/smoke.spec.ts`
- Modify: `package.json`

**Interfaces:**
- Consumes: scenarios from Task 8
- Produces:
  ```ts
  async function checkPage(page: Page, name: string): Promise<void>;
  ```

- [ ] **Step 1: Install and configure**

```bash
pnpm add -D @playwright/test @axe-core/playwright
```

Config starts `vite` as a `webServer`, three viewport projects (380×800, 768×1024, 1440×900), `chromium` and `webkit`, screenshots to `e2e/__screens__/`.

- [ ] **Step 2: Write `checkPage`**

Asserts, at the current viewport: `document.scrollWidth <= clientWidth`; zero console errors and zero unhandled rejections; every `button, a, input, select, textarea` is tabbable and shows a non-`none` outline when focused; `axe-core` reports no violations; no element is clipped by an ancestor's `overflow: hidden`. Then writes a screenshot named `<scenario>-<viewport>.png`.

- [ ] **Step 3: Write the visual sweep**

Every scenario × every viewport, plus `/__ui` and `/nope`.

- [ ] **Step 4: Write the smoke suite**

Dev sign-in against a local Convex backend, walking `/`, `/cli/authorize?…` with valid params, and `/nope`. Skipped when no local backend is reachable, so the visual suite is never blocked by backend availability.

- [ ] **Step 5: Add scripts**

```json
"e2e": "playwright test -c e2e/playwright.config.ts",
"ui:check": "playwright test -c e2e/playwright.config.ts visual.spec.ts"
```

- [ ] **Step 6: Verify**

Run: `pnpm ui:check`
Expected: it runs to completion and writes one screenshot per scenario × viewport. Failures here are expected at this point — they are Task 11's input.

- [ ] **Step 7: Commit**

```bash
git add riabuild-web/e2e riabuild-web/package.json
git commit -m "Add the Playwright visual and smoke suites"
```

---

## Task 11: The loop

**Files:** whatever the screenshots indict.

- [ ] **Step 1: Run `pnpm ui:check`**
- [ ] **Step 2: Open every screenshot and look at it**

Every scenario, every viewport. Not a sample.

- [ ] **Step 3: Fix what is wrong**

Clipping, overlap, wrapping, unreadable contrast, a broken frame, a control that vanishes at 380px, an empty state that says nothing useful.

- [ ] **Step 4: Repeat until a full pass produces no fixes**
- [ ] **Step 5: Commit each round**

```bash
git commit -m "Fix <specific thing> at <viewport>"
```

---

## Task 12: Skills

**Files:**
- Create: `.claude/skills/riabuild-ui/SKILL.md`, `.claude/skills/visual-testing/SKILL.md`
- Modify: `riabuild-web/CLAUDE.md`

- [ ] **Step 1: Write `riabuild-ui`**

Frontmatter `description` triggers on building/changing UI in `riabuild-web`. Body: the visual system, the component inventory table, the use/extend/generalize rules, the no-keystrokes policy stated as a hard rule with its rationale, and the requirement that any new state gets a scenario.

- [ ] **Step 2: Write `visual-testing`**

Frontmatter triggers on verifying or claiming a UI works. Body: the suites, the scenario system, the loop discipline, and the edge-case checklist — empty, one, many, huge, unicode, unbroken string, error, loading, disabled, narrow, 200% zoom.

- [ ] **Step 3: Cross-link from `riabuild-web/CLAUDE.md`**

- [ ] **Step 4: Verify**

Both files have valid frontmatter with `name` and `description`, and the descriptions do not collide with the existing `frontend-design` plugin skill.

- [ ] **Step 5: Commit**

```bash
git add .claude/skills riabuild-web/CLAUDE.md
git commit -m "Add the riabuild-ui and visual-testing skills"
```

---

## Task 13: CI and the pull request

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add a Playwright job**

Installs deps, runs `npx playwright install --with-deps chromium`, runs the visual suite for its structural assertions, uploads `e2e/__screens__/` as an artifact. **No pixel baselines** — CI asserts structure only.

- [ ] **Step 2: Open the PR**

```bash
gh pr create --fill
gh pr checks --watch
```

- [ ] **Step 3: Fix CI until green**

Per the root `CLAUDE.md`: work is not finished until PR CI has completed. Fixing a failure is part of this task.

---

## Self-Review

**Spec coverage.** Interaction policy → Task 1 Step 2 + Task 12. Visual system → Tasks 1–4. Frame drawing rule → Task 2 Step 1. Component library → Tasks 2–4, rules → Task 12. Routes/404 → Task 5. Error boundary → Task 5 + Task 7 Step 1. Dev sign-in → Task 9. Scenario fixtures → Task 8. Gallery → Task 8 Step 3. Visual testing + assertions → Task 10. The loop → Task 11. CI → Task 13. Skills → Task 12. No gaps.

**Type consistency.** `Tone` is defined once in Task 2 and reused by Tasks 3, 4, 6. `Data`/`Loadable`/`Member`/`Session`/`AuditEntry`/`OrgConfig`/`Membership` are defined once in Task 6 and consumed by Tasks 7 and 8. `Column<T>`/`DataTable` in Task 4 are consumed by Task 7. `route()`/`Route` in Task 5 are consumed by Task 8's gallery entry. Token names in Task 1 are the ones used throughout.

**Placeholders.** None. Every step names files, exact identifiers, and a runnable verification command.

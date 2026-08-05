# riabuild console — a fake-TUI dashboard

Replaces the "manifest sheet" print aesthetic of `riabuild-web` with a single framed
terminal. Adds the component library, dev-access harness, Playwright loop, 404 page and
error boundary that the reskin needs in order to be verifiable.

Supersedes the visual section of `2026-08-04-riabuild-design.md`. Nothing here changes the
`/api/v1` contract, the auth model, or any architecture rule in `CLAUDE.md`.

## Why

The dashboard is the first thing a new Clubria developer sees, and the product it
introduces is a terminal program. A page that looks like a terminal sets the right
expectation before the first `brew install`.

The reskin is the visible half. The invisible half matters more: today every list
component calls `useQuery` itself, so no UI state can be rendered without a database in
that state. A suspended-and-expired session, a 300-character device label, a failing
mutation — none are reachable in a test. Half this spec exists to make those states
reachable.

## Interaction policy

**It looks like a TUI. It behaves like a website.**

The page never handles keystrokes. There is no `j`/`k` navigation, no `:` command palette,
no modal key mode. Every affordance is a real focusable `<button>` or `<a>`, operable by
mouse, touch, and the browser's own tab order.

The corollary is a hard rule: **never render a hint for a key we do not handle.** No
`^K cmd`, no `q quit`, no `? help` in the status line. A terminal that advertises
keybindings and then ignores them is worse than one that advertises none.

## Visual system

Dark only. `color-scheme: dark` is forced; there is no light variant and no
`prefers-color-scheme` branch. One typeface — JetBrains Mono, already loaded. Archivo and
Newsreader are dropped, and with them the serif/sans/mono split that the old system used
to separate prose from machine values. In a terminal everything is machine output, so the
distinction is carried by colour and case instead.

### Palette

Semantic names survive the retheme; only their values change.

| Token | Role |
|---|---|
| `bg` / `bg-sunk` / `bg-raised` | terminal background, inset wells, lifted panels |
| `fg` / `fg-dim` / `fg-faint` | body text, secondary, tertiary |
| `accent` | structure, links, focus — cyan |
| `ok` | verified, active, success — green |
| `warn` | needs attention — amber |
| `danger` | destructive, failed, suspended — red |
| `rule` | borders |

### Drawing the frame

Box-drawing characters are **not** placed in the DOM. Literal `─│┌┘` requires a fixed
character grid, breaks on any resize or font fallback, and makes a screen reader announce
"box drawings light horizontal" once per cell.

Instead: 1px CSS borders, with corner glyphs on `::before`/`::after` pseudo-elements
(which screen readers skip), and panel titles notched into the top rule — the label is
positioned over the border with the panel background behind it. Visually indistinguishable
from a drawn box at a glance; responsive and accessible.

ASCII glyphs appear only where they are content, not structure: `●` status dots, `▸`
markers, `$` shell prompts, `▓░` meters, `[x]` checkboxes. Each is either inside an
`aria-hidden` span with a real text label alongside, or is itself the accessible label.

### Motion

One blinking block cursor, at the loading state and the hero line. Panels do not animate
in. Everything respects `prefers-reduced-motion: reduce`, which stops the blink.

## Component library — `riabuild-web/src/ui/`

One component per file, re-exported from `src/ui/index.ts`. Consumers import from the
barrel.

| Component | Purpose | Key props |
|---|---|---|
| `Screen` | outer terminal frame | `title`, `tabs`, `status`, `children` |
| `TitleBar` | app name, tab strip, window dots | `title`, `tabs`, `active` |
| `StatusBar` | pinned bottom line | `left`, `right` |
| `Panel` | titled box | `title`, `index`, `subtitle`, `tone`, `actions`, `dense` |
| `Button` | every action | `variant: primary\|quiet\|danger`, `pending`, `disabled`, `href` |
| `Field` | labelled input | `label`, `value`, `onChange`, `type`, `hint`, `error`, `placeholder` |
| `TextArea` | labelled multiline | as `Field` plus `rows` |
| `Select` | labelled select | `label`, `value`, `options`, `onChange` |
| `Badge` | state chip | `tone`, `children` |
| `Dot` | `●` status indicator | `tone`, `label` |
| `Command` | `$ cmd  [copy]` | `command`, `prompt`, `multiline` |
| `KeyValue` | mono definition grid | `rows: {label, value, tone}[]` |
| `DataTable` | list of rows with actions | `columns`, `rows`, `renderActions`, `empty`, `caption` |
| `Alert` | inline message | `tone`, `title`, `children` |
| `Empty` | empty state | `glyph`, `title`, `children`, `action` |
| `Loading` | skeleton line with cursor | `label` |

`DataTable` replaces three hand-rolled list implementations — `Members`, `Sessions` and
`AuditLog` are the same component with different columns. Columns carry width, alignment
and a `priority` used to drop low-value columns at narrow viewports.

The `01 ·` step numbering is a `Panel` prop, not a separate `Step` component.

### Rules for changing the library

- Use it. A page does not hand-roll a button, input, badge or box.
- If a pattern appears a second time, promote it into `src/ui/` rather than copying it.
- If an existing component almost fits, **generalize it with a prop** rather than forking
  a near-duplicate. Props are added when a second real caller needs them, never
  speculatively.
- Every component added or given a new prop gets a `/__ui` gallery entry covering the new
  state, and that state gets visually checked.

## Routes

The no-router approach stays — the product has three destinations — but it is formalised
into a `route(pathname)` function returning a discriminated union. An explicit list of
valid routes is a precondition for having a 404 at all.

| Path | View |
|---|---|
| `/` | dashboard, or sign-in when unauthenticated |
| `/cli/authorize` | device approval |
| `/__ui` | component gallery (dev builds only) |
| anything else | `NotFound` |

`NotFound` renders as `command not found: /whatever` with a `[ cd / ]` button.

## Error boundary

A class `ErrorBoundary` renders a "core dumped" panel: the error message, a component
stack in dev builds only, and a `[ reload ]` button. In production builds the message is
replaced with a generic line so a thrown error cannot leak internals into the page.

It is applied twice:

1. Around the whole app, so a render crash shows a terminal rather than a white page.
2. Around each lead-only panel, so one failing admin query does not blank a developer's
   onboarding steps.

## Dev access

Two mechanisms. Both are inert in production, and both follow the gating pattern
`convex/devSeed.ts` already establishes.

### Dev sign-in

`convex/auth.ts` registers the `Anonymous` provider from `@convex-dev/auth` only when
`RIABUILD_DEV_AUTH=1` is set on the deployment. Production never sets it, so the provider
does not exist there. The client renders a dev sign-in button only under
`import.meta.env.DEV`, so it cannot ship in a production bundle even by accident.

`github.viewerOrgMembership` gets a matching env-gated bypass returning `member`.  Without
it every dev page renders the "GitHub check unavailable" state and the happy path is
invisible.

`devSeed` extends from one member plus one session to a full fixture org: a lead, a
developer, a candidate, a suspended member, and sessions that are active, expired and
revoked.

**Both gates are deployment environment variables, not client flags.** A client cannot
opt itself into either.

### Scenario fixtures

Presenter components take data as props. A `DataProvider` React context supplies the
handful of reads and writes the UI needs — `viewer`, `members`, `sessions`, `auditLog`,
`orgConfig`, `membership`, and the mutations against them.

- The real implementation calls Convex hooks.
- The dev implementation reads `?scenario=<name>`, returns fixture data, and returns
  mutations that no-op or reject on demand.

The dev implementation is only reachable when `import.meta.env.DEV`; in a production build
the scenario module is not imported and tree-shakes away.

Scenarios:

`loading` · `signed-out` · `candidate` · `developer` · `lead` · `suspended` ·
`not-member` · `org-unavailable` · `sessions-empty` · `sessions-one` · `sessions-many` ·
`session-expired` · `session-revoked` · `audit-empty` · `audit-full` · `mutation-error` ·
`overflow` · `authorize` · `authorize-bad-params` · `authorize-done` · `authorize-error` ·
`boom`

`overflow` is the adversarial one: 300-character device labels, unbroken 200-character
strings with no spaces, emoji, CJK, RTL Arabic, and empty strings in every optional field.

### Gallery

`/__ui` renders every component in every prop combination, grouped by component. It is the
surface that makes the library itself testable rather than only testable through pages.

## Visual testing

`riabuild-web/e2e/`, Playwright, Chromium and WebKit (both already cached locally).

**Visual suite.** Every scenario at three viewports — 380, 768 and 1440 — plus the
gallery. Screenshots are written to `e2e/__screens__/`. These are looked at by a human or
by Claude reading the image files; that inspection is the point of the suite.

**Smoke suite.** Real dev sign-in against a local Convex backend, walking `/`,
`/cli/authorize` and a bad path.

**Assertions on every page, at every viewport**, independent of pixels:

- no horizontal document overflow
- no console errors or unhandled rejections
- every interactive element reachable by tab and showing a visible focus ring
- `axe-core` reports no violations
- no element clipped by an ancestor's `overflow: hidden`

### The loop

Not done until this has converged:

1. `pnpm ui:check` — runs the visual suite, writes screenshots
2. Open the screenshots and look at them, every scenario, every viewport
3. Fix what is wrong — clipping, overlap, wrapping, unreadable contrast, broken frame
4. Repeat from 1

A change is finished when a full pass produces no fixes.

### CI

CI runs the structural assertions and the smoke suite, and uploads screenshots as
artifacts. **No committed pixel baselines.** Font rasterisation differs between a
developer's machine and a CI container, so a pixel gate would fail permanently for reasons
unrelated to the change. Pixel judgement stays with the human-in-the-loop step above.

## Skills

Two repo skills in `.claude/skills/`.

**`riabuild-ui`** — read before any UI work in `riabuild-web`. Covers the visual system,
the component inventory, the use-extend-generalize rules, the no-keystrokes interaction
policy, and the requirement that new states get a scenario. Named `riabuild-ui` rather
than `frontend-design` because a `frontend-design` plugin skill already exists and two
entries sharing a name make trigger selection a coin flip.

**`visual-testing`** — read before claiming any UI works. Covers running the suites, the
scenario system, the loop discipline above, and the edge-case checklist: empty, one, many,
huge, unicode, unbroken string, error, loading, disabled, narrow viewport, 200% zoom.

## Out of scope

- Light mode. Dark only, deliberately.
- Keyboard navigation beyond the browser's native tab order.
- Any change to `/api/v1`, the Rust CLI, or the auth and authorization model.
- Committed pixel baselines.

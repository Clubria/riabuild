---
name: riabuild-ui
description: Use when building, changing, styling or reviewing any user interface in riabuild-web — a page, a route, a component, a form, a table, a state, an error screen, CSS or design tokens. Covers the fake-TUI visual system, the src/ui component library, and the rules for extending it.
---

# The riabuild console

`riabuild-web` is one framed terminal. Everything visual goes through
`riabuild-web/src/ui/`. Design: `docs/superpowers/specs/2026-08-05-tui-console-design.md`.

**Before claiming any UI works, read `.claude/skills/visual-testing/SKILL.md`.** Looking at
the rendered result is not optional here.

## The one rule people get wrong

**It looks like a TUI. It behaves like a website.**

The page handles **no keystrokes**. No `j`/`k` navigation, no `:` command palette, no
modal key mode, no global `keydown` listener. Every affordance is a real focusable
`<button>` or `<a>` in the browser's own tab order.

The corollary is the part that actually bites:

> **Never render a hint for a key we do not handle.**

No `^K cmd` in the status bar. No `q quit`. No `[1] setup` tab numbers. No `press ? for
help`. A terminal that advertises keybindings and ignores them is worse than one that
advertises none — it teaches the reader something false, and they find out by pressing the
key and having nothing happen.

Keyboard *accessibility* is a different thing and is required: tab order, visible focus,
and `tabIndex={0}` on any region that scrolls. That is the browser's keyboard handling,
not ours.

## Use the library

`import { Button, Panel, DataTable, ... } from "../ui"`.

| Component | For |
|---|---|
| `Screen` | the terminal frame — title bar, body, status bar |
| `TitleBar` / `StatusBar` | chrome; the status bar reports **state only** |
| `Panel` | a titled box; `index` gives it a step number |
| `Button` | every action — `primary` / `quiet` / `danger`, `pending`, `href` |
| `Field` / `TextArea` / `Select` | labelled controls with hint and error slots |
| `Badge` / `Dot` | state, as a chip or an indicator |
| `Alert` | something the reader must act on |
| `Empty` | a list with nothing in it, saying why |
| `Loading` | a wait, with the blinking cursor |
| `Command` | a `$ …  [copy]` shell line |
| `KeyValue` | the machine-fact grid |
| `DataTable` | **any** list of rows with actions |

A page never hand-rolls a button, an input, a badge or a box.

### Extending it

Three moves, in this order:

1. **Reuse.** If a component fits, use it.
2. **Generalize.** If it *almost* fits, add a prop — driven by a second real caller, never
   speculatively. `DataTable` exists because Members, Sessions and AuditLog were three
   copies of one thing; do not make a fourth.
3. **Promote.** If a pattern appears a second time in a page, move it into `src/ui/` with
   its own file and a barrel export.

Never fork a near-duplicate component. Two components that differ by a colour are one
component with a `tone` prop.

Anything added or given a new prop gets an entry in `/__ui` (`src/routes/Gallery.tsx`)
covering the new state — and that state gets looked at.

## Visual system

Dark only. There is no light variant and no `prefers-color-scheme` branch; do not add one.

Tokens live in `src/ui/tokens.css`. Use the semantic names (`bg`, `bg-sunk`, `bg-raised`,
`fg`, `fg-dim`, `fg-faint`, `accent`, `ok`, `warn`, `danger`, `rule`) — never a raw hex in
a component. Tones map through `src/ui/tone.ts` so a `danger` badge and a `danger` panel
are the same red.

**Every foreground token clears 4.5:1 against every background token.** If you change one,
recompute — `fg-faint` was originally 3.18:1 and axe caught it across 18 nodes. There is a
contrast script pattern in the spec; the visual suite fails on any regression.

### Drawing the frame

**No box-drawing characters in the DOM as structure.** Literal `─│┌┘` needs a fixed
character grid, breaks on any resize or font fallback, and makes a screen reader announce
"box drawings light horizontal" once per cell.

Frames are 1px CSS borders. Panel titles notch into the top rule by being positioned over
it with the page background behind. Corner glyphs, if any, go on pseudo-elements, which
screen readers skip.

ASCII belongs in the DOM only where it is *content*: `●` status dots, `$` prompts, `▸`
markers, `[ ]` button brackets. Each is either inside an `aria-hidden` span with a real
label alongside, or is itself the accessible label.

### Layout

- Long machine values — device labels, repo slugs, error strings — get `wrap-value`. They
  wrap; they never widen the page.
- Nothing sets `overflow: hidden` to tidy up a layout. Clipping hides exactly the bugs the
  visual suite exists to catch. Content that is too wide scrolls its own container, and
  that container gets `tabIndex={0}`.
- Test at 380px before declaring anything done. That is where a table runs out of room.

## Data

Components never call `useQuery`. **`src/data/convexProvider.tsx` is the only file in
`src/` that may import from `convex/react`.** Verify with:

```sh
grep -rn "convex/react" src/ --include=*.tsx | grep -v data/convexProvider   # must be empty
```

Pages read `useData()`; leaf presenters take props. This is what makes every state
renderable from fixtures — a component that fetches its own data cannot be shown holding
a suspended member with an expired session and a 300-character device label.

**Adding a UI state means adding a scenario** in `src/dev/scenarios.ts`. A state with no
scenario is a state nobody has ever looked at.

## Routes and failure

Routing is `src/app/route.ts` — an explicit list, because a 404 is impossible without one.
New destination → add it there, and to `DASHBOARD_TABS` if it belongs in the tab strip
(tabs are real anchors to sections that exist).

Every failure has a screen: `NotFound` for a bad path, `ErrorBoundary` for a thrown
render, `offlineData` for a missing backend. Wrap a new admin-only panel in its own
`ErrorBoundary` so one failing query does not blank the page.

Error detail is shown only under `import.meta.env.DEV`. A production error string can
carry backend internals.

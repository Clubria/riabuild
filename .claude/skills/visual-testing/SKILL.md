---
name: visual-testing
description: Use when verifying, testing, or claiming that any riabuild-web user interface works — before saying a page, component, layout or style change is done. Covers the Playwright suites, the scenario fixtures, and the look-at-every-screenshot loop.
---

# Looking at the UI

**A UI change is not done until you have looked at it rendering, in every state, at every
viewport.** Not a sample. `tsc` passing and a component "looking right in the code" are not
evidence about pixels.

Design rules for the UI itself are in `.claude/skills/riabuild-ui/SKILL.md`.

## The loop

```sh
cd riabuild-web
pnpm ui:check                 # all scenarios × 380 / 768 / 1440
```

1. Run it.
2. **Open every screenshot in `e2e/__screens__/` and look at it.** Read the PNGs. This is
   the step that gets skipped and it is the only step that catches a layout that is
   technically valid and visually broken.
3. Fix what is wrong — clipping, overlap, a control that vanished at 380px, unreadable
   contrast, a broken frame, an empty state that says nothing useful.
4. Repeat from 1.

Done means **a full pass produced no fixes**. Not "the failures I saw are fixed".

Narrow the loop while iterating, then always finish with a full run:

```sh
pnpm exec playwright test -c e2e/playwright.config.ts visual.spec.ts --project=narrow
pnpm exec playwright test -c e2e/playwright.config.ts visual.spec.ts --project=narrow -g overflow
```

## What runs without you

`e2e/helpers.ts` asserts, on every page at every viewport:

- the document does not scroll horizontally, naming the widest offender
- nothing is clipped by an `overflow: hidden` ancestor
- every interactive element is tabbable and shows a focus ring
- `axe-core` reports no WCAG 2.1 AA violations
- no console errors or unhandled rejections

These catch the mechanical failures. They do not catch ugly, confusing or misaligned —
that is what your eyes are for.

## Scenarios

`src/dev/scenarios.ts` drives everything. `?scenario=<name>` swaps Convex for fixtures, so
any data shape is one URL away:

```
http://127.0.0.1:5199/?scenario=overflow
http://127.0.0.1:5199/__ui                    # component gallery
```

**Every UI state you add gets a scenario.** The spec list is generated from `SCENARIOS`,
so a new scenario is automatically screenshotted at three viewports — and a state with no
scenario is a state nobody has ever looked at.

All fixture timestamps derive from a frozen `NOW`. Never call `Date.now()` in a fixture: a
suite whose data moves with the wall clock produces a different image every run and stops
being evidence of anything. Playwright pins locale to `en-GB` and timezone to `UTC` for
the same reason.

## The edge-case checklist

For every list, field and value, ask what happens with:

| | |
|---|---|
| **empty** | zero rows, `""` in every optional field |
| **one** | a single row — do headers still make sense? |
| **many** | 40 rows; does anything need to scroll or paginate? |
| **huge** | a 300-character label, an unbroken 200-character string with no spaces |
| **unicode** | emoji, CJK, RTL Arabic mixed into an LTR row |
| **loading** | before data arrives |
| **error** | the query failed; the mutation was rejected |
| **disabled** | the control the viewer is not allowed to use |
| **narrow** | 380px — where tables run out of room |
| **zoom** | 200% browser zoom, which is 380px in disguise |

The `overflow` scenario carries the adversarial versions of these. When you add a
component, add its hostile case there too.

## Debugging an overflow

The assertion names the widest element, but that element is often *inside* a legitimate
scroll container and not the cause. Walk the ancestors and find the topmost element that
is genuinely wider than the viewport and is **not** inside an `overflow-x: auto` ancestor.

In practice the cause is nearly always a long unbroken string in a flex row: a flex item
defaults to `min-width: auto` and refuses to shrink below its content. The fix is
`min-w-0` on the item plus `wrap-value` on the text — not `overflow: hidden` on a parent,
which only hides it from you.

## The smoke suite

```sh
pnpm exec playwright test -c e2e/playwright.config.ts smoke.spec.ts
```

Signs in for real against a local Convex deployment and walks the pages. It needs
`RIABUILD_DEV_AUTH=1` and `RIABUILD_DEV_SEED=1` on that deployment, and skips itself when
no backend is reachable — so the visual suite is never blocked by backend availability.

Fixtures prove the shapes render. The smoke suite proves the wiring is real. You want both
and they answer different questions.

## CI

CI runs the structural assertions and uploads the screenshots as artifacts. **There are no
committed pixel baselines**, deliberately: font rasterisation differs between a laptop and
a CI container, so a pixel gate would fail forever for reasons unrelated to any change.
Pixel judgement stays with the human-in-the-loop step above. Do not add snapshot
baselines to CI.

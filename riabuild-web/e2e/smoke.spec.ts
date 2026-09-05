import type { Page } from "@playwright/test";
import { checkPage, expect, test } from "./helpers";

/**
 * The real thing: dev sign-in against a live Convex deployment, no fixtures.
 *
 * The scenario suite proves every shape renders. This proves the wiring behind
 * them is real — that `convexProvider` maps the actual query results onto the
 * same `Data` contract the fixtures implement. Fixtures cannot catch a renamed
 * field; this can.
 *
 * It runs as part of `pnpm ui:check`, which is what CI runs — for a long time
 * it did not, because that script named `visual.spec.ts` and nothing named this
 * one, so the suite documented as the one that catches a renamed field was
 * executed by nobody.
 *
 * It skips itself when no backend is reachable, so a missing local deployment
 * never blocks the visual suite, and `RIABUILD_E2E_BACKEND=1` turns that skip
 * into a failure for a run that arranged one. It needs, on that deployment:
 *
 *   RIABUILD_DEV_AUTH=1     registers the dev sign-in provider
 *   RIABUILD_DEV_SEED=1     allows devSeed:seedOrgForDev
 *   RIABUILD_BOOTSTRAP_LEADS=devlead
 */
const SIGN_IN = /sign in as devlead/i;

/**
 * Long enough for a cold Vite dev server to transform the entry modules and for
 * `@convex-dev/auth` to settle. Only ever spent when there is no backend: with
 * one, the button is there as soon as the app renders.
 */
const BACKEND_TIMEOUT = 15_000;

/**
 * Turns "no backend" from a skip into a failure.
 *
 * A run that went to the trouble of standing a deployment up wants to hear
 * about it when the deployment is not answering — a silent skip there is the
 * suite reporting green for tests that never ran. Off by default, because on a
 * laptop with no `pnpm dev` running the skip is the right answer and blocking
 * the visual suite behind backend availability is not.
 */
const BACKEND_REQUIRED = process.env.RIABUILD_E2E_BACKEND === "1";

/**
 * Asked once per worker. The answer cannot change mid-run, and the probe is the
 * only slow thing here when there is nothing to talk to.
 */
let backendUp: boolean | null = null;

/**
 * A panel, located by its heading rather than by its title text.
 *
 * `Panel` notches the title into its top rule as an `<h2>`, and that heading's
 * accessible name is the step index followed by the title — `04 your machines`,
 * `lead audit log`. Naming it in full is what makes these locators pick one
 * node and keep picking it.
 *
 * `getByText(title)` did not. It matches any element whose own text *contains*
 * the string, and on the lead dashboard three of these titles are a substring
 * of something else:
 *
 *   audit log          the heading; the ngrok note ("…answered by the audit log
 *                      below and not by ngrok"); `loading audit log` while the
 *                      query is in flight; `Could not load the audit log` when
 *                      it fails
 *   your machines      the heading; `Could not list your machines`
 *   org configuration  the heading; `core dumped — org configuration`, from the
 *                      ErrorBoundary wrapped around that panel
 *
 * The first CI run of this suite — its first run anywhere, because `ui:check`
 * named only `visual.spec.ts` until this branch — died on exactly that: against
 * a real backend the audit query had not answered yet, so `audit log` found the
 * heading and `loading audit log` together and strict mode refused to guess.
 *
 * The other two are located the same way regardless. A title that is unique
 * today is unique until somebody adds a panel, and a suite that only tightens
 * the locator that has already failed spends a CI cycle per collision.
 *
 * Matched in full rather than as `/audit log/i`, which would still also find
 * `err · core dumped — the audit log`. A panel that crashed must fail this
 * test, and it should fail saying the panel is not there rather than saying two
 * nodes matched.
 */
function panel(page: Page, name: string) {
  return page.getByRole("heading", { name });
}

/**
 * The dashboard's `<h1>` — what being signed in looks like, and the wait every
 * test here starts with once the sign-in button is clicked.
 */
function signedIn(page: Page) {
  return page.getByRole("heading", {
    name: "One command builds the machine.",
    level: 1,
  });
}

/**
 * Waits until no panel is still fetching, and is the gate `checkPage` needs.
 *
 * `Loading` is the only thing in `src/` that renders `role="status"`, so one
 * locator finds every panel with a query in flight. It matters because the
 * dashboard's `<h1>` arrives with the `members.viewer` result and the other
 * eight queries land after it, each swapping a one-line placeholder for a table
 * — so a `checkPage` fired the moment the heading appears photographs, measures
 * overflow on, tabs through and runs axe over whichever half had arrived. That
 * is a different page on every run, and the focus sweep is the worst of it: it
 * presses Tab sixty times against a DOM that is still growing new stops
 * underneath it.
 *
 * Web-first and about the condition rather than a duration: a query that never
 * answers fails here saying a panel is still loading, which is the truth and is
 * worth a failure.
 *
 * Not folded into `checkPage` itself, because the fixture suite deliberately
 * screenshots permanent loading states — `loading`, `viewer-missing` and
 * `org-unavailable` are scenarios whose whole subject is a spinner.
 */
async function settled(page: Page) {
  await expect(
    page.getByRole("status"),
    "a panel was still loading when the page was checked",
  ).toHaveCount(0);
}

/**
 * Tagged so this suite runs under one viewport instead of three.
 *
 * Not because these pages are width-independent — they are the same pages the
 * fixture suite shoots at 380, 768 and 1440 — but because what this proves is
 * the wiring, and the wiring is the same at every width. Three runs of it cost
 * three sign-ins against a real deployment to assert one thing.
 */
test.describe("against a real backend", { tag: "@viewport-agnostic" }, () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    const signIn = page.getByRole("button", { name: SIGN_IN });

    if (backendUp === null) {
      // `isVisible()` asks with no waiting at all, so a first render that had
      // not landed yet answered "no backend" and the whole suite skipped
      // itself — quietly, and most likely on the slow machine where running it
      // mattered. A real wait cannot be raced by a slow render.
      backendUp = await signIn
        .waitFor({
          state: "visible",
          timeout: BACKEND_TIMEOUT,
        })
        .then(
          () => true,
          () => false,
        );
    }

    if (BACKEND_REQUIRED) {
      // Fails with the locator's own message, naming what was looked for.
      await expect(
        signIn,
        "RIABUILD_E2E_BACKEND=1 asked for a deployment with RIABUILD_DEV_AUTH=1, and none answered",
      ).toBeVisible({ timeout: BACKEND_TIMEOUT });
    }

    test.skip(
      !backendUp,
      "no local Convex deployment with RIABUILD_DEV_AUTH=1 — run `pnpm dev` first, " +
        "or set RIABUILD_E2E_BACKEND=1 to make its absence a failure",
    );
  });

  test("a lead can sign in and reach every panel", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: SIGN_IN }).click();

    await expect(signedIn(page)).toBeVisible();
    await expect(panel(page, "01 confirm your profile")).toBeVisible();
    await expect(panel(page, "04 your machines")).toBeVisible();
    await expect(panel(page, "lead members and roles")).toBeVisible();
    await expect(panel(page, "lead org configuration")).toBeVisible();
    await expect(panel(page, "lead audit log")).toBeVisible();

    await settled(page);
    await checkPage(page, info, consoleErrors, { screenshot: "smoke-lead" });
  });

  test("a bad path still 404s when signed in", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: SIGN_IN }).click();
    await expect(signedIn(page)).toBeVisible();

    await page.goto("/nope");
    // The whole line rather than the phrase: `command not found` is a substring
    // match that would go strict-mode ambiguous the moment a second element
    // mentions it, and matching the path too is the assertion worth making —
    // the 404 exists to name what was asked for.
    await expect(
      page.getByText("command not found: /nope", { exact: true }),
    ).toBeVisible();
    await settled(page);
    await checkPage(page, info, consoleErrors, { screenshot: "smoke-404" });
  });

  /**
   * The state that stranded a developer on riabuild.clubria.com: signed out,
   * with a refresh token still in storage that the deployment will not honour.
   *
   * `@convex-dev/auth` writes the JWT and the refresh token together and
   * removes them together, so removing one is exactly the tear a rejected
   * refresh leaves behind. The library never reconciles it — `fetchAccessToken`
   * rethrows and erases nothing — so before `useStaleCredentialReset` this
   * survived every reload and sent the sign-in screen straight back.
   *
   * No `checkPage` here: the console output during a torn load belongs to the
   * library, and the assertion worth making is that storage came out clean.
   */
  test("stale sign-in state clears itself", async ({ page }) => {
    await page.getByRole("button", { name: SIGN_IN }).click();
    await expect(signedIn(page)).toBeVisible();

    const refreshTokens = () =>
      page.evaluate(() =>
        Object.keys(localStorage).filter((key) =>
          key.startsWith("__convexAuthRefreshToken"),
        ),
      );

    await expect
      .poll(refreshTokens, {
        message: "signing in should leave a refresh token to tear",
      })
      .not.toEqual([]);

    /**
     * The tear happens in the *next* document, before a line of app code runs,
     * and that is the whole reason this test is deterministic.
     *
     * Deleting the JWT from the live page and then reloading is the obvious
     * way to write it, and it is a race CI lost. `@convex-dev/auth` does not
     * stop working when `signIn` resolves: it immediately exchanges the stored
     * refresh token for a fresh pair — a second `POST /api/action` calling
     * `auth:signIn`, this time with `{ refreshToken }` — and on the response
     * it writes a new JWT *and* a rotated refresh token back to
     * `localStorage`. Nothing in the page tells you that is outstanding, and
     * the dashboard heading above appears while it still is.
     *
     * The trace from run 32533013839 has it to the millisecond: JWT deleted at
     * t+95589, `reload()` issued at t+95610, the exchange's response landing
     * at t+95694 — 84ms into the gap before the navigation committed at
     * t+95830. The old document was still alive to receive it, so the JWT went
     * straight back, the reload found a whole credential, and the app rendered
     * the signed-in lead dashboard. The failure is `element(s) not found` for
     * five seconds because the sign-in button was never going to be there, and
     * the run before it passed on the same commit because that one response
     * happened to land 20ms earlier.
     *
     * An init script closes the window rather than widening it. It runs in the
     * *next* document, before `ConvexAuthProvider` reads storage, so it cannot
     * matter whether the exchange landed before the reload, inside the gap, or
     * not at all — whatever storage holds when the app boots, the JWT is not
     * in it. Holding that response back and letting it land after the tear
     * fails the old version every time and this one never.
     *
     * Guarded so it fires once. Playwright runs an init script on every later
     * navigation in this page, and the sign-in at the end of this test must be
     * allowed to keep its credential.
     */
    await page.addInitScript(() => {
      if (sessionStorage.getItem("e2e-tore-the-jwt") === "1") return;
      sessionStorage.setItem("e2e-tore-the-jwt", "1");
      for (const key of Object.keys(localStorage)) {
        if (key.startsWith("__convexAuthJWT")) localStorage.removeItem(key);
      }
    });
    await page.reload();

    // The door is open again...
    await expect(page.getByRole("button", { name: SIGN_IN })).toBeVisible();

    // ...and the dead half went with it, rather than waiting to break the next
    // attempt the way it did in production.
    await expect.poll(refreshTokens, { timeout: 10_000 }).toEqual([]);

    // The point of all of it: signing in works without clearing site data.
    await page.getByRole("button", { name: SIGN_IN }).click();
    await expect(signedIn(page)).toBeVisible();
  });

  /**
   * The failure that used to render as a blank sign-in screen.
   *
   * When the OAuth cookies do not reach `/api/auth/callback/*`,
   * `@convex-dev/auth` swallows the error and answers a bare redirect to
   * `SITE_URL` — no `code`, no error, byte-identical to somebody typing the
   * address in. `functions/_proxy.ts` marks that redirect and this is the other
   * end of the mark.
   *
   * Against a real backend rather than a fixture on purpose. The
   * `signin-round-trip-failed` scenario proves the alert *renders*; only a real
   * page load proves the part that cannot be faked — that `authFailure.ts`
   * captures the parameter before `ConvexAuthProvider` mounts, and hands the
   * developer a clean URL afterwards so a reload is an ordinary visit again.
   */
  test("a callback that produced no code says so", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto("/?authFailed=1");

    await expect(
      page.getByText("Sign-in did not complete", { exact: true }),
    ).toBeVisible();
    // Still a door, not a dead end.
    await expect(page.getByRole("button", { name: SIGN_IN })).toBeVisible();
    // Stripped in the same breath it was read, so this is not the page's
    // permanent opinion of itself.
    expect(page.url()).not.toContain("authFailed");

    await settled(page);
    await checkPage(page, info, consoleErrors, {
      screenshot: "smoke-signin-failed",
    });
  });

  test("the authorize page renders for a signed-in machine", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: SIGN_IN }).click();
    await expect(signedIn(page)).toBeVisible();

    // No code: against a real backend there is no pending request to find, so
    // the code box is what this proves renders and accepts input.
    await page.goto("/cli");
    await expect(page.getByLabel(/code from your terminal/i)).toBeVisible();
    await settled(page);
    await checkPage(page, info, consoleErrors, {
      screenshot: "smoke-authorize",
    });
  });
});

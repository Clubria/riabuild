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

    await checkPage(page, info, consoleErrors, { screenshot: "smoke-lead" });
  });

  test("a bad path still 404s when signed in", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: SIGN_IN }).click();
    await expect(signedIn(page)).toBeVisible();

    await page.goto("/nope");
    await expect(page.getByText("command not found")).toBeVisible();
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

    expect(
      await refreshTokens(),
      "signing in should leave a refresh token to tear",
    ).not.toEqual([]);

    await page.evaluate(() => {
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
    await checkPage(page, info, consoleErrors, {
      screenshot: "smoke-authorize",
    });
  });
});

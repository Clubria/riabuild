import { AUTHORIZE_QUERY, SCENARIO_NAMES } from "../src/dev/scenarios";
import { checkPage, expect, test } from "./helpers";

/** Scenarios whose whole point is rendering a failure. */
const EXPECTS_CONSOLE_ERRORS = new Set(["boom"]);

test.describe("scenarios", () => {
  for (const scenario of SCENARIO_NAMES) {
    test(scenario, async ({ page, consoleErrors }, info) => {
      const query = AUTHORIZE_QUERY[scenario];
      const path =
        query === undefined
          ? `/?scenario=${scenario}`
          : `/cli/authorize?scenario=${scenario}&${query}`;

      await page.goto(path);
      await checkPage(page, info, consoleErrors, {
        screenshot: scenario,
        expectConsoleErrors: EXPECTS_CONSOLE_ERRORS.has(scenario),
      });
    });
  }
});

test("component gallery", async ({ page, consoleErrors }, info) => {
  await page.goto("/__ui?scenario=signed-out");
  await expect(page.getByText(/Component gallery\. Dev builds only/)).toBeVisible();
  await checkPage(page, info, consoleErrors, { screenshot: "gallery" });
});

test("404", async ({ page, consoleErrors }, info) => {
  await page.goto("/does/not/exist?scenario=signed-out");
  await expect(page.getByText("command not found")).toBeVisible();
  await checkPage(page, info, consoleErrors, { screenshot: "404" });
});

test("404 does not render a path as markup", async ({ page }) => {
  await page.goto("/%3Cimg%20src=x%20onerror=alert(1)%3E?scenario=signed-out");
  await expect(page.locator("body img")).toHaveCount(0);
  await expect(page.getByText("command not found")).toBeVisible();
});

/**
 * The 404 must survive the backend being unreachable — it is one of the screens
 * that exists for when things are broken. Without a scenario and without a
 * Convex URL the app falls back to offline data, which is exactly the state a
 * misconfigured deployment is in.
 */
test("404 renders with no backend at all", async ({
  page,
  consoleErrors,
}, info) => {
  await page.goto("/does/not/exist");
  await expect(page.getByText("command not found")).toBeVisible();
  await checkPage(page, info, consoleErrors, {
    screenshot: "404-offline",
    expectConsoleErrors: true,
  });
});

/**
 * A tab that jumps to a panel must land with that panel's title on screen.
 *
 * The title is notched into the top rule — absolutely positioned *above* the
 * section's own border box — so the scroll target starts below the panel's
 * visible top edge. With no scroll margin the browser parks the border box at
 * y=0 and the heading is sliced off above the fold, which is the reader
 * arriving at "01 · CONFIRM YOUR PROFILE" and seeing the bottom half of it.
 *
 * A screenshot of the page at rest cannot catch this: it only exists after the
 * jump.
 */
test.describe("section anchors", () => {
  test("landing on a panel shows its title", async ({ page }) => {
    await page.goto("/?scenario=lead");

    const tabs = page.locator('nav[aria-label="Sections"] a');
    // `evaluateAll` does not auto-wait the way `click` and `textContent` do:
    // it resolves against whatever matches at that instant, and zero matches
    // is a valid answer, so it returns [] rather than retrying. Without this
    // the test races the first render — which it lost about one viewport in
    // three, failing on an assertion written as a sanity check.
    await expect(tabs.first()).toBeVisible();
    const hrefs = await tabs.evaluateAll((els) =>
      els.map((el) => el.getAttribute("href") ?? ""),
    );
    expect(hrefs.length, "dashboard tabs to jump to").toBeGreaterThan(0);

    const clipped: string[] = [];
    for (const href of hrefs) {
      await page.locator(`nav[aria-label="Sections"] a[href="${href}"]`).click();
      // The jump is the browser's, not ours; give it a frame to settle.
      await page.waitForTimeout(300);

      const top = await page.evaluate((selector) => {
        const title = document.querySelector(selector)?.querySelector("h2");
        return title === null || title === undefined
          ? null
          : Math.round(title.getBoundingClientRect().top);
      }, href);

      if (top === null || top < 0) clipped.push(`${href} title at y=${top}`);
    }

    expect(clipped, "panel titles cut off above the fold after a tab jump").toEqual(
      [],
    );
  });
});

/**
 * Failure states that only exist after someone clicks something. A scenario that
 * renders them at rest would be lying about how they are reached.
 */
test.describe("interaction states", () => {
  test("a rejected mutation surfaces in a panel", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto("/?scenario=mutation-error");
    await page.getByLabel("first name").fill("Changed");
    await page.getByRole("button", { name: /save profile/i }).click();
    await expect(page.getByText("Not saved")).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "mutation-error-after-save",
    });
  });

  test("a rejected authorisation surfaces in a panel", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto(
      `/cli/authorize?scenario=authorize-error&${AUTHORIZE_QUERY["authorize-error"]}`,
    );
    await page.getByRole("button", { name: /approve this machine/i }).click();
    await expect(page.getByText("Not approved")).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "authorize-error-after-click",
    });
  });

  test("approving reaches the hand-off screen", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto(
      `/cli/authorize?scenario=authorize&${AUTHORIZE_QUERY.authorize}`,
    );
    await page.getByRole("button", { name: /approve this machine/i }).click();
    await expect(page.getByText("Back to your terminal.")).toBeVisible();
    await checkPage(page, info, consoleErrors, { screenshot: "authorize-done" });
  });

  test("an unknown scenario name fails loudly", async ({ page }) => {
    await page.goto("/?scenario=no-such-fixture");
    await expect(page.getByText("unknown scenario")).toBeVisible();
  });
});

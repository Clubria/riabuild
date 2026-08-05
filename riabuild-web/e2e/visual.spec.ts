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

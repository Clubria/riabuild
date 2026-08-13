import {
  ED25519_FINGERPRINT,
  ED25519_PRIVATE,
  ENCRYPTED_PRIVATE,
} from "../convex/lib/opensshKey.fixtures";
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
          : `/cli?scenario=${scenario}&${query}`;

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

  test("an address riabuild-web refuses says which rule it broke", async ({
    page,
    consoleErrors,
  }, info) => {
    // The rule that is not cosmetic — a hostname `ssh` would read as an option
    // — so the sentence a lead gets back has to name it rather than say the
    // save failed.
    await page.goto("/?scenario=shared-server-refused");
    // Exact: "name" alone also matches "first name", "last name" and
    // "username", and a filled-in username with an empty name is a screenshot
    // that shows something other than what this test says it does.
    await page.getByLabel("name", { exact: true }).fill("gpu");
    await page.getByLabel("hostname").fill("-oProxyCommand=x");
    await page.getByLabel("username").fill("ada");
    await page.getByRole("button", { name: /add server/i }).click();
    await expect(page.getByText("A hostname cannot start with a dash.")).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "shared-server-refused-after-add",
    });
  });

  test("editing an address warns that it is a different machine", async ({
    page,
    consoleErrors,
  }, info) => {
    // A rename is free; an address change re-identifies the server for every
    // developer, and the warning is what stops a lead doing it by accident.
    await page.goto("/?scenario=lead");
    await page.getByRole("button", { name: "Edit shared-gpu" }).click();
    await page.getByLabel("hostname").fill("gpu-2.internal");
    await expect(
      page.getByText("This is a different machine to riabuild"),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "shared-server-readdressed",
    });
  });

  test("a pasted key fills in what will be stored, before it is stored", async ({
    page,
    consoleErrors,
  }, info) => {
    // The whole point of the paste box: a lead can see the public key and the
    // fingerprint that the row will carry, derived in the browser from the key
    // itself, while the private half is still only in the textarea.
    await page.goto("/?scenario=lead");
    await page.getByLabel("key name").fill("prod-bastion");
    await page.getByLabel("private key").fill(ED25519_PRIVATE);
    await expect(page.getByText(ED25519_FINGERPRINT)).toBeVisible();
    // Scoped to the preview's own definition list: the table above it lists a
    // key of the same type, and an unscoped match would pass on that row while
    // the preview stayed empty.
    await expect(
      page.getByRole("definition").filter({ hasText: /^ssh-ed25519$/ }),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "issued-key-preview",
    });
  });

  test("a key that will not parse says so at the box, not after saving", async ({
    page,
    consoleErrors,
  }, info) => {
    // A passphrase-protected key is the likeliest bad paste, and it is refused
    // before any round trip: `ssh-add` would prompt for that passphrase on a
    // developer's laptop with nobody able to answer it.
    await page.goto("/?scenario=lead");
    await page.getByLabel("key name").fill("prod-bastion");
    await page.getByLabel("private key").fill(ENCRYPTED_PRIVATE);
    await expect(page.getByText(/protected by a passphrase/)).toBeVisible();
    // And the control that would store it stays unavailable.
    await expect(page.getByRole("button", { name: /add key/i })).toBeDisabled();
    await checkPage(page, info, consoleErrors, {
      screenshot: "issued-key-unparseable",
    });
  });

  test("a failed lookup surfaces in a panel", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto(
      `/cli?scenario=authorize-error&${AUTHORIZE_QUERY["authorize-error"]}`,
    );
    await expect(page.getByText("Not approved")).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "authorize-error-after-lookup",
    });
  });

  test("approving reaches the back-to-your-terminal screen", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto(`/cli?scenario=authorize&${AUTHORIZE_QUERY.authorize}`);
    await page.getByRole("button", { name: /approve this machine/i }).click();
    await expect(page.getByText("Back to your terminal.")).toBeVisible();
    await checkPage(page, info, consoleErrors, { screenshot: "authorize-done" });
  });

  test("denying says plainly that nothing was granted", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto(`/cli?scenario=authorize&${AUTHORIZE_QUERY.authorize}`);
    await page.getByRole("button", { name: /^deny$/i }).click();
    await expect(page.getByText("Nothing was granted.")).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "authorize-denied",
    });
  });

  test("typing a code by hand finds the machine", async ({ page }) => {
    // The SSH path: no prefilled code, the developer reads eight characters off
    // a terminal on another computer and types them here.
    await page.goto(`/cli?scenario=authorize-empty`);
    await page
      .getByLabel(/code from your terminal/i)
      .fill("wxzbcdfg");
    await expect(
      page.getByRole("button", { name: /approve this machine/i }),
    ).toBeVisible();
  });

  test("an unknown scenario name fails loudly", async ({ page }) => {
    await page.goto("/?scenario=no-such-fixture");
    await expect(page.getByText("unknown scenario")).toBeVisible();
  });
});

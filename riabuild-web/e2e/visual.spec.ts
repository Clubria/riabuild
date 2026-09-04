import type { Page } from "@playwright/test";
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
  await expect(
    page.getByText(/Component gallery\. Dev builds only/),
  ).toBeVisible();
  await checkPage(page, info, consoleErrors, { screenshot: "gallery" });
});

test("404", async ({ page, consoleErrors }, info) => {
  await page.goto("/does/not/exist?scenario=signed-out");
  await expect(page.getByText("command not found")).toBeVisible();
  await checkPage(page, info, consoleErrors, { screenshot: "404" });
});

test(
  "404 does not render a path as markup",
  {
    tag: "@viewport-agnostic",
  },
  async ({ page }) => {
    await page.goto(
      "/%3Cimg%20src=x%20onerror=alert(1)%3E?scenario=signed-out",
    );
    await expect(page.locator("body img")).toHaveCount(0);
    await expect(page.getByText("command not found")).toBeVisible();
  },
);

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
      await page
        .locator(`nav[aria-label="Sections"] a[href="${href}"]`)
        .click();
      const top = await settledTitleTop(page, href);
      if (top === null || top < 0) clipped.push(`${href} title at y=${top}`);
    }

    expect(
      clipped,
      "panel titles cut off above the fold after a tab jump",
    ).toEqual([]);
  });
});

/**
 * Where a panel's title has come to rest after the browser's jump, or `null` if
 * the panel has no title at all.
 *
 * This used to be `waitForTimeout(300)` — a number chosen to be longer than a
 * scroll usually takes, in the one test whose entire subject is where the page
 * ends up. A slow machine reads mid-scroll and fails for no reason; a scroll
 * that quietly grows past 300ms passes for no reason. Two identical readings
 * fifty milliseconds apart is the actual question ("has it stopped moving?"),
 * asked of the actual thing being measured.
 */
async function settledTitleTop(
  page: Page,
  href: string,
): Promise<number | null> {
  const read = () =>
    page.evaluate((selector) => {
      const title = document.querySelector(selector)?.querySelector("h2");
      return title === null || title === undefined
        ? null
        : Math.round(title.getBoundingClientRect().top);
    }, href);

  let previous = await read();
  await expect
    .poll(
      async () => {
        const current = await read();
        const stopped = current === previous;
        previous = current;
        return stopped;
      },
      {
        message: `the jump to ${href} never came to rest`,
        intervals: [50],
        timeout: 5_000,
      },
    )
    .toBe(true);
  return previous;
}

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
    await expect(
      page.getByText("A hostname cannot start with a dash."),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "shared-server-refused-after-add",
    });
  });

  test("a folder riabuild-web refuses says how to spell it instead", async ({
    page,
    consoleErrors,
  }, info) => {
    // The rule a lead is most likely to hit, because it is what the Infisical
    // UI shows them: a path copied with its trailing slash. Refused rather
    // than trimmed, so the sentence has to say which spelling to use — "not
    // saved" alone leaves them retyping the same thing.
    await page.goto("/?scenario=secret-path-refused");
    // Exact: "repository" alone also matches org config's "default
    // repository" box, and filling that one instead would be a screenshot of
    // something other than what this test says it is.
    await page
      .getByLabel("repository", { exact: true })
      .fill("Clubria/payments");
    await page.getByLabel("infisical folders").fill("/apps/payments/");
    await page.getByRole("button", { name: /map repository/i }).click();
    await expect(
      page.getByText("Leave the trailing slash off", { exact: false }),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "secret-path-refused-after-map",
    });
  });

  test("editing a mapping warns that every developer's files are rewritten", async ({
    page,
    consoleErrors,
  }, info) => {
    // Moving a repository to another folder is what makes every `.env.<name>`
    // on the team stale, and the warning only exists once a lead has actually
    // changed the list — a scenario rendering it at rest would claim it
    // appears before they touched anything.
    await page.goto("/?scenario=lead");
    await page
      .getByRole("button", { name: "Edit the folders for Clubria/payments" })
      .click();
    await page.getByLabel("infisical folders").fill("/apps/somewhere-else");
    await expect(
      page.getByText("Every developer's files are rewritten"),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "secret-path-moved-before-save",
    });
  });

  test(
    "Enter in the hostname box does what the button does",
    {
      tag: "@viewport-agnostic",
    },
    async ({ page }) => {
      // The panel was four fields in a `div`, so Enter did nothing at all. The
      // refusal only appears if a submit actually ran, which makes it the proof
      // that one did.
      await page.goto("/?scenario=shared-server-refused");
      await page.getByLabel("name", { exact: true }).fill("gpu");
      await page.getByLabel("username").fill("ada");
      await page.getByLabel("hostname").fill("-oProxyCommand=x");
      await page.getByLabel("hostname").press("Enter");
      await expect(
        page.getByText("A hostname cannot start with a dash."),
      ).toBeVisible();
    },
  );

  test("removing the ngrok token asks first, and names what goes", async ({
    page,
    consoleErrors,
  }, info) => {
    // The only irreversible control in the dashboard: no route returns the
    // token, so the click that destroys it cannot be the same one that reaches
    // for it by accident next to "save org config".
    await page.goto("/?scenario=lead");
    await page.getByRole("button", { name: /remove ngrok authtoken/i }).click();
    await expect(
      page.getByText("Remove the ngrok authtoken ending …tok3?"),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: /^remove the token$/i }),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "ngrok-token-removal-confirm",
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

  /**
   * The invite panel does not fetch the org's members until a lead asks it to,
   * so every state past "nobody invited from here yet" only exists after a
   * click. A scenario rendering the filled-in form at rest would be claiming it
   * arrives without one.
   */
  test("listing the org turns typing a name into picking one", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto("/?scenario=lead");
    await page.getByRole("button", { name: /list the org.s members/i }).click();

    // Only people with no riabuild row are offered: the fixture org holds ilya
    // and dana, who are already members, alongside those who are not.
    const person = page.getByLabel("person");
    await expect(person).toBeVisible();
    const offered = await person.locator("option").allTextContents();
    expect(offered).toContain("priya");
    expect(offered).not.toContain("dana");

    await expect(
      page.getByRole("button", { name: /invite @priya as developer/i }),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, { screenshot: "invite-picker" });
  });

  test("a key can be picked out before the person exists", async ({
    page,
    consoleErrors,
  }, info) => {
    // The half of this feature that has no mechanism of its own: an invited row
    // is a real member row, so the existing grant takes it.
    await page.goto("/?scenario=lead");
    await page.getByRole("button", { name: /list the org.s members/i }).click();
    const key = page
      .getByRole("group", { name: "keys to issue them" })
      .getByRole("button")
      .first();
    await key.click();
    await expect(key).toHaveAttribute("aria-pressed", "true");
    await checkPage(page, info, consoleErrors, {
      screenshot: "invite-with-key",
    });
  });

  test("an org riabuild cannot list says so, and offers the retry", async ({
    page,
    consoleErrors,
  }, info) => {
    // Distinct from the member list failing: the members are fine, and what is
    // missing is the thing that makes a typo impossible.
    await page.goto("/?scenario=invite-org-unreachable");
    await page.getByRole("button", { name: /list the org.s members/i }).click();
    await expect(
      page.getByText("Could not list the org's members"),
    ).toBeVisible();
    await expect(page.getByText(/GITHUB_ORG_TOKEN is not set/)).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "invite-org-unreachable-after-list",
    });
  });

  test("an org with nobody left to invite says that, not nothing", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto("/?scenario=invite-nobody-left");
    await page.getByRole("button", { name: /list the org.s members/i }).click();
    await expect(
      page.getByText("Everyone in the org is already here."),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "invite-nobody-left-after-list",
    });
  });

  test("a refused invitation says who it collided with", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto("/?scenario=invite-refused");
    await page.getByRole("button", { name: /list the org.s members/i }).click();
    await page.getByRole("button", { name: /invite @/i }).click();
    await expect(page.getByText("Not invited")).toBeVisible();
    await expect(
      page.getByText("@priya has already been invited."),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "invite-refused-after-invite",
    });
  });

  test("an org that issues no keys explains the missing row", async ({
    page,
    consoleErrors,
  }, info) => {
    // A lead who came here to hand somebody a key must not find the control
    // silently absent and conclude the feature is broken.
    await page.goto("/?scenario=invite-no-keys");
    await page.getByRole("button", { name: /list the org.s members/i }).click();
    await expect(
      page.getByText(/The org issues no SSH keys yet/),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "invite-no-keys-after-list",
    });
  });

  /**
   * The picker at its widest: a 39-character login is what GitHub permits, and
   * an option that long is what pushes a `<select>` past the column it sits in.
   * 380px is where that shows up.
   */
  test("the picker survives the longest login GitHub allows", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.goto("/?scenario=overflow");
    await page.getByRole("button", { name: /list the org.s members/i }).click();
    await expect(page.getByLabel("person")).toBeVisible();
    await checkPage(page, info, consoleErrors, {
      screenshot: "invite-picker-overflow",
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
    await checkPage(page, info, consoleErrors, {
      screenshot: "authorize-done",
    });
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

  test(
    "typing a code by hand finds the machine",
    {
      tag: "@viewport-agnostic",
    },
    async ({ page }) => {
      // The SSH path: no prefilled code, the developer reads eight characters off
      // a terminal on another computer and types them here.
      await page.goto(`/cli?scenario=authorize-empty`);
      await page.getByLabel(/code from your terminal/i).fill("wxzbcdfg");
      await expect(
        page.getByRole("button", { name: /approve this machine/i }),
      ).toBeVisible();
    },
  );

  test(
    "an unknown scenario name fails loudly",
    {
      tag: "@viewport-agnostic",
    },
    async ({ page }) => {
      await page.goto("/?scenario=no-such-fixture");
      await expect(page.getByText("unknown scenario")).toBeVisible();
    },
  );
});

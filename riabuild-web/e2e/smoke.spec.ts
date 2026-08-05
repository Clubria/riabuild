import { checkPage, expect, test } from "./helpers";

/**
 * The real thing: dev sign-in against a live Convex deployment, no fixtures.
 *
 * The scenario suite proves every shape renders. This proves the wiring behind
 * them is real — that `convexProvider` maps the actual query results onto the
 * same `Data` contract the fixtures implement. Fixtures cannot catch a renamed
 * field; this can.
 *
 * It skips itself when no backend is reachable, so a missing local deployment
 * never blocks the visual suite. It needs, on that deployment:
 *
 *   RIABUILD_DEV_AUTH=1     registers the dev sign-in provider
 *   RIABUILD_DEV_SEED=1     allows devSeed:seedOrgForDev
 *   RIABUILD_BOOTSTRAP_LEADS=devlead
 */
test.describe("against a real backend", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    const hasBackend = await page
      .getByRole("button", { name: /sign in as devlead/i })
      .isVisible()
      .catch(() => false);
    test.skip(
      !hasBackend,
      "no local Convex deployment with RIABUILD_DEV_AUTH=1 — run `pnpm dev` first",
    );
  });

  test("a lead can sign in and reach every panel", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: /sign in as devlead/i }).click();

    await expect(page.getByText("One command builds the machine.")).toBeVisible();
    await expect(page.getByText("confirm your profile")).toBeVisible();
    await expect(page.getByText("your machines")).toBeVisible();
    await expect(page.getByText("members and roles")).toBeVisible();
    await expect(page.getByText("org configuration")).toBeVisible();
    await expect(page.getByText("audit log")).toBeVisible();

    await checkPage(page, info, consoleErrors, { screenshot: "smoke-lead" });
  });

  test("a bad path still 404s when signed in", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: /sign in as devlead/i }).click();
    await expect(page.getByText("One command builds the machine.")).toBeVisible();

    await page.goto("/nope");
    await expect(page.getByText("command not found")).toBeVisible();
    await checkPage(page, info, consoleErrors, { screenshot: "smoke-404" });
  });

  test("the authorize page renders for a signed-in machine", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: /sign in as devlead/i }).click();
    await expect(page.getByText("One command builds the machine.")).toBeVisible();

    await page.goto(
      `/cli/authorize?state=${"s".repeat(20)}&challenge=${"c".repeat(40)}` +
        `&port=51789&label=smoke-machine&version=2026.08.04`,
    );
    await expect(
      page.getByRole("button", { name: /approve this machine/i }),
    ).toBeVisible();
    await checkPage(page, info, consoleErrors, { screenshot: "smoke-authorize" });
  });
});

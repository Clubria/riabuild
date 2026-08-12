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
    await page.getByRole("button", { name: /sign in as devlead/i }).click();
    await expect(page.getByText("One command builds the machine.")).toBeVisible();

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
    await expect(
      page.getByRole("button", { name: /sign in as devlead/i }),
    ).toBeVisible();

    // ...and the dead half went with it, rather than waiting to break the next
    // attempt the way it did in production.
    await expect.poll(refreshTokens, { timeout: 10_000 }).toEqual([]);

    // The point of all of it: signing in works without clearing site data.
    await page.getByRole("button", { name: /sign in as devlead/i }).click();
    await expect(page.getByText("One command builds the machine.")).toBeVisible();
  });

  test("the authorize page renders for a signed-in machine", async ({
    page,
    consoleErrors,
  }, info) => {
    await page.getByRole("button", { name: /sign in as devlead/i }).click();
    await expect(page.getByText("One command builds the machine.")).toBeVisible();

    // No code: against a real backend there is no pending request to find, so
    // the code box is what this proves renders and accepts input.
    await page.goto("/cli");
    await expect(page.getByLabel(/code from your terminal/i)).toBeVisible();
    await checkPage(page, info, consoleErrors, { screenshot: "smoke-authorize" });
  });
});

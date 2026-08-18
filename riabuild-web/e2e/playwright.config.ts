import { defineConfig, devices } from "@playwright/test";

/**
 * `RIABUILD_UI_PORT` because `reuseExistingServer` is on off CI, and the port was
 * fixed: a second worktree running this suite found the first one's Vite already
 * listening, reused it, and screenshotted **another checkout's code**. Every
 * assertion still ran, most still passed, and the images were of a page nobody
 * was testing — which is worse than a failure, because a green run is evidence
 * of nothing at all.
 *
 * Overriding the port is how a worktree gets its own server without stopping
 * anybody else's. CI is unaffected: it sets neither, and `reuseExistingServer`
 * is false there.
 */
const PORT = Number(process.env.RIABUILD_UI_PORT ?? 5199);

/**
 * Three viewports, because the failures live at the edges: 380 is a small phone
 * where a table has no room for its columns, 1440 is where a page can look
 * empty, and 768 is the breakpoint itself.
 *
 * Locale and timezone are pinned. `formatTime` calls `toLocaleString(undefined)`,
 * so without pinning them a screenshot taken in CI and one taken on a laptop
 * differ for reasons that have nothing to do with the change under test.
 */
export default defineConfig({
  testDir: ".",
  outputDir: "./__results__",
  fullyParallel: true,
  forbidOnly: process.env.CI === "true",
  retries: 0,

  /**
   * Playwright's default is *half* the logical cores, which on the four-vCPU
   * runner meant 147 tests sharing two workers while half the machine sat idle
   * for five minutes. Summing the reported test durations came to 566
   * worker-seconds against a 294s wall clock, so the schedule was already 96%
   * packed — there was no waste to reclaim, only workers to add.
   *
   * Three rather than four, because the count is not the only thing competing
   * for those cores: `webServer` below runs Vite in dev mode and transforms
   * modules on demand as each test navigates, so it needs one to itself. A
   * fourth worker takes the suite from contended to oversubscribed, and what
   * fails then is a visual assertion timing out somewhere unrelated to whatever
   * change is being tested.
   *
   * Left at Playwright's default off CI, where the machine has other work to do
   * and the suite is not the thing being waited on.
   */
  workers: process.env.CI === "true" ? 3 : undefined,
  reporter: process.env.CI === "true" ? [["github"], ["list"]] : [["list"]],

  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    locale: "en-GB",
    timezoneId: "UTC",
    colorScheme: "dark",
    trace: "retain-on-failure",
  },

  projects: [
    {
      name: "narrow",
      use: { ...devices["Desktop Chrome"], viewport: { width: 380, height: 800 } },
    },
    {
      name: "medium",
      use: { ...devices["Desktop Chrome"], viewport: { width: 768, height: 1024 } },
    },
    {
      name: "wide",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } },
    },
  ],

  webServer: {
    // Bound explicitly: Vite's default `localhost` can resolve to ::1 only,
    // and then nothing on 127.0.0.1 ever answers.
    command: `pnpm exec vite --port ${PORT} --strictPort --host 127.0.0.1`,
    url: `http://127.0.0.1:${PORT}/?scenario=lead`,
    reuseExistingServer: process.env.CI !== "true",
    timeout: 60_000,
    cwd: "..",
  },
});

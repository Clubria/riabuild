import { defineConfig, devices } from "@playwright/test";

const PORT = 5199;

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

import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "edge-runtime",
    // `src/` too, for the pure helpers pages depend on. Components are covered
    // by the Playwright suite instead — rendering them here would mean a second
    // environment and a DOM shim to assert things a screenshot already shows.
    include: ["convex/**/*.test.ts", "src/**/*.test.ts"],
    server: { deps: { inline: ["convex-test"] } },
  },
});

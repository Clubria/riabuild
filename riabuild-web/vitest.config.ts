import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "edge-runtime",
    // Testing Library's auto-cleanup (unmounting between tests) only
    // registers itself when it finds a global `afterEach` — hence globals.
    globals: true,
    include: ["convex/**/*.test.ts", "src/**/*.test.tsx"],
    setupFiles: ["./src/setupTests.ts"],
    server: { deps: { inline: ["convex-test"] } },
  },
});

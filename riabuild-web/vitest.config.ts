import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "edge-runtime",
    // Testing Library's auto-cleanup (unmounting between tests) only
    // registers itself when it finds a global `afterEach` — hence globals.
    globals: true,
    // Three sets: Convex functions, the pure helpers under `src/` that pages
    // depend on (`.test.ts`), and the handful of components whose behaviour a
    // screenshot cannot assert (`.test.tsx`) — `Copyable`'s clipboard write and
    // its copied/idle announcement, for instance. The Playwright suite still
    // owns everything a screenshot *can* show; these are the exceptions, not a
    // second rendering strategy.
    include: [
      "convex/**/*.test.ts",
      "src/**/*.test.ts",
      "src/**/*.test.tsx",
    ],
    setupFiles: ["./src/setupTests.ts"],
    server: { deps: { inline: ["convex-test"] } },
  },
});

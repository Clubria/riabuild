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
    // `functions/` is the fourth set: the Cloudflare Pages Function that serves
    // `/api/auth/*` from the dashboard's own origin. It runs in no browser and
    // renders nothing, so a screenshot can say nothing about it, and the parts
    // that break — folded `Set-Cookie` values, a relayed `content-encoding`, a
    // callback bounced with no `code` — are all header handling. `edge-runtime`
    // is the environment that has the same `Request`/`Response`/`Headers` it
    // will meet in production.
    include: [
      "convex/**/*.test.ts",
      "functions/**/*.test.ts",
      "src/**/*.test.ts",
      "src/**/*.test.tsx",
    ],
    setupFiles: ["./src/setupTests.ts"],
    server: { deps: { inline: ["convex-test"] } },
  },
});

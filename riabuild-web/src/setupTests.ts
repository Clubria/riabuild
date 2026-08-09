// Registers jest-dom's matchers (`toBeVisible`, `toHaveTextContent`, ...) for
// component tests. Loaded for every suite, including the Convex ones under
// `convex/`, but it only extends `expect` — it does not touch `document`, so
// it is a no-op outside a DOM environment.
import "@testing-library/jest-dom/vitest";

/**
 * Kept in its own module so `main.tsx` can decide which provider to boot without
 * statically importing the fixtures — that import is what would drag them into
 * the production bundle.
 */
export function scenarioName(): string | null {
  if (!import.meta.env.DEV) return null;
  const value = new URLSearchParams(window.location.search).get("scenario");
  return value === null || value === "" ? null : value;
}

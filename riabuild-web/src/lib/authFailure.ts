/**
 * Whether GitHub sent this developer back and sign-in still did not happen.
 *
 * The OAuth callback cannot report its own failure. `@convex-dev/auth` catches
 * every error inside `/api/auth/callback/*` and answers a bare redirect to the
 * dashboard, so a failed round trip and an ordinary visit arrive at this page as
 * exactly the same request. That is why the same bug has been diagnosed from
 * scratch more than once: there was nothing to read. `functions/_proxy.ts` sits
 * in the one position that can tell the two apart — it sees the redirect leave
 * without a `code` — and marks the URL. This reads the mark.
 *
 * Read once, eagerly, at module load. The flag has to be captured before
 * anything can rewrite the URL, and `AuthProvider` rewrites it as soon as React
 * mounts; a module-level read runs before React exists at all, which is the
 * only ordering that needs no reasoning about effects.
 *
 * The parameter is stripped in the same breath, so a reload is an ordinary visit
 * rather than the same complaint again. `replaceState` rather than `assign`: the
 * developer is looking at the page already, and reloading it to tidy a query
 * parameter would throw away the very state being reported.
 */
import { CALLBACK_FAILED_PARAM } from "./authCallbackParam";

export function hasAuthFailureMark(search: string): boolean {
  return new URLSearchParams(search).get(CALLBACK_FAILED_PARAM) !== null;
}

export function withoutAuthFailureMark(url: URL): string {
  const stripped = new URL(url.toString());
  stripped.searchParams.delete(CALLBACK_FAILED_PARAM);
  return stripped.pathname + stripped.search + stripped.hash;
}

function captureAtLoad(): boolean {
  if (typeof window === "undefined" || window.location === undefined) {
    return false;
  }
  if (!hasAuthFailureMark(window.location.search)) return false;

  try {
    const url = new URL(window.location.href);
    window.history.replaceState({}, "", withoutAuthFailureMark(url));
  } catch {
    // A history the browser will not let us rewrite is not a reason to
    // withhold the message the rewrite was tidying up after.
  }
  return true;
}

const CAME_BACK_WITHOUT_SIGNING_IN = captureAtLoad();

/**
 * True for the life of this page load, false after a reload. The sign-in screen
 * is the only caller: it turns this into the sentence the developer needed the
 * first three times this happened.
 */
export function signInRoundTripFailed(): boolean {
  return CAME_BACK_WITHOUT_SIGNING_IN;
}

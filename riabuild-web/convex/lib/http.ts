/**
 * The one way riabuild-web talks to anything it does not host.
 *
 * Not to be confused with `convex/http.ts`, which is the *inbound* `/api/v1`
 * router. This is the outbound side: every `fetch` to api.github.com or
 * Infisical goes through `fetchUpstream`, and none of them uses the global
 * `fetch` directly.
 *
 * The reason is that a bare `fetch` has no deadline. A slow upstream holds a
 * Convex action open until the platform kills it, and the caller waiting on it
 * waits just as long — which for `checkOrgMembership` means every
 * secret-brokering request, every `/api/v1/org/config`, and, through the ngrok
 * shim, every single `ngrok` invocation on every laptop. A GitHub that is
 * merely slow would then be indistinguishable from a GitHub that is down, and
 * the failure would arrive as a timeout somewhere else rather than as the 503
 * each caller already knows how to report.
 *
 * Ten seconds is chosen against the two callers that matter: an org membership
 * check that a healthy api.github.com answers in well under a second, and an
 * Infisical universal-auth login of the same shape. Anything past that is not
 * slow, it is broken, and every call site already fails closed.
 */
export const UPSTREAM_TIMEOUT_MS = 10_000;

/**
 * `signal` is deliberately not accepted. Its whole job here is to carry the
 * deadline, and a call site that could supply its own could supply one that
 * never fires — which is the unbounded fetch this exists to remove.
 */
export type UpstreamInit = Omit<RequestInit, "signal">;

/**
 * `fetch` with a deadline, and a failure that says so.
 *
 * The abort surfaces as a `TimeoutError` whose own message names neither the
 * URL nor the limit, and every call site funnels errors into a single
 * "could not reach X" string — so it is renamed here, where both are still in
 * hand.
 */
export async function fetchUpstream(
  url: string,
  init: UpstreamInit = {},
  timeoutMs: number = UPSTREAM_TIMEOUT_MS,
): Promise<Response> {
  try {
    return await fetch(url, {
      ...init,
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (error) {
    if (isTimeout(error)) {
      throw new Error(`timed out after ${timeoutMs}ms`);
    }
    throw error;
  }
}

/**
 * A timeout rather than a refused connection.
 *
 * Checked by name rather than with `instanceof DOMException`: the same code
 * runs in the Convex isolate, in Node during `vitest`, and against whatever a
 * test has put in `globalThis.fetch`, and the three do not agree on which
 * class an abort throws.
 */
function isTimeout(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "name" in error &&
    (error as { name?: unknown }).name === "TimeoutError"
  );
}

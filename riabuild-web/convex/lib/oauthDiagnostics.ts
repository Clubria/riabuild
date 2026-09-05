/**
 * What GitHub actually said, when a sign-in did not complete.
 *
 * `@convex-dev/auth` runs the OAuth token exchange through `oauth4webapi`, and
 * every failure inside it reaches the log as one of two library sentences:
 * `"response" body "access_token" property must be a string`, or `server
 * responded with an error in the response body`. Neither carries the one thing
 * a reader needs — the `error` GitHub put in the body. A stale authorization
 * code, a rejected client secret and a redirect URI that does not match are
 * three different repairs, and all three arrive worded identically.
 *
 * This was found the hard way, and the cost is worth writing down. A failure
 * here bounces the developer back to the sign-in screen carrying no `code`,
 * which is the silent failure `functions/_proxy.ts` exists to mark. This is
 * that same silence one layer further down, on the server, in the one place
 * where the reason is actually known and is currently thrown away. Reproducing
 * it from outside is close to impossible: every GitHub token-endpoint error
 * reachable without a real authorization code — a wrong secret, a missing one,
 * an unregistered redirect URI — comes back **HTTP 200**, so the shapes that
 * matter cannot be provoked by probing, only observed in flight.
 *
 * Nothing here logs a credential. A successful exchange is not described at
 * all. A failed one is described by its status, its content type, the OAuth
 * error fields — whose names and values are public by construction — and the
 * *names* of any other keys the body carried, never their values.
 */

/**
 * The OAuth error fields. RFC 6749 defines these as the machine-readable
 * explanation, so repeating them is the entire point rather than a leak.
 */
const REPORTABLE_FIELDS = ["error", "error_description", "error_uri"] as const;

/**
 * Names that may never be echoed even as a key, because a body carrying one is
 * a body worth being careful about. Everything else is reported by name only.
 */
const CREDENTIAL_FIELDS = new Set([
  "access_token",
  "refresh_token",
  "id_token",
]);

/** Prefix every line shares, so a deployment's logs can be filtered to these. */
export const OAUTH_FAILURE_PREFIX = "riabuild: GitHub OAuth";

/**
 * A response body, if it was JSON we could read. Reads a clone, so the caller's
 * response is left untouched for the library that actually consumes it.
 */
async function readJsonObject(
  response: Response,
): Promise<Record<string, unknown> | null> {
  try {
    const parsed: unknown = await response.clone().json();
    return typeof parsed === "object" &&
      parsed !== null &&
      !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

/** Host and path only. The token request carries its secrets in the body. */
function describeEndpoint(url: string): string {
  try {
    const parsed = new URL(url);
    return `${parsed.host}${parsed.pathname}`;
  } catch {
    return "an unparseable URL";
  }
}

/**
 * Describe a failed OAuth response, or return `null` when there is nothing
 * wrong to say. A 200 whose body carries an `error` counts as failed: that is
 * exactly what GitHub returns for a stale code, and it is the case that
 * otherwise surfaces as a complaint about a missing `access_token`.
 */
export async function describeOAuthFailure(
  url: string,
  response: Response,
): Promise<string | null> {
  const body = await readJsonObject(response);
  const carriesError = body !== null && typeof body.error === "string";
  if (response.status === 200 && !carriesError) return null;

  const parts = [
    `${OAUTH_FAILURE_PREFIX} request to ${describeEndpoint(url)} failed`,
    `HTTP ${response.status}`,
    `content-type ${response.headers.get("content-type") ?? "(none)"}`,
  ];

  if (body === null) {
    parts.push("body was not readable JSON");
    return parts.join(" — ");
  }

  for (const field of REPORTABLE_FIELDS) {
    const value = body[field];
    if (typeof value === "string" && value.length > 0) {
      parts.push(`${field}=${JSON.stringify(value)}`);
    }
  }

  const otherKeys = Object.keys(body)
    .filter((key) => !REPORTABLE_FIELDS.includes(key as never))
    .map((key) => (CREDENTIAL_FIELDS.has(key) ? `${key} (redacted)` : key));
  if (otherKeys.length > 0) {
    parts.push(`other body keys: ${otherKeys.join(", ")}`);
  }

  return parts.join(" — ");
}

/**
 * The `fetch` the GitHub provider runs its OAuth requests through. It changes
 * nothing about the request or the response — it only says, once, what a
 * failure was, and hands the untouched response back to the library.
 */
export function loggingOAuthFetch(
  fetchImpl: typeof fetch = fetch,
  log: (message: string) => void = console.error,
): typeof fetch {
  return async (input, init) => {
    const response = await fetchImpl(input, init);
    const url =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.toString()
          : input.url;
    const failure = await describeOAuthFailure(url, response);
    if (failure !== null) log(failure);
    return response;
  };
}

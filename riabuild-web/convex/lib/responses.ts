/**
 * The error shape the CLI depends on.
 *
 * Every failure reaches a developer who may not be technical, so `message` says
 * what went wrong in their terms and `action` says what to do about it. The CLI
 * prints both verbatim.
 */
export type ApiErrorCode =
  | "unauthenticated"
  | "session_expired"
  | "session_revoked"
  | "not_org_member"
  | "suspended"
  | "forbidden"
  | "cli_too_old"
  | "bad_request"
  | "not_configured"
  | "org_check_unavailable"
  | "upstream_error"
  | "session_unknown"
  /**
   * A session that was itself minted by another machine asked to mint a third.
   * One hop only — see `sessions.delegate`.
   */
  | "delegation_not_permitted";

export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": "application/json",
      "cache-control": "no-store",
    },
  });
}

export function apiError(
  status: number,
  code: ApiErrorCode,
  message: string,
  action: string,
): Response {
  return jsonResponse({ error: { code, message, action } }, status);
}

/** Thrown by helpers so a handler can `catch` and return the prepared Response. */
export class ApiFailure extends Error {
  constructor(readonly response: Response) {
    super("api failure");
  }
}

export function fail(
  status: number,
  code: ApiErrorCode,
  message: string,
  action: string,
): never {
  throw new ApiFailure(apiError(status, code, message, action));
}

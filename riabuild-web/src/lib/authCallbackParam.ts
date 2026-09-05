/**
 * The one query parameter the OAuth proxy and the dashboard both have to spell
 * the same way.
 *
 * It lives in its own file, with nothing else in it, because the two ends are in
 * different TypeScript projects: `functions/_proxy.ts` writes it at the edge and
 * `src/lib/authFailure.ts` reads it in the browser. A shared constant is the
 * only thing that keeps a rename from silently disabling the message — which is
 * the failure mode this whole mechanism exists to prevent, so it would be a
 * poor one to reintroduce here.
 *
 * `tsconfig.functions.json` names this file for the same reason
 * `tsconfig.e2e.json` names `src/dev/scenarios.ts`.
 */
export const CALLBACK_FAILED_PARAM = "authFailed";

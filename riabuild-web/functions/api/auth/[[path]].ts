import { proxyAuthRequest, upstreamOriginFrom } from "../../_proxy";

/**
 * `/api/auth/*`, on the dashboard's own origin. `../../_proxy.ts` is the file
 * that explains why this exists; everything here is the Cloudflare Pages
 * adapter around it.
 *
 * The double-bracket name is a Pages catch-all, so this one file answers both
 * `/api/auth/signin/github` and `/api/auth/callback/github` — and anything else
 * `@convex-dev/auth` adds under that prefix later, without a second file to
 * remember. `public/_routes.json` narrows the paths Pages invokes a Function
 * for at all, so the SPA fallback in `public/_redirects` keeps every other URL.
 *
 * The context is typed here rather than by depending on
 * `@cloudflare/workers-types`: two fields is not worth a package, and a wrong
 * guess about either of them fails at the first request rather than quietly.
 */
type PagesContext = {
  request: Request;
  env: Record<string, string | undefined>;
};

export const onRequest = (context: PagesContext): Promise<Response> =>
  proxyAuthRequest(context.request, upstreamOriginFrom(context.env));

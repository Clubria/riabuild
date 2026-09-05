/**
 * The OAuth round trip, served from the dashboard's own origin.
 *
 * `@convex-dev/auth` carries the OAuth `state` and the PKCE code verifier
 * between the two legs of a GitHub sign-in in cookies, and it sets them on
 * whichever host serves `/api/auth/*`. Until this existed that host was
 * `handsome-vulture-127.eu-west-1.convex.site` — a registrable domain that is
 * third-party to `riabuild.clubria.com` and that no developer ever visits on
 * purpose. What the browser sees is a domain it has no first-party
 * relationship with, which sets `SameSite=None` cookies during a redirect
 * chain and is never rendered: the exact shape every tracking-prevention
 * feature is built to catch. Safari's is the strictest, and its judgement is
 * stored per browser profile, outside cookies and outside local storage — so
 * the developer it happens to cannot clear it, a fresh profile does not have it
 * yet, and the two profiles disagree for as long as anybody cares to test.
 *
 * What that costs is invisible, which is why it took several attempts. When the
 * cookies do not arrive, `/api/auth/callback/github` answers
 * `Response.redirect(SITE_URL)` — a 302 carrying no `code`, indistinguishable
 * from an ordinary visit. The developer authorises on GitHub, lands back on the
 * dashboard, and is shown the sign-in screen, with nothing in the page, the
 * console or the URL saying why. It reproduces against the live deployment with
 * nothing but an empty cookie jar:
 *
 *     $ curl -sD- -o/dev/null \
 *         'https://handsome-vulture-127.eu-west-1.convex.site/api/auth/callback/github?code=x&state=y'
 *     HTTP/2 302
 *     location: https://riabuild.clubria.com
 *
 * So the fix is to stop having a third-party domain in the flow at all. This
 * Pages Function serves `/api/auth/*` from `riabuild.clubria.com` and hands each
 * request to Convex unchanged; `CUSTOM_AUTH_SITE_URL` is what makes the library
 * name this origin in both legs. The cookies then belong to the origin the
 * developer uses every day, and the sign-in is shaped like every other
 * first-party OAuth app on the web — which is the point. Nothing here is a
 * browser workaround: no browser is asked to relax anything.
 *
 * It is a proxy rather than a Convex custom domain because a custom domain is
 * the thing `docs/deploying.md` already priced and declined —
 * `api.riabuild.clubria.com` has no edge certificate (two labels below the
 * apex) and Convex routes HTTP actions by hostname, so it needs a paid plan.
 * This needs no new hostname, no new certificate and no plan change:
 * `riabuild.clubria.com` is already served by this Pages project.
 *
 * `/api/v1` is deliberately not proxied. The CLI calls it directly, holds no
 * cookies and is not a browser, so none of the above applies to it, and routing
 * it through here would put a second hop under the endpoint every developer's
 * provisioning run depends on, in exchange for nothing.
 */

// Added to the dashboard URL when the callback produced no `code` — the silent
// failure described above, and the only evidence of it that ever reaches a
// browser. `src/lib/authFailure.ts` is what reads it back.
import { CALLBACK_FAILED_PARAM } from "../src/lib/authCallbackParam";

/** Everything this function serves. `public/_routes.json` must agree. */
export const AUTH_PATH_PREFIX = "/api/auth/";

/** The leg GitHub sends the developer back to. */
export const CALLBACK_PATH_PREFIX = "/api/auth/callback/";

/**
 * Where the Convex HTTP actions really live.
 *
 * Defaulted rather than required, for the reason `dashboardUrl()` in
 * `convex/http.ts` defaults `SITE_URL`: a deployment that forgets the variable
 * should keep working rather than take sign-in down, and there is exactly one
 * right answer for this project. Set a `CONVEX_SITE_URL` variable on the Pages
 * project when the deployment moves.
 */
export const DEFAULT_UPSTREAM =
  "https://handsome-vulture-127.eu-west-1.convex.site";

/**
 * Headers describing how the *upstream* body was framed on the wire. The
 * runtime has already decoded and re-framed it by the time we relay it, so
 * repeating them describes a body that no longer exists — a `content-encoding:
 * gzip` on bytes that are no longer gzipped is a corrupt response.
 */
const REFRAMED_BY_THE_RUNTIME = new Set([
  "content-encoding",
  "content-length",
  "transfer-encoding",
]);

/**
 * Set on every response this function returns.
 *
 * Each one carries either a `Set-Cookie` or a single-use code, and a 302 that
 * some intermediary decided to keep is a sign-in that works exactly once.
 * Convex sets `must-revalidate` on the success leg and nothing at all on the
 * failure leg; this depends on neither.
 */
const NEVER_CACHE = "no-store";

export async function proxyAuthRequest(
  request: Request,
  upstreamOrigin: string,
  fetchImpl: typeof fetch = fetch,
): Promise<Response> {
  const incoming = new URL(request.url);
  const target = new URL(incoming.pathname + incoming.search, upstreamOrigin);

  const headers = new Headers(request.headers);
  // `fetch` derives Host from the URL it is given; carrying the browser's copy
  // across would name this origin to a server that is not it.
  headers.delete("host");

  const hasBody = request.method !== "GET" && request.method !== "HEAD";
  const upstream = await fetchImpl(target.toString(), {
    method: request.method,
    headers,
    // Buffered rather than streamed: the only body in this flow is a form-post
    // callback, it is a few hundred bytes, and streaming one costs a `duplex`
    // negotiation that differs between the runtimes this has to work on.
    body: hasBody ? await request.arrayBuffer() : undefined,
    // The whole point. Following the redirect here would run the hop to GitHub
    // inside this worker, where the developer's cookie jar is not.
    redirect: "manual",
  });

  return relay(incoming, upstream);
}

function relay(incoming: URL, upstream: Response): Response {
  const headers = new Headers();
  for (const [name, value] of upstream.headers) {
    const lower = name.toLowerCase();
    if (lower === "set-cookie") continue;
    if (REFRAMED_BY_THE_RUNTIME.has(lower)) continue;
    headers.set(name, value);
  }
  // Appended one at a time and read through `getSetCookie`, because folding
  // several `Set-Cookie` values into one comma-joined header is how a browser
  // ends up with one cookie named after two. The sign-in leg sets three.
  for (const cookie of setCookiesOf(upstream.headers)) {
    headers.append("set-cookie", cookie);
  }
  headers.set("cache-control", NEVER_CACHE);

  const location = upstream.headers.get("location");
  if (location !== null) {
    const refusal = misconfiguredCallbackUrl(
      incoming.pathname,
      location,
      incoming.origin,
    );
    if (refusal !== null) return refusal;

    const marked = markSilentCallbackFailure(incoming.pathname, location);
    if (marked !== null) headers.set("location", marked);
  }

  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers,
  });
}

/**
 * Refuses the one deployment mistake this design makes possible, before it can
 * become the silent failure it exists to end.
 *
 * Serving `/api/auth/*` here only helps if the library also *names* this origin,
 * which it does when `CUSTOM_AUTH_SITE_URL` is set on the Convex deployment. If
 * it is not, the sign-in leg still runs here — setting its cookies on this
 * origin — and then sends the developer to GitHub with a `redirect_uri` pointing
 * back at `convex.site`, where those cookies are not. GitHub authorises, the
 * callback finds nothing, and the developer is bounced to the sign-in screen
 * with nothing to read: precisely the bug, reintroduced by an unset variable.
 *
 * The mismatch is legible in the `Location` we are about to hand to the browser,
 * so it is caught here rather than reconstructed later from a Convex log nobody
 * reads. A 500 naming the variable is a worse afternoon for whoever deployed and
 * a much better one for everybody else.
 */
function misconfiguredCallbackUrl(
  path: string,
  location: string,
  origin: string,
): Response | null {
  if (!path.startsWith(AUTH_PATH_PREFIX)) return null;
  if (path.startsWith(CALLBACK_PATH_PREFIX)) return null;

  const redirectUri = searchParam(location, "redirect_uri");
  // No `redirect_uri` to check is not evidence of a problem — say nothing.
  if (redirectUri === null) return null;

  let declared: string;
  try {
    declared = new URL(redirectUri).origin;
  } catch {
    return null;
  }
  if (declared === origin) return null;

  return new Response(
    "riabuild: sign-in is misconfigured, and has been stopped rather than " +
      "allowed to fail silently.\n\n" +
      `This origin (${origin}) serves ${AUTH_PATH_PREFIX}, so the OAuth cookies are set here,\n` +
      `but the Convex deployment told GitHub to come back to ${declared} instead,\n` +
      "where those cookies do not exist.\n\n" +
      `Fix: set CUSTOM_AUTH_SITE_URL=${origin} on the Convex deployment, and set the\n` +
      `GitHub OAuth app's callback URL to ${origin}${CALLBACK_PATH_PREFIX}github.\n` +
      'See docs/deploying.md, "Sign-in runs on the dashboard\'s own origin".\n',
    {
      status: 500,
      headers: {
        "content-type": "text/plain; charset=utf-8",
        "cache-control": NEVER_CACHE,
      },
    },
  );
}

/**
 * Marks a callback that produced no `code`.
 *
 * This is the failure the whole file is about, and this function is the only
 * place in the system positioned to see it happen: Convex has already swallowed
 * the error and answered a redirect indistinguishable from an ordinary visit,
 * and the browser is about to follow it. A flag on the URL is enough for the
 * dashboard to say "GitHub sent you back, but sign-in did not complete" instead
 * of showing a blank sign-in screen for the fourth time.
 *
 * It says that a round trip failed, never why — the reason is not knowable from
 * here, and a specific guess stated confidently is worse than an honest shrug.
 */
function markSilentCallbackFailure(
  path: string,
  location: string,
): string | null {
  if (!path.startsWith(CALLBACK_PATH_PREFIX)) return null;
  if (searchParam(location, "code") !== null) return null;

  try {
    const url = new URL(location);
    url.searchParams.set(CALLBACK_FAILED_PARAM, "1");
    return url.toString();
  } catch {
    return null;
  }
}

function searchParam(url: string, name: string): string | null {
  try {
    return new URL(url).searchParams.get(name);
  } catch {
    return null;
  }
}

/**
 * `Headers.getSetCookie` where the runtime has it, and the spec's iteration
 * behaviour where it does not — `set-cookie` is the one header a `Headers`
 * iterator yields once per value rather than comma-joined.
 */
function setCookiesOf(headers: Headers): string[] {
  const withGetter = headers as Headers & { getSetCookie?: () => string[] };
  if (typeof withGetter.getSetCookie === "function") {
    return withGetter.getSetCookie();
  }
  const collected: string[] = [];
  for (const [name, value] of headers) {
    if (name.toLowerCase() === "set-cookie") collected.push(value);
  }
  return collected;
}

export function upstreamOriginFrom(
  env: Record<string, string | undefined>,
): string {
  const configured = env.CONVEX_SITE_URL;
  const chosen =
    configured !== undefined && configured !== ""
      ? configured
      : DEFAULT_UPSTREAM;
  return chosen.replace(/\/+$/, "");
}

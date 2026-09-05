import { describe, expect, it } from "vitest";
import {
  CALLBACK_PATH_PREFIX,
  DEFAULT_UPSTREAM,
  proxyAuthRequest,
  upstreamOriginFrom,
} from "./_proxy";
import { CALLBACK_FAILED_PARAM } from "../src/lib/authCallbackParam";

const ORIGIN = "https://riabuild.clubria.com";
const UPSTREAM = "https://handsome-vulture-127.eu-west-1.convex.site";

type Call = { url: string; init: RequestInit };

/**
 * A stub upstream that records what it was asked for.
 *
 * The whole value of this file is that it exercises the real header handling
 * rather than a description of it: every assertion below is about a `Headers`
 * object the runtime built, which is where the two bugs this code could
 * plausibly have — folded `Set-Cookie` values and a relayed `content-encoding`
 * — would actually appear.
 */
function upstreamReturning(response: Response): {
  fetchImpl: typeof fetch;
  calls: Call[];
} {
  const calls: Call[] = [];
  const fetchImpl = ((url: string, init: RequestInit) => {
    calls.push({ url, init });
    return Promise.resolve(response);
  }) as unknown as typeof fetch;
  return { fetchImpl, calls };
}

function signInRequest(
  path = "/api/auth/signin/github?code=verifier",
): Request {
  return new Request(`${ORIGIN}${path}`, {
    headers: {
      cookie: "__Host-githubOAuthstate=abc",
      host: "riabuild.clubria.com",
    },
  });
}

describe("upstreamOriginFrom", () => {
  it("falls back to the deployment this project actually has", () => {
    expect(upstreamOriginFrom({})).toBe(DEFAULT_UPSTREAM);
    expect(upstreamOriginFrom({ CONVEX_SITE_URL: "" })).toBe(DEFAULT_UPSTREAM);
  });

  it("takes a configured origin, without a trailing slash", () => {
    expect(
      upstreamOriginFrom({ CONVEX_SITE_URL: "https://other.convex.site/" }),
    ).toBe("https://other.convex.site");
  });
});

describe("proxyAuthRequest", () => {
  it("asks the upstream for the same path and query", async () => {
    const { fetchImpl, calls } = upstreamReturning(
      new Response(null, { status: 204 }),
    );
    await proxyAuthRequest(signInRequest(), UPSTREAM, fetchImpl);

    expect(calls[0].url).toBe(
      `${UPSTREAM}/api/auth/signin/github?code=verifier`,
    );
  });

  it("never follows the redirect itself", async () => {
    // Following it here would run the hop to GitHub inside the worker, where
    // the developer's cookies are not — the sign-in would complete for nobody.
    const { fetchImpl, calls } = upstreamReturning(
      new Response(null, { status: 204 }),
    );
    await proxyAuthRequest(signInRequest(), UPSTREAM, fetchImpl);

    expect(calls[0].init.redirect).toBe("manual");
  });

  it("forwards the cookies and drops the browser's Host", async () => {
    const { fetchImpl, calls } = upstreamReturning(
      new Response(null, { status: 204 }),
    );
    await proxyAuthRequest(signInRequest(), UPSTREAM, fetchImpl);

    const sent = new Headers(calls[0].init.headers);
    expect(sent.get("cookie")).toBe("__Host-githubOAuthstate=abc");
    expect(sent.get("host")).toBeNull();
  });

  it("relays every Set-Cookie separately", async () => {
    // The sign-in leg sets three. Folded into one comma-joined header they
    // become a single cookie with a nonsense name, and the callback finds none
    // of them — which is this bug with a different first cause.
    const upstream = new Response(null, {
      status: 302,
      headers: [
        ["location", "https://github.com/login/oauth/authorize"],
        ["set-cookie", "__Host-githubOAuthpkce=one; Path=/; Secure"],
        ["set-cookie", "__Host-githubOAuthstate=two; Path=/; Secure"],
        ["set-cookie", "__Host-githubRedirectTo=/cli; Path=/; Secure"],
      ],
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      signInRequest(),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.headers.getSetCookie()).toEqual([
      "__Host-githubOAuthpkce=one; Path=/; Secure",
      "__Host-githubOAuthstate=two; Path=/; Secure",
      "__Host-githubRedirectTo=/cli; Path=/; Secure",
    ]);
  });

  it("drops the headers that describe a body the runtime has already re-framed", async () => {
    const upstream = new Response("hello", {
      status: 200,
      headers: {
        "content-encoding": "gzip",
        "content-length": "5",
        "content-type": "text/plain",
      },
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      signInRequest(),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.headers.get("content-encoding")).toBeNull();
    expect(response.headers.get("content-length")).toBeNull();
    expect(response.headers.get("content-type")).toBe("text/plain");
  });

  it("lets nothing cache a response carrying a cookie or a single-use code", async () => {
    const upstream = new Response(null, {
      status: 302,
      headers: {
        location: `${ORIGIN}?code=abc`,
        "cache-control": "public, max-age=600",
      },
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      new Request(`${ORIGIN}${CALLBACK_PATH_PREFIX}github?code=gh&state=s`),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

describe("the silent callback failure", () => {
  it("marks a callback that produced no code", async () => {
    // Reproduced against the live deployment by sending the callback no
    // cookies: `302` to the bare dashboard URL, no `code`, no error anywhere.
    const upstream = new Response(null, {
      status: 302,
      headers: { location: ORIGIN },
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      new Request(`${ORIGIN}${CALLBACK_PATH_PREFIX}github?code=gh&state=s`),
      UPSTREAM,
      fetchImpl,
    );

    const location = new URL(response.headers.get("location")!);
    expect(location.searchParams.get(CALLBACK_FAILED_PARAM)).toBe("1");
    expect(location.origin).toBe(ORIGIN);
  });

  it("keeps the rest of the destination, including a redirectTo path", async () => {
    const upstream = new Response(null, {
      status: 302,
      headers: { location: `${ORIGIN}/cli?user_code=WXYZ-1234` },
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      new Request(`${ORIGIN}${CALLBACK_PATH_PREFIX}github?code=gh`),
      UPSTREAM,
      fetchImpl,
    );

    const location = new URL(response.headers.get("location")!);
    expect(location.pathname).toBe("/cli");
    expect(location.searchParams.get("user_code")).toBe("WXYZ-1234");
    expect(location.searchParams.get(CALLBACK_FAILED_PARAM)).toBe("1");
  });

  it("says nothing when the callback succeeded", async () => {
    const upstream = new Response(null, {
      status: 302,
      headers: { location: `${ORIGIN}?code=verification` },
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      new Request(`${ORIGIN}${CALLBACK_PATH_PREFIX}github?code=gh`),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.headers.get("location")).toBe(
      `${ORIGIN}?code=verification`,
    );
  });

  it("does not mark the sign-in leg, which redirects to GitHub with no code of ours", async () => {
    const upstream = new Response(null, {
      status: 302,
      headers: {
        location: `https://github.com/login/oauth/authorize?client_id=x&redirect_uri=${encodeURIComponent(
          `${ORIGIN}${CALLBACK_PATH_PREFIX}github`,
        )}`,
      },
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      signInRequest(),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.status).toBe(302);
    expect(response.headers.get("location")).not.toContain(
      CALLBACK_FAILED_PARAM,
    );
  });
});

describe("the misconfiguration guard", () => {
  it("refuses to send a developer to GitHub with a callback URL on another origin", async () => {
    // `CUSTOM_AUTH_SITE_URL` unset: cookies get set here, GitHub is told to come
    // back to convex.site, and the callback finds nothing. Stopping now is the
    // difference between a deploy that fails and a fleet that cannot sign in.
    const upstream = new Response(null, {
      status: 302,
      headers: {
        location: `https://github.com/login/oauth/authorize?client_id=x&redirect_uri=${encodeURIComponent(
          `${UPSTREAM}${CALLBACK_PATH_PREFIX}github`,
        )}`,
      },
    });
    const { fetchImpl } = upstreamReturning(upstream);

    const response = await proxyAuthRequest(
      signInRequest(),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.status).toBe(500);
    const body = await response.text();
    expect(body).toContain("CUSTOM_AUTH_SITE_URL=https://riabuild.clubria.com");
    expect(body).toContain(UPSTREAM);
  });

  it("passes a matching callback URL straight through", async () => {
    const location = `https://github.com/login/oauth/authorize?client_id=x&redirect_uri=${encodeURIComponent(
      `${ORIGIN}${CALLBACK_PATH_PREFIX}github`,
    )}`;
    const { fetchImpl } = upstreamReturning(
      new Response(null, { status: 302, headers: { location } }),
    );

    const response = await proxyAuthRequest(
      signInRequest(),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe(location);
  });

  it("stays quiet about a redirect carrying no redirect_uri to judge", async () => {
    const { fetchImpl } = upstreamReturning(
      new Response(null, {
        status: 302,
        headers: { location: "https://github.com/login" },
      }),
    );

    const response = await proxyAuthRequest(
      signInRequest(),
      UPSTREAM,
      fetchImpl,
    );

    expect(response.status).toBe(302);
  });
});

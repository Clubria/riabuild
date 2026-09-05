import { describe, expect, test } from "vitest";
import { customFetch } from "@auth/core";
import { GITHUB_ISSUER, GitHubProvider } from "./auth";

/**
 * Regression test for a sign-in outage that produced no error anywhere.
 *
 * GitHub turned on RFC 9207, so its authorization response now carries an
 * `iss` parameter. `oauth4webapi` rejects an `iss` that does not equal the
 * issuer the provider was configured with, and `@convex-dev/auth` supplies the
 * literal placeholder `theremustbeastringhere.dev` for a provider that names
 * none — so every callback threw. The library's OAuth route catches everything
 * and answers `Response.redirect(SITE_URL)` with no `code`, which renders as
 * the sign-in screen: GitHub authorises the developer and the dashboard greets
 * them as a stranger, with nothing in the page, the console or the URL saying
 * why.
 *
 * Asserting a config object is a weak test and this is a deliberate exception.
 * The thing that broke is a constant compared byte for byte against a value
 * only GitHub can produce, the failure is invisible from the client, and the
 * blast radius is every sign-in there is.
 */
describe("the GitHub provider", () => {
  test("expects the issuer GitHub actually sends", () => {
    // Pinned rather than derived: there is nothing here to derive it from, and
    // a value that drifts from GitHub's is the same outage again.
    expect(GITHUB_ISSUER).toBe("https://github.com/login/oauth");
    expect(GitHubProvider.issuer).toBe(GITHUB_ISSUER);
  });

  /**
   * The risk that arrives *with* naming an issuer. `@convex-dev/auth` runs OIDC
   * discovery for any config missing `authorization`, `token` or `userinfo`,
   * and the issuer is what it would discover from — so dropping one of the
   * three would stop this being a plain OAuth provider and put a request to
   * `${issuer}/.well-known/openid-configuration` in front of every sign-in.
   */
  test("still names every endpoint, so nothing is discovered", () => {
    expect(GitHubProvider.type).toBe("oauth");
    expect(GitHubProvider.authorization).toBeDefined();
    expect(GitHubProvider.token).toBeDefined();
    expect(GitHubProvider.userinfo).toBeDefined();
  });

  /**
   * `read:org` is what lets a token answer membership questions; without it
   * every secret-brokering request fails closed. Asserted beside the issuer
   * because both are single strings that break everything when they are wrong.
   */
  test("asks for the scopes membership checks need", () => {
    const scope = (
      GitHubProvider.authorization as { params?: { scope?: string } }
    ).params?.scope;
    expect(scope?.split(/\s+/)).toEqual(
      expect.arrayContaining(["read:user", "user:email", "read:org"]),
    );
  });
});

/**
 * Regression test for a fix that deployed successfully and did nothing.
 *
 * The diagnostic in `lib/oauthDiagnostics.ts` is wired to the provider through
 * `@auth/core`'s `customFetch` symbol. Passing it to `GitHub()` looks right,
 * typechecks, deploys, and is silently ignored: the provider does not spread
 * the config it is given, it stores it under `options`, and the merge that
 * folds `options` back in walks its source with `for...in` — which skips symbol
 * keys. The symbol has to sit on the provider object itself.
 *
 * Nothing else would notice. A dropped `customFetch` changes no behaviour and
 * raises nothing; it just means the one line that says why sign-in failed never
 * appears, which is the failure mode this whole area keeps having.
 */
describe("the OAuth failure diagnostic", () => {
  test("is reachable where the library looks for it", () => {
    expect(typeof GitHubProvider[customFetch]).toBe("function");
  });

  test("is not left in `options`, where the merge would drop it", () => {
    const options = (GitHubProvider as { options?: Record<symbol, unknown> })
      .options;
    expect(options?.[customFetch]).toBeUndefined();
  });

  // The precise reason it would be dropped, pinned so the claim above is not
  // just a comment: this is the enumeration `merge` uses.
  test("would be invisible to a `for...in` merge of `options`", () => {
    const source: Record<string, unknown> = {};
    Object.assign(source, { [customFetch]: () => undefined });
    const copied: string[] = [];
    for (const key in source) copied.push(key);
    expect(copied).toEqual([]);
  });
});

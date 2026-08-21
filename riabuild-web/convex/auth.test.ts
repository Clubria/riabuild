import { describe, expect, test } from "vitest";
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

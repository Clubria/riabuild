import { describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { listOrgMembers } from "./github";
import { publishCliVersion } from "./release";
import { setup, stubFetch } from "./testing.fixtures";

/**
 * `release.publishCliVersion`: the action the release workflow calls, and why
 * no browser client may call it.
 *
 * Split out of the old `api.test.ts`.
 */

describe("announcing a release", () => {
  /** Stands in for api.github.com, and records how it was asked. */
  function stubGithub(options: {
    status: number;
    body?: unknown;
    rateLimitRemaining?: string;
  }) {
    const calls: { url: string; authorization: string | null }[] = [];
    stubFetch(async (input, init) => {
      const url = input instanceof Request ? input.url : input.toString();
      const headers = new Headers(init?.headers);
      calls.push({ url, authorization: headers.get("authorization") });
      return new Response(
        options.body === undefined ? null : JSON.stringify(options.body),
        {
          status: options.status,
          headers: {
            "x-ratelimit-remaining": options.rateLimitRemaining ?? "4999",
          },
        },
      );
    });
    return calls;
  }

  test("no browser client can announce a release", () => {
    // It used to be a public `action`, on the argument that it verifies
    // everything it stores: GitHub must really have a published, non-draft
    // release, and `setLatestCliVersion` refuses to move backwards. That
    // reasons about the value and says nothing about the call. Every call
    // spends a request from `GITHUB_ORG_TOKEN` — the same token
    // `checkOrgMembership` uses on every secret-brokering request — so an open
    // loop against it exhausts 5000/hr and riabuild starts failing closed for
    // the whole org.
    //
    // Asserted against the function's own visibility metadata, which is what
    // the deployment routes on. Calling it is no test at all: `convex-test`
    // resolves a reference by path and runs whatever it finds, so
    // `makeFunctionReference<"action">("release:publishCliVersion")` would
    // succeed against an `internalAction` too — while a real deployment keeps
    // public and internal in separate route spaces.
    expect(publishCliVersion.isInternal).toBe(true);
    // And the flag means something: a public action in the same codebase
    // carries the other one, so this is not a property every registered
    // function happens to have.
    expect(listOrgMembers.isPublic).toBe(true);

    // The same boundary, in the generated types: an `internalAction` is
    // absent from the public `api`. If the line below stops erroring under
    // `tsc -p convex`, the action is callable by anyone with the deployment
    // URL again — and `@ts-expect-error` says so by failing the build.
    //
    // A type-level read rather than a value one. `api` is a proxy that
    // resolves any path at runtime, so reading the property into a variable
    // asserted nothing the compiler was not already saying, and the value it
    // produced was `any` — which this file no longer keeps anywhere.
    // @ts-expect-error — see above.
    type _PubliclyReachable = (typeof api.release)["publishCliVersion"];
    // Still reachable the way CI calls it, with a deploy key.
    expect(internal.release.publishCliVersion).toBeDefined();
  });

  test("the release check is authenticated with the org token", async () => {
    // Not for permission — the repository is public and this read works
    // signed out. For the rate limit: unauthenticated api.github.com allows
    // 60 requests an hour per IP, and a Convex deployment shares its egress
    // addresses, so a signed-out check is refused for traffic riabuild never
    // made. That is what stranded v2026.08.12.1 on the shelf while every
    // machine kept installing the release before it.
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    const t = setup();
    const calls = stubGithub({ status: 200, body: { draft: false } });

    const result = await t.action(internal.release.publishCliVersion, {
      version: "2026.08.12.1",
    });

    expect(result).toEqual({ updated: true, latestCliVersion: "2026.08.12.1" });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toContain("/releases/tags/v2026.08.12.1");
    expect(calls[0].authorization).toBe("Bearer ghp_test");
  });

  test("a deployment with no org token still announces, unauthenticated", async () => {
    // The token buys headroom, not permission. Losing the ability to announce
    // a release without one would trade a rate limit for a manual step.
    vi.stubEnv("GITHUB_ORG_TOKEN", "");
    const t = setup();
    const calls = stubGithub({ status: 200, body: { draft: false } });

    await t.action(internal.release.publishCliVersion, {
      version: "2026.08.12.1",
    });

    expect(calls[0].authorization).toBeNull();
  });

  test("a rate-limited refusal says so instead of reading as forbidden", async () => {
    // The message the failing run left was "api.github.com returned 403",
    // which sends whoever reads it looking for a permission they never
    // lacked. The version must also stay put: an unverified release is not
    // one to offer every developer.
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    const t = setup();
    stubGithub({ status: 403, rateLimitRemaining: "0" });

    await expect(
      t.action(internal.release.publishCliVersion, { version: "2026.08.12.1" }),
    ).rejects.toThrow(/rate limit for GITHUB_ORG_TOKEN is exhausted/i);

    expect((await t.query(internal.org.forApi, {})).latestCliVersion).toBe(
      "0.1.0",
    );
  });

  test("a release GitHub has never heard of is not announced", async () => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    const t = setup();
    stubGithub({ status: 404 });

    await expect(
      t.action(internal.release.publishCliVersion, { version: "2026.09.01" }),
    ).rejects.toThrow(/Cut the release before announcing it/i);
  });

  test("a draft release is not announced", async () => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    const t = setup();
    stubGithub({ status: 200, body: { draft: true } });

    await expect(
      t.action(internal.release.publishCliVersion, { version: "2026.09.01" }),
    ).rejects.toThrow(/still a draft/i);
  });
});

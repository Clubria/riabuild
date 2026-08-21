import { beforeEach, describe, expect, test, vi } from "vitest";
import { checkOrgMembership } from "./github";
import { fetchUpstream } from "./lib/http";
import { stubFetch } from "./testing.fixtures";

/**
 * What riabuild-web asks of the outside world: every upstream call carries a
 * deadline, and a deadline that fires says so rather than hanging a route.
 *
 * Split out of the old `api.test.ts`.
 */

describe("what riabuild-web asks of the outside world", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  test("an upstream call carries a deadline", async () => {
    let seen: RequestInit | undefined;
    stubFetch(async (_input, init) => {
      seen = init;
      return new Response(null, { status: 204 });
    });

    await fetchUpstream("https://api.github.com/orgs/Clubria/members/ada");

    expect(seen?.signal).toBeInstanceOf(AbortSignal);
    expect(seen?.signal?.aborted).toBe(false);
  });

  test("the deadline fires, and says that is what happened", async () => {
    // A bare `fetch` in a Convex action has no deadline at all: a slow
    // api.github.com holds the action open until the platform kills it, and
    // every CLI request queued behind that membership check waits with it.
    stubFetch(
      async (_input, init) =>
        await new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => {
            reject(init.signal?.reason as Error);
          });
        }),
    );

    await expect(
      fetchUpstream("https://api.github.com/orgs/Clubria/members/ada", {}, 5),
    ).rejects.toThrow(/timed out after 5ms/);
  });

  test("a hung GitHub is 'we could not check', not 'you are not a member'", async () => {
    // The point of bounding it. `unavailable` is what every route turns into a
    // 503, so riabuild fails closed and says why — rather than telling a
    // developer they were removed from the org, or hanging until something
    // upstream gives up first.
    stubFetch(
      async (_input, init) =>
        await new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => {
            reject(init.signal?.reason as Error);
          });
        }),
    );

    const result = await checkOrgMembership("ada", 5);
    expect(result.status).toBe("unavailable");
    expect(result.status === "unavailable" && result.detail).toMatch(
      /timed out/i,
    );
  });
});

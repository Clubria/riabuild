import { beforeEach, describe, expect, test, vi } from "vitest";
import {
  ApiError,
  bearer,
  issueSession,
  json,
  SecretsToken,
  seedMember,
  setup,
  stubFetch,
} from "./testing.fixtures";

/**
 * `POST /api/v1/secrets/token`: the org re-check, the universal-auth login
 * against Infisical, and the short-lived token that comes back.
 *
 * Split out of the old `api.test.ts`. `infisical.test.ts` covers the pure
 * role-to-environment mapping this endpoint carries.
 */

describe("secret brokering", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_ID", "client-id");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_SECRET", "client-secret");
    vi.stubEnv("INFISICAL_CANDIDATE_CLIENT_ID", "cand-id");
    vi.stubEnv("INFISICAL_CANDIDATE_CLIENT_SECRET", "cand-secret");
    vi.stubEnv("INFISICAL_PROJECT_ID", "proj_1");
  });

  /** Stands in for GitHub and Infisical so failure paths are reachable. */
  function stubUpstreams(options: {
    membership: number;
    infisical?: { status: number; body?: unknown };
    onLogin?: (body: unknown) => void;
  }) {
    stubFetch(async (input, init) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        return new Response(null, { status: options.membership });
      }
      if (url.includes("universal-auth/login")) {
        const body = typeof init?.body === "string" ? init.body : "{}";
        options.onLogin?.(JSON.parse(body));
        const spec = options.infisical ?? {
          status: 200,
          body: { accessToken: "inf_token", expiresIn: 300 },
        };
        return new Response(JSON.stringify(spec.body ?? {}), {
          status: spec.status,
        });
      }
      throw new Error(`unexpected fetch to ${url}`);
    });
  }

  test("an org member gets a short-lived token and an audit entry", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    let loginBody: unknown = null;
    stubUpstreams({ membership: 204, onLogin: (body) => (loginBody = body) });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect(response.status).toBe(200);
    const body = await json<SecretsToken>(response);
    expect(body.token).toBe("inf_token");
    expect(body.projectId).toBe("proj_1");
    expect(body.expiresAt).toBeGreaterThan(Date.now());
    expect(loginBody).toEqual({
      clientId: "client-id",
      clientSecret: "client-secret",
    });

    const actions = await t.run(async (ctx) => {
      const rows = await ctx.db.query("auditLog").collect();
      return rows.map((row) => row.action);
    });
    expect(actions).toContain("secrets.token_brokered");
  });

  test("a candidate is brokered through the narrower identity", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    const { token } = await issueSession(t, rowId);
    let loginBody: unknown = null;
    stubUpstreams({ membership: 204, onLogin: (body) => (loginBody = body) });

    await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect(loginBody).toEqual({
      clientId: "cand-id",
      clientSecret: "cand-secret",
    });
  });

  test("a developer is told to pull dev and staging", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ membership: 204 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    const body = await json<SecretsToken>(response);
    expect(body.environments).toEqual(["dev", "staging"]);
    // Still the base environment on its own, because a CLI released before
    // `environments` existed reads this field and nothing else.
    expect(body.environment).toBe("dev");
  });

  test("a candidate is told to pull dev alone", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ membership: 204 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect((await json<SecretsToken>(response)).environments).toEqual(["dev"]);
  });

  test("the audit entry records every environment that was brokered", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ membership: 204 });

    await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });

    const meta = await t.run(async (ctx) => {
      const rows = await ctx.db.query("auditLog").collect();
      return rows.find((row) => row.action === "secrets.token_brokered")?.meta;
    });
    // Which environments a credential opened is the part worth being able to
    // answer later; the single `environment` field cannot say "and staging".
    expect(meta?.environments).toBe("dev,staging");
  });

  test("leaving the GitHub org ends access, whatever Convex says", async () => {
    const t = setup();
    // Still `developer` and still `active` in Convex — GitHub is the gate.
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ membership: 404 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect(response.status).toBe(403);
    expect((await json<ApiError>(response)).error.code).toBe("not_org_member");
  });

  test("an unusable org token fails closed, and says it could not check", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    vi.stubEnv("GITHUB_ORG_TOKEN", "");
    stubUpstreams({ membership: 204 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    // Not "you were removed from the org" — that sends them to the wrong person.
    expect(response.status).toBe(503);
    expect((await json<ApiError>(response)).error.code).toBe(
      "org_check_unavailable",
    );
  });

  test("an Infisical outage is translated, not forwarded", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    stubUpstreams({
      membership: 204,
      infisical: { status: 500, body: { message: "identity mi-developer" } },
    });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect(response.status).toBe(503);
    const body = await json<ApiError>(response);
    expect(body.error.code).toBe("upstream_error");
    expect(JSON.stringify(body)).not.toContain("mi-developer");
  });
});

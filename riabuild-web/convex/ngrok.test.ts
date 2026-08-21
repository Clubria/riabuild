import { beforeEach, describe, expect, test, vi } from "vitest";
import { api } from "./_generated/api";
import {
  ApiError,
  bearer,
  currentVersion,
  issueSession,
  json,
  OrgConfigBody,
  seedMember,
  setup,
  stubMembership,
  TestConvex,
} from "./testing.fixtures";

/**
 * `GET /api/v1/org/ngrok-token`: the second response carrying a durable
 * credential, and the only attribution the team's single ngrok account has.
 *
 * Split out of the old `api.test.ts`.
 */

describe("the team's ngrok authtoken", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  async function setToken(t: TestConvex, token: string) {
    const { rowId, userId } = await seedMember(t, {
      login: "lead",
      role: "lead",
    });
    await t
      .withIdentity({ subject: userId })
      .mutation(api.org.update, { ngrokAuthToken: token });
    return rowId;
  }

  test("a member's CLI is served the token, and the fetch is audited", async () => {
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");
    const { rowId } = await seedMember(t, { login: "ada" });
    const { token: session } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(200);
    expect((await json<{ token: string }>(response)).token).toBe(
      "2abcDEF_the_org_token",
    );

    // ngrok sees one account for the whole team, so this row is the only
    // record of who opened a tunnel.
    const actions = await t.run(async (ctx) => {
      const rows = await ctx.db.query("auditLog").collect();
      return rows.map((row) => row.action);
    });
    expect(actions).toContain("org.ngrok_token_fetched");
  });

  test("a team whose lead has not set one gets a 404 the CLI can explain", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: session } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(404);
    // The code, not just the status: a route that does not exist is also a 404,
    // and the CLI rewords this one for the developer.
    expect((await json<ApiError>(response)).error.code).toBe("not_configured");
  });

  test("a developer removed from the GitHub org loses the tunnel today", async () => {
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");
    const { rowId } = await seedMember(t, { login: "ada" });
    const { token: session } = await issueSession(t, rowId);
    // Their Convex row still says active; GitHub is the identity.
    stubMembership(404);

    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(403);
  });

  test("a machine with no session is refused", async () => {
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");

    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: currentVersion,
    });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("unauthenticated");
  });

  test("only a lead can set it", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { role: "developer" });
    await expect(
      t
        .withIdentity({ subject: userId })
        .mutation(api.org.update, { ngrokAuthToken: "2abcDEF_someone_elses" }),
    ).rejects.toThrow();
  });

  test("the dashboard is shown a hint, never the token", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { role: "lead" });
    await t
      .withIdentity({ subject: userId })
      .mutation(api.org.update, { ngrokAuthToken: "2abcDEF_the_org_token" });

    const config = await t
      .withIdentity({ subject: userId })
      .query(api.org.get, {});
    expect(JSON.stringify(config)).not.toContain("2abcDEF_the_org_token");
    expect(config.ngrokAuthTokenHint).toBe("…oken");
    expect(config.ngrokAuthTokenUpdatedAt).toBeGreaterThan(0);
  });

  test("a developer is shown no hint at all", async () => {
    // Four characters of a live team credential, kept to the one screen that
    // has a use for them: the lead panel, where the person who pasted the
    // token recognises it. Everyone else sees what a team with no token set
    // sees.
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");
    const { userId } = await seedMember(t, {
      login: "ada",
      role: "developer",
    });

    const config = await t
      .withIdentity({ subject: userId })
      .query(api.org.get, {});
    expect(config.ngrokAuthTokenHint).toBe("");
    expect(JSON.stringify(config)).not.toContain("2abcDEF");
  });

  test("config says when the token was set, and never what it is", async () => {
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");
    const { rowId } = await seedMember(t, { login: "ada" });
    const { token: session } = await issueSession(t, rowId);

    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(session),
    });
    const body = await json<OrgConfigBody>(response);
    // The CLI reads this on every run to tell a developer their lead has not
    // set one — without brokering a live credential to find out.
    expect(body.ngrokAuthTokenUpdatedAt).toBeGreaterThan(0);
    expect(JSON.stringify(body)).not.toContain("2abcDEF_the_org_token");
  });

  test("clearing it puts the team back to unconfigured", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { role: "lead" });
    const identity = t.withIdentity({ subject: userId });
    await identity.mutation(api.org.update, { ngrokAuthToken: "2abcDEF_x" });
    await identity.mutation(api.org.update, { ngrokAuthToken: "" });

    const { rowId } = await seedMember(t, { login: "ada" });
    const { token: session } = await issueSession(t, rowId);
    stubMembership(204);
    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(404);
  });
});

import { describe, expect, test } from "vitest";
import {
  ApiError,
  bearer,
  currentVersion,
  issueSession,
  json,
  OrgConfigBody,
  seedMember,
  setup,
} from "./testing.fixtures";

/**
 * The prologue every `/api/v1` route runs before it answers: the version floor
 * first, then the session, then the member's standing.
 *
 * Split out of the old `api.test.ts`. It is asserted through `/api/v1/me`
 * because that route does nothing else — a failure here is the guard.
 */

describe("session authentication", () => {
  test("no bearer token is 401", async () => {
    // A current version and no session, so the answer is about the session.
    // Sending nothing at all is a 409 before authentication is reached — see
    // "the floor is checked before the session is" below, which pins that
    // ordering on purpose rather than as a side effect of an empty header bag.
    const t = setup();
    const response = await t.fetch("/api/v1/me", { headers: currentVersion });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("unauthenticated");
  });

  test("the floor is checked before the session is", async () => {
    // Deliberate, and the order six of the eight routes already used: a CLI
    // too old to be trusted is turned away whether or not it is signed in, and
    // an omitted version header is version `0` rather than an exemption.
    const t = setup();
    const response = await t.fetch("/api/v1/me", {});
    expect(response.status).toBe(409);
    expect((await json<ApiError>(response)).error.code).toBe("cli_too_old");
  });

  test("a revoked session says so, so the CLI can re-login", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId, { revoked: true });
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("session_revoked");
  });

  test("an expired session says so", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId, { expiresAt: 1 });
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("session_expired");
  });

  test("a suspended member is 403, never 401", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { status: "suspended" });
    const { token } = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    // 401 would make the CLI re-authenticate, succeed, and loop forever.
    expect(response.status).toBe(403);
    expect((await json<ApiError>(response)).error.code).toBe("suspended");
  });

  test("a successful request records when the machine was last seen", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    await t.fetch("/api/v1/me", { headers: bearer(token) });
    const lastUsed = await t.run(async (ctx) => {
      const session = await ctx.db.query("cliSessions").first();
      return session?.lastUsedAt ?? 0;
    });
    expect(lastUsed).toBeGreaterThan(0);
  });
});

describe("version floors", () => {
  test("an outdated CLI is turned away with 409", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "Clubria/ai-builders-hub",
        defaultProjectPath: "~/code/ai-builders-hub",
        minCliVersion: "2.0.0",
        latestCliVersion: "2.0.0",
        secretsUpdatedAt: 0,
      });
    });

    const me = await t.fetch("/api/v1/me", {
      headers: bearer(token, "1.9.9"),
    });
    expect(me.status).toBe(409);
    expect((await json<ApiError>(me)).error.code).toBe("cli_too_old");
  });

  test("org config still answers an outdated CLI — it is how it learns", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "Clubria/ai-builders-hub",
        defaultProjectPath: "~/code/ai-builders-hub",
        minCliVersion: "2.0.0",
        latestCliVersion: "2.4.0",
        secretsUpdatedAt: 0,
      });
    });

    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token, "1.9.9"),
    });
    expect(response.status).toBe(200);
    const body = await json<OrgConfigBody>(response);
    expect(body.minCliVersion).toBe("2.0.0");
    expect(body.latestCliVersion).toBe("2.4.0");
  });
});

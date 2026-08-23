import { beforeEach, describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { Id } from "./_generated/dataModel";
import { sha256Hex } from "./lib/crypto";
import {
  ApiError,
  bearer,
  CURRENT_VERSION as CURRENT,
  DeviceCodes,
  issueSession,
  json,
  seedMember,
  setup,
  stubFetch,
  stubMembership as stubGitHub,
  TestConvex,
  TokenGrant,
} from "./testing.fixtures";

/**
 * The `cliSessions` lifecycle from the Convex side: suspension reaching every
 * live row, expiry, the floor the guard applies, and the dashboard's own list.
 *
 * `sessionsApi.test.ts` covers the two `/api/v1` routes that mint and revoke
 * one; `apiGuard.test.ts` covers the prologue every other route runs.
 */

beforeEach(() => {
  vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  stubGitHub(204);
});

/* -------------------------------------------------------------------------- */
/* I041 — suspension reaches every session, not the first hundred              */
/* -------------------------------------------------------------------------- */

describe("suspension revokes live sessions", () => {
  test("all of them, not the first hundred", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "grace", role: "lead" });
    const subject = await seedMember(t, { login: "ada", role: "developer" });

    // Comfortably past the old `.take(100)`: 250 rows means a truncating
    // implementation leaves 150 live credentials behind.
    const total = 250;
    await t.run(async (ctx) => {
      for (let i = 0; i < total; i++) {
        await ctx.db.insert("cliSessions", {
          memberId: subject.rowId,
          tokenHash: `hash-${i}`,
          deviceLabel: `server-${i}`,
          cliVersion: "0.1.0",
          lastUsedAt: 0,
          expiresAt: Date.now() + 60_000,
        });
      }
    });

    await t
      .withIdentity({ subject: `${lead.userId}|session` })
      .mutation(api.members.setStatus, {
        memberId: subject.rowId,
        status: "suspended",
      });

    const stillLive = await t.run(async (ctx) => {
      const rows = await ctx.db
        .query("cliSessions")
        .withIndex("by_memberId", (q) => q.eq("memberId", subject.rowId))
        .collect();
      return rows.filter((row) => row.revokedAt === undefined).length;
    });
    expect(stillLive).toBe(0);
  });

  test("a session revoked earlier keeps the timestamp it was revoked at", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "grace", role: "lead" });
    const subject = await seedMember(t, { login: "ada" });
    const alreadyRevokedAt = 1_000;
    await t.run(async (ctx) => {
      await ctx.db.insert("cliSessions", {
        memberId: subject.rowId,
        tokenHash: "hash-old",
        deviceLabel: "retired-laptop",
        cliVersion: "0.1.0",
        lastUsedAt: 0,
        expiresAt: Date.now() + 60_000,
        revokedAt: alreadyRevokedAt,
      });
    });

    await t
      .withIdentity({ subject: `${lead.userId}|session` })
      .mutation(api.members.setStatus, {
        memberId: subject.rowId,
        status: "suspended",
      });

    const revokedAt = await t.run(async (ctx) => {
      const rows = await ctx.db
        .query("cliSessions")
        .withIndex("by_memberId", (q) => q.eq("memberId", subject.rowId))
        .collect();
      return rows[0].revokedAt;
    });
    expect(revokedAt).toBe(alreadyRevokedAt);
  });
});

/* -------------------------------------------------------------------------- */
/* I042 — delegate re-reads the parent's revocation state                      */
/* -------------------------------------------------------------------------- */

describe("delegation re-reads the parent", () => {
  test("a parent revoked after it authenticated cannot mint", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { sessionId } = await issueSession(t, rowId);

    // The window this closes: `authenticate` read the row, then the request
    // spent a GitHub round trip, and the revocation landed in between.
    await t.run(async (ctx) => {
      await ctx.db.patch("cliSessions", sessionId, { revokedAt: Date.now() });
    });

    const result = await t.mutation(internal.sessions.delegate, {
      parentSessionId: sessionId,
      tokenHash: await sha256Hex("child"),
      deviceLabel: "build-01",
      cliVersion: "0.1.0",
    });
    expect(result.status).toBe("revoked");

    const minted = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.filter((row) => row.origin === "delegated").length;
    });
    expect(minted).toBe(0);
  });

  test("a parent past its expiry cannot mint", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { sessionId } = await issueSession(t, rowId, {
      expiresAt: Date.now() - 1,
    });

    const result = await t.mutation(internal.sessions.delegate, {
      parentSessionId: sessionId,
      tokenHash: await sha256Hex("child"),
      deviceLabel: "build-01",
      cliVersion: "0.1.0",
    });
    expect(result.status).toBe("expired");
  });

  test("a live device session still mints, and the child is one hop deep", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { sessionId } = await issueSession(t, rowId);

    const result = await t.mutation(internal.sessions.delegate, {
      parentSessionId: sessionId,
      tokenHash: await sha256Hex("child"),
      deviceLabel: "build-01",
      cliVersion: "0.1.0",
    });
    expect(result.status).toBe("ok");

    const child = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.find((row) => row.origin === "delegated");
    });
    expect(child?.delegatedFrom).toBe(sessionId);
  });

  test("a delegated session still cannot mint a third", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { sessionId } = await issueSession(t, rowId, {
      origin: "delegated",
    });

    const result = await t.mutation(internal.sessions.delegate, {
      parentSessionId: sessionId,
      tokenHash: await sha256Hex("child"),
      deviceLabel: "build-01",
      cliVersion: "0.1.0",
    });
    expect(result.status).toBe("not_permitted");
  });

  test("the endpoint turns a revoked parent into a 401, not a 403", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token, sessionId } = await issueSession(t, rowId);

    // Revoked between `authenticate` and `delegate`: the GitHub stub is the
    // seam, so the patch lands mid-request exactly as a real revocation would.
    stubFetch(async (input) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        await t.run(async (ctx) => {
          await ctx.db.patch("cliSessions", sessionId, {
            revokedAt: Date.now(),
          });
        });
        return new Response(null, { status: 204 });
      }
      throw new Error(`unexpected fetch to ${url}`);
    });

    const response = await t.fetch("/api/v1/cli/sessions", {
      method: "POST",
      headers: bearer(token),
      body: JSON.stringify({ deviceLabel: "build-01" }),
    });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("session_revoked");
  });
});

/* -------------------------------------------------------------------------- */
/* I044 — /cli/token re-verifies org membership                                */
/* -------------------------------------------------------------------------- */

describe("signing in re-verifies the GitHub org", () => {
  /** Drives a device code all the way to the poll that mints a session. */
  async function approvedDeviceCode(t: TestConvex, userId: Id<"users">) {
    const start = await t.fetch("/api/v1/cli/device", {
      method: "POST",
      headers: { "x-riabuild-cli-version": CURRENT },
      body: JSON.stringify({ deviceLabel: "ada-mbp" }),
    });
    const device = await json<DeviceCodes>(start);
    await t
      .withIdentity({ subject: `${userId}|session` })
      .mutation(api.cliAuth.approve, { userCode: device.userCode });
    return device.deviceCode;
  }

  test("somebody removed from the org gets no session out of their own approval", async () => {
    const t = setup();
    // Still `developer` and still `active` in Convex — GitHub is the gate.
    const { userId, rowId } = await seedMember(t, { role: "developer" });
    const deviceCode = await approvedDeviceCode(t, userId);
    stubGitHub(404);

    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ deviceCode }),
    });
    expect(response.status).toBe(403);
    const body = await json<ApiError & { token?: string }>(response);
    expect(body.error.code).toBe("not_org_member");
    expect(body.token).toBeUndefined();

    // And no live row is left behind for the ninety days that would otherwise
    // follow: the token was minted inside the handler and discarded with it.
    const live = await t.run(async (ctx) => {
      const rows = await ctx.db
        .query("cliSessions")
        .withIndex("by_memberId", (q) => q.eq("memberId", rowId))
        .collect();
      return rows.filter((row) => row.revokedAt === undefined).length;
    });
    expect(live).toBe(0);
  });

  test("an unreachable GitHub fails closed and says it could not check", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const deviceCode = await approvedDeviceCode(t, userId);
    vi.stubEnv("GITHUB_ORG_TOKEN", "");

    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ deviceCode }),
    });
    // Not "you were removed from the org" — that sends them to the wrong person.
    expect(response.status).toBe(503);
    expect((await json<ApiError>(response)).error.code).toBe(
      "org_check_unavailable",
    );
  });

  test("an org member still gets their token", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const deviceCode = await approvedDeviceCode(t, userId);

    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ deviceCode }),
    });
    expect(response.status).toBe(200);
    const body = await json<TokenGrant>(response);
    expect(body.status).toBe("ok");
    expect(typeof body.token).toBe("string");
    expect(body.sessionId).toBeDefined();
  });

  test("polling before approval never reaches GitHub", async () => {
    const t = setup();
    await seedMember(t);
    const start = await t.fetch("/api/v1/cli/device", {
      method: "POST",
      headers: { "x-riabuild-cli-version": CURRENT },
      body: JSON.stringify({ deviceLabel: "ada-mbp" }),
    });
    const device = await json<DeviceCodes>(start);
    // A membership check on every tick of a poll loop would be dozens of
    // GitHub calls per login. `stubGitHub` throws on any other host, and the
    // pending branch returns before reaching it.
    stubFetch(async () => {
      throw new Error("no upstream call belongs on a pending poll");
    });

    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ deviceCode: device.deviceCode }),
    });
    expect(response.status).toBe(200);
    expect((await json<TokenGrant>(response)).status).toBe("pending");
  });
});

/* -------------------------------------------------------------------------- */
/* I015 — an absent version header is version 0, not an exemption              */
/* -------------------------------------------------------------------------- */

describe("the version floor", () => {
  async function withFloor(t: TestConvex, minCliVersion: string) {
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion,
        latestCliVersion: minCliVersion,
        secretsUpdatedAt: 0,
      });
    });
  }

  const enforcing = [
    "/api/v1/me",
    "/api/v1/org/claude-settings",
    "/api/v1/org/ngrok-token",
    "/api/v1/remotes/shared",
    "/api/v1/issued-keys",
  ];

  test.each(enforcing)(
    "%s refuses a client that sends no version at all",
    async (path) => {
      const t = setup();
      const { rowId } = await seedMember(t);
      const { token } = await issueSession(t, rowId);
      await withFloor(t, "2.0.0");

      const response = await t.fetch(path, { headers: bearer(token, null) });
      expect(response.status).toBe(409);
      expect((await json<ApiError>(response)).error.code).toBe("cli_too_old");
    },
  );

  test("so does the unauthenticated device endpoint", async () => {
    const t = setup();
    await withFloor(t, "2.0.0");

    const response = await t.fetch("/api/v1/cli/device", {
      method: "POST",
      body: JSON.stringify({ deviceLabel: "ada-mbp" }),
    });
    expect(response.status).toBe(409);
    expect((await json<ApiError>(response)).error.code).toBe("cli_too_old");
  });

  test("the message says the version was missing rather than naming one", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    await withFloor(t, "2.0.0");

    const body = await json<ApiError>(
      await t.fetch("/api/v1/me", { headers: bearer(token, null) }),
    );
    expect(body.error.message).toMatch(/did not say which version/i);
    expect(body.error.message).toContain("2.0.0");
  });

  test("a team that has set no floor still takes an unversioned client", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    await withFloor(t, "0.0.0");

    const response = await t.fetch("/api/v1/me", {
      headers: bearer(token, null),
    });
    expect(response.status).toBe(200);
  });

  test("/org/config still answers, because it is how a CLI learns the floor", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    await withFloor(t, "2.0.0");

    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token, null),
    });
    expect(response.status).toBe(200);
    expect(
      (await json<{ minCliVersion: string }>(response)).minCliVersion,
    ).toBe("2.0.0");
  });

  test("/cli/token still answers, because it is how a CLI signs in to be told", async () => {
    const t = setup();
    await seedMember(t);
    // No version header, and no floor could turn this into a 409.
    await withFloor(t, "2.0.0");

    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ deviceCode: "nope" }),
    });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("unauthenticated");
  });

  test("revoking a session is never blocked by the floor", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    const other = await issueSession(t, rowId, { deviceLabel: "build-01" });
    await withFloor(t, "2.0.0");

    // `riabuild remote forget` is how a leaked ninety-day credential gets
    // pulled. Refusing it over a version number would block the one command
    // that has to work on whatever build the developer happens to have.
    const response = await t.fetch(`/api/v1/cli/sessions/${other.sessionId}`, {
      method: "DELETE",
      headers: bearer(token, null),
    });
    expect(response.status).toBe(200);
  });
});

/* -------------------------------------------------------------------------- */
/* I087 — one guard, and the org check is on every route that hands out access */
/* -------------------------------------------------------------------------- */

describe("the shared auth prologue", () => {
  const everyGuardedRoute: Array<[string, RequestInit]> = [
    ["/api/v1/me", {}],
    ["/api/v1/org/config", {}],
    ["/api/v1/org/claude-settings", {}],
    ["/api/v1/org/ngrok-token", {}],
    ["/api/v1/remotes/shared", {}],
    ["/api/v1/issued-keys", {}],
    ["/api/v1/secrets/token", { method: "POST" }],
    ["/api/v1/cli/sessions", { method: "POST", body: "{}" }],
  ];

  test.each(everyGuardedRoute)(
    "%s rejects a revoked session",
    async (path, init) => {
      const t = setup();
      const { rowId } = await seedMember(t);
      const { token } = await issueSession(t, rowId, { revoked: true });

      const response = await t.fetch(path, { ...init, headers: bearer(token) });
      expect(response.status).toBe(401);
      expect((await json<ApiError>(response)).error.code).toBe(
        "session_revoked",
      );
    },
  );

  test.each(everyGuardedRoute)(
    "%s rejects a suspended member",
    async (path, init) => {
      const t = setup();
      const { rowId } = await seedMember(t, { status: "suspended" });
      const { token } = await issueSession(t, rowId);

      const response = await t.fetch(path, { ...init, headers: bearer(token) });
      expect(response.status).toBe(403);
      expect((await json<ApiError>(response)).error.code).toBe("suspended");
    },
  );

  /**
   * The routes that hand out access, as opposed to the two that describe it.
   * `/me` and the two `/org/*` config reads broker nothing, so they do not pay
   * for a GitHub round trip on every `riabuild --check`.
   */
  const brokering: Array<[string, RequestInit]> = [
    ["/api/v1/org/ngrok-token", {}],
    ["/api/v1/remotes/shared", {}],
    ["/api/v1/issued-keys", {}],
    ["/api/v1/secrets/token", { method: "POST" }],
    ["/api/v1/cli/sessions", { method: "POST", body: "{}" }],
  ];

  test.each(brokering)(
    "%s stops answering someone who left the GitHub org",
    async (path, init) => {
      const t = setup();
      // Still `developer` and still `active` in Convex — GitHub is the gate.
      const { rowId } = await seedMember(t, { role: "developer" });
      const { token } = await issueSession(t, rowId);
      stubGitHub(404);

      const response = await t.fetch(path, { ...init, headers: bearer(token) });
      expect(response.status).toBe(403);
      expect((await json<ApiError>(response)).error.code).toBe(
        "not_org_member",
      );
    },
  );

  test("DELETE /cli/sessions/<id> re-verifies the org too", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    const other = await issueSession(t, rowId, { deviceLabel: "build-01" });
    stubGitHub(404);

    const response = await t.fetch(`/api/v1/cli/sessions/${other.sessionId}`, {
      method: "DELETE",
      headers: bearer(token),
    });
    expect(response.status).toBe(403);
    expect((await json<ApiError>(response)).error.code).toBe("not_org_member");
  });

  test("a missing Authorization header is 401 everywhere", async () => {
    const t = setup();
    for (const [path, init] of everyGuardedRoute) {
      const response = await t.fetch(path, {
        ...init,
        headers: { "x-riabuild-cli-version": CURRENT },
      });
      expect([path, response.status]).toEqual([path, 401]);
    }
  });
});

/* -------------------------------------------------------------------------- */
/* I045 — dead session rows are reaped                                         */
/* -------------------------------------------------------------------------- */

describe("reaping dead sessions", () => {
  test("expired rows past the grace period are deleted, live ones are not", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const now = Date.now();
    await t.run(async (ctx) => {
      const rows: Array<[string, number]> = [
        ["long-dead", now - 100 * 24 * 60 * 60 * 1000],
        ["just-expired", now - 1_000],
        ["live", now + 60_000],
      ];
      for (const [label, expiresAt] of rows) {
        await ctx.db.insert("cliSessions", {
          memberId: rowId,
          tokenHash: `hash-${label}`,
          deviceLabel: label,
          cliVersion: "0.1.0",
          lastUsedAt: 0,
          expiresAt,
        });
      }
    });

    const result = await t.mutation(internal.sessions.reapDead, {});
    expect(result.deleted).toBe(1);

    const left = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.map((row) => row.deviceLabel).sort();
    });
    // "just-expired" survives the hour of grace: a request already in flight
    // against it must still find the row and be told `session_expired`.
    expect(left).toEqual(["just-expired", "live"]);
  });

  test("a revoked but unexpired session is left where a developer can see it", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    await issueSession(t, rowId, { revoked: true, deviceLabel: "stolen" });

    await t.mutation(internal.sessions.reapDead, {});

    const left = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.map((row) => row.deviceLabel);
    });
    expect(left).toEqual(["stolen"]);
  });

  test("an empty table reaps nothing and does not throw", async () => {
    const t = setup();
    expect(await t.mutation(internal.sessions.reapDead, {})).toEqual({
      deleted: 0,
    });
  });
});

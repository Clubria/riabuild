import { beforeEach, describe, expect, test, vi } from "vitest";
import { sha256Hex } from "./lib/crypto";
import {
  ApiError,
  bearer,
  DelegatedSession,
  issueSession,
  json,
  seedMember,
  setup,
  stubFetch,
  stubMembership,
  TestConvex,
} from "./testing.fixtures";

/**
 * The two `/api/v1` routes that change what a session can do: the one a laptop
 * uses to mint a server's session without a second browser approval, and the
 * one that revokes any of them.
 *
 * Split out of the old `api.test.ts`. `sessions.test.ts` next door covers the
 * lifecycle from the Convex side — suspension reaching every row, expiry, the
 * dashboard's own list.
 */

describe("signing a server in from the laptop", () => {
  // Delegation hands out a live 90-day credential, so it re-verifies GitHub
  // org membership exactly the way /secrets/token does. Stub GitHub as
  // reachable and membership as current unless a test says otherwise.
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    stubMembership(204);
  });

  function delegate(
    t: TestConvex,
    token: string,
    body: unknown = { deviceLabel: "build-01.fly.dev" },
    version?: string,
  ) {
    return t.fetch("/api/v1/cli/sessions", {
      method: "POST",
      headers: bearer(token, version),
      body: JSON.stringify(body),
    });
  }

  test("a laptop mints the server's session without a second browser approval", async () => {
    // The whole point: `riabuild remote` used to run a *second* device-code
    // flow on a laptop that was already signed in, so setting up a server cost
    // the developer two trips to riabuild.clubria.com/cli. The laptop asks
    // instead.
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId, {
      deviceLabel: "ada-mbp",
    });

    const response = await delegate(t, laptop);
    expect(response.status).toBe(200);
    const body = await json<DelegatedSession>(response);
    expect(typeof body.token).toBe("string");
    expect(body.token).not.toBe(laptop);
    expect(body.expiresAt).toBeGreaterThan(Date.now());
    expect(body.member.githubLogin).toBe("ada");

    // The minted token is a working session in its own right — this is what
    // the server's own riabuild runs as.
    const asServer = await t.fetch("/api/v1/me", {
      headers: bearer(body.token),
    });
    expect(asServer.status).toBe(200);

    // Labelled after the server, so the dashboard lists it as its own
    // revocable device rather than a second copy of the laptop.
    const labels = await t.run(async (ctx) =>
      (await ctx.db.query("cliSessions").collect()).map((r) => r.deviceLabel),
    );
    expect(labels).toEqual(
      expect.arrayContaining(["ada-mbp", "build-01.fly.dev"]),
    );

    // No device code was created. If one were, this endpoint would be the
    // browser flow wearing a different hat.
    expect(
      await t.run(
        async (ctx) => await ctx.db.query("cliDeviceCodes").collect(),
      ),
    ).toHaveLength(0);
  });

  test("the returned session id is the one that revokes it", async () => {
    // `riabuild remote forget` names this exact row through
    // `DELETE /api/v1/cli/sessions/<id>`. A session it cannot name is a live
    // credential on a shared box that nothing can take back.
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId);
    const { token, sessionId } = await json<DelegatedSession>(
      await delegate(t, laptop),
    );

    const revoked = await t.fetch(`/api/v1/cli/sessions/${sessionId}`, {
      method: "DELETE",
      headers: bearer(laptop),
    });
    expect(revoked.status).toBe(200);
    expect(
      (await t.fetch("/api/v1/me", { headers: bearer(token) })).status,
    ).toBe(401);
  });

  test("the minted token is stored hashed, never in the clear", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId);
    const { token } = await json<DelegatedSession>(await delegate(t, laptop));

    const rows = await t.run(
      async (ctx) => await ctx.db.query("cliSessions").collect(),
    );
    const hash = await sha256Hex(token);
    expect(rows.some((row) => row.tokenHash === token)).toBe(false);
    expect(rows.some((row) => row.tokenHash === hash)).toBe(true);
  });

  test("a delegated session cannot delegate again", async () => {
    // One hop. A server's token is readable by every co-tenant sharing that
    // Unix account; if it could mint, any of them could manufacture fresh
    // 90-day credentials that outlive `riabuild remote forget` — which is
    // precisely the guarantee the on-disk token rests on.
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId);
    const { token: server } = await json<DelegatedSession>(
      await delegate(t, laptop),
    );

    const again = await delegate(t, server, { deviceLabel: "build-02" });
    expect(again.status).toBe(403);
    expect((await json<ApiError>(again)).error.code).toBe(
      "delegation_not_permitted",
    );

    // Refused, not merely unreported: no third session came into existence.
    expect(
      await t.run(async (ctx) => await ctx.db.query("cliSessions").collect()),
    ).toHaveLength(2);
  });

  test("leaving the GitHub org ends delegation, whatever Convex says", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId);
    stubFetch(async () => new Response(null, { status: 404 }));

    const response = await delegate(t, laptop);
    expect(response.status).toBe(403);
    expect((await json<ApiError>(response)).error.code).toBe("not_org_member");
    expect(
      await t.run(async (ctx) => await ctx.db.query("cliSessions").collect()),
    ).toHaveLength(1);
  });

  test("a revoked laptop session mints nothing", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId, { revoked: true });
    expect((await delegate(t, laptop)).status).toBe(401);
  });

  test("a suspended member mints nothing, and is told to stop rather than retry", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { status: "suspended" });
    const { token: laptop } = await issueSession(t, rowId);
    const response = await delegate(t, laptop);
    // 403, not 401: signing in again would succeed and change nothing.
    expect(response.status).toBe(403);
    expect((await json<ApiError>(response)).error.code).toBe("suspended");
  });

  test("delegation is written to the audit log", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId);
    await delegate(t, laptop);

    const entry = await t.run(async (ctx) =>
      (await ctx.db.query("auditLog").collect()).find(
        (row) => row.action === "cli.session_delegated",
      ),
    );
    expect(entry?.actorId).toBe(rowId);
    expect(entry?.meta?.deviceLabel).toBe("build-01.fly.dev");
  });

  test("a malformed body is a 400, not a 500", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId);
    const response = await delegate(t, laptop, { deviceLabel: 42 });
    expect(response.status).toBe(400);
    expect((await json<ApiError>(response)).error.code).toBe("bad_request");
  });

  test("a CLI below the floor is told to upgrade rather than handed a token", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: laptop } = await issueSession(t, rowId);
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

    const response = await delegate(
      t,
      laptop,
      { deviceLabel: "build-01" },
      "1.9.9",
    );
    expect(response.status).toBe(409);
    expect((await json<ApiError>(response)).error.code).toBe("cli_too_old");
  });
});

describe("revoking a session", () => {
  // This endpoint re-verifies GitHub org membership same as /secrets/token —
  // revocation changes access, so the Convex row is never the sole gate here
  // either. Stub GitHub as reachable and membership as current.
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    stubMembership(204);
  });

  test("a member can revoke their own session", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: caller } = await issueSession(t, rowId);
    const { token: victim } = await issueSession(t, rowId, {
      deviceLabel: "build-01.fly.dev",
    });
    const victimId = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      const found = rows.find((row) => row.deviceLabel === "build-01.fly.dev");
      return found?._id;
    });

    const response = await t.fetch(`/api/v1/cli/sessions/${victimId}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(response.status).toBe(200);
    expect(await json<{ revoked: boolean }>(response)).toEqual({
      revoked: true,
    });

    const revoked = await t.run(
      async (ctx) => await ctx.db.get("cliSessions", victimId!),
    );
    expect(revoked?.revokedAt).toBeTruthy();

    // The revoked token is dead everywhere, not just on the laptop that held it.
    const after = await t.fetch("/api/v1/me", { headers: bearer(victim) });
    expect(after.status).toBe(401);

    const actions = await t.run(async (ctx) =>
      (await ctx.db.query("auditLog").collect()).map((row) => row.action),
    );
    expect(actions).toContain("session.revoked");
  });

  // A session id that belongs to somebody else must read identically to one
  // that never existed at all — see the next test. If revoking somebody
  // else's session returned a distinct status (e.g. 403), a caller could
  // enumerate live session ids one guess at a time by watching which ones
  // come back "forbidden" instead of "not found". So this is 404, not 403,
  // and the two tests below assert the response bodies are indistinguishable.
  test("a developer cannot revoke somebody else's session, and it looks the same as not existing", async () => {
    const t = setup();
    const { rowId: mine } = await seedMember(t);
    const { rowId: theirs } = await seedMember(t, { login: "bob" });
    const { token: caller } = await issueSession(t, mine);
    const { token: other } = await issueSession(t, theirs);
    const otherId = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.find((row) => row.memberId === theirs)?._id;
    });

    const response = await t.fetch(`/api/v1/cli/sessions/${otherId}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(response.status).toBe(404);
    expect((await json<ApiError>(response)).error.code).toBe("session_unknown");
    expect(
      await t.run(
        async (ctx) => (await ctx.db.get("cliSessions", otherId!))?.revokedAt,
      ),
    ).toBeFalsy();

    // The victim's session survives untouched — the failed attempt didn't
    // revoke it, and it still authenticates.
    const stillLive = await t.fetch("/api/v1/me", { headers: bearer(other) });
    expect(stillLive.status).toBe(200);
  });

  test("an unknown session id gets the identical 404 as somebody else's session", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: caller } = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/cli/sessions/not-a-real-id", {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(response.status).toBe(404);
    expect(await json<ApiError>(response)).toEqual({
      error: {
        code: "session_unknown",
        message: "That session no longer exists.",
        action: "Run `riabuild remote list` to see what is left.",
      },
    });
  });

  test("an org lead can revoke somebody else's session", async () => {
    const t = setup();
    const { rowId: leadRow } = await seedMember(t, {
      login: "lead",
      role: "lead",
    });
    const { rowId: devRow } = await seedMember(t, { login: "bob" });
    const { token: caller } = await issueSession(t, leadRow);
    const { token: victim } = await issueSession(t, devRow);
    const victimId = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.find((row) => row.memberId === devRow)?._id;
    });

    const response = await t.fetch(`/api/v1/cli/sessions/${victimId}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(response.status).toBe(200);

    const after = await t.fetch("/api/v1/me", { headers: bearer(victim) });
    expect(after.status).toBe(401);

    const revocation = await t.run(async (ctx) =>
      (await ctx.db.query("auditLog").collect()).find(
        (row) => row.action === "session.revoked",
      ),
    );
    expect(revocation?.actorId).toBe(leadRow);
    expect(revocation?.subjectId).toBe(devRow);
  });

  test("revoking your own currently-authenticating session is not an error, and kills it for real", async () => {
    // `apply()` runs twice; so does `forget` after a half-finished one.
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token: caller } = await issueSession(t, rowId);
    const target = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows[0]?._id;
    });

    const once = await t.fetch(`/api/v1/cli/sessions/${target}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(once.status).toBe(200);

    // The caller just revoked the session it is calling with, so a second
    // attempt authenticates as nobody — 401, not a 500. It never reaches the
    // idempotency check inside the mutation, because it never reaches the
    // mutation at all.
    const twice = await t.fetch(`/api/v1/cli/sessions/${target}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(twice.status).toBe(401);
  });

  test("revoking an already-revoked session through a live caller is a no-op, not an error", async () => {
    // Exercises the mutation's own idempotency, using a caller (a lead) whose
    // token survives the first call — unlike the self-revoke case above,
    // where the second call never reaches the mutation.
    const t = setup();
    const { rowId: leadRow } = await seedMember(t, {
      login: "lead",
      role: "lead",
    });
    const { rowId: devRow } = await seedMember(t, { login: "bob" });
    const { token: caller } = await issueSession(t, leadRow);
    await issueSession(t, devRow);
    const targetId = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.find((row) => row.memberId === devRow)?._id;
    });

    const once = await t.fetch(`/api/v1/cli/sessions/${targetId}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(once.status).toBe(200);
    const revokedAtFirst = await t.run(
      async (ctx) => (await ctx.db.get("cliSessions", targetId!))?.revokedAt,
    );

    const twice = await t.fetch(`/api/v1/cli/sessions/${targetId}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(twice.status).toBe(200);
    const revokedAtSecond = await t.run(
      async (ctx) => (await ctx.db.get("cliSessions", targetId!))?.revokedAt,
    );
    // Not re-stamped with a later timestamp — a no-op, not a second write.
    expect(revokedAtSecond).toBe(revokedAtFirst);

    const revocations = await t.run(async (ctx) =>
      (await ctx.db.query("auditLog").collect()).filter(
        (row) => row.action === "session.revoked",
      ),
    );
    expect(revocations).toHaveLength(1);
  });
});

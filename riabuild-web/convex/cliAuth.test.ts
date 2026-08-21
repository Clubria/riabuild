import { beforeEach, describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { randomToken, sha256Hex } from "./lib/crypto";
import {
  ApiError,
  bearer,
  currentVersion,
  DeviceCodes,
  json,
  MemberPayload,
  seedMember,
  setup,
  stubMembership,
  TestConvex,
  TokenGrant,
} from "./testing.fixtures";

/**
 * The device-authorisation flow: the pair of codes the CLI prints, the browser
 * approval, and the poll loop that turns the two into a session.
 *
 * Split out of the old `api.test.ts`. `sessions.test.ts` covers what happens
 * to a session afterwards; this file stops at the moment one is minted.
 */

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

describe("CLI login — device authorisation", () => {
  /** What the CLI does first: ask for a pair of codes. */
  async function startDevice<Body = DeviceCodes>(
    t: TestConvex,
    options: { label?: string; version?: string } = {},
  ) {
    const response = await t.fetch("/api/v1/cli/device", {
      method: "POST",
      // Always sent, for the reason `bearer` gives: an absent version header
      // is version `0` now, and `/cli/device` enforces the floor before it
      // hands out anything.
      headers: { "x-riabuild-cli-version": options.version ?? "9999.0.0" },
      body: JSON.stringify({ deviceLabel: options.label ?? "build-01" }),
    });
    return { response, body: await json<Body>(response) };
  }

  // Redeeming a device code mints a live session, so `/cli/token` re-verifies
  // GitHub org membership like every other route that hands out access — the
  // Convex row is never the sole gate. Stub GitHub as reachable and membership
  // as current unless a test says otherwise.
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    stubMembership(204);
  });

  /** One tick of the CLI's poll loop. */
  async function poll<Body = TokenGrant>(t: TestConvex, deviceCode: string) {
    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      headers: currentVersion,
      body: JSON.stringify({ deviceCode }),
    });
    return { response, body: await json<Body>(response) };
  }

  test("prints a code, waits, and signs in once it is approved", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const { body: device } = await startDevice(t, { label: "build-01" });

    expect(device.userCode).toMatch(
      /^[BCDFGHJKMNPQRSTVWXZ]{4}-[BCDFGHJKMNPQRSTVWXZ]{4}$/,
    );
    expect(device.verificationUriComplete).toContain(device.userCode);
    expect(device.interval).toBeGreaterThan(0);

    // Nothing has happened yet, so the CLI is told to keep waiting rather than
    // handed an error to unwind on every tick of its loop.
    const waiting = await poll(t, device.deviceCode);
    expect(waiting.response.status).toBe(200);
    expect(waiting.body.status).toBe("pending");

    const asAda = t.withIdentity({ subject: `${userId}|session` });
    const seen = await asAda.query(api.cliAuth.deviceRequest, {
      userCode: device.userCode,
    });
    // The developer checks this against their own terminal before approving.
    expect(seen).toMatchObject({ status: "pending", deviceLabel: "build-01" });

    expect(
      await asAda.mutation(api.cliAuth.approve, { userCode: device.userCode }),
    ).toEqual({ status: "ok" });

    const granted = await poll(t, device.deviceCode);
    expect(granted.response.status).toBe(200);
    expect(granted.body.status).toBe("ok");
    expect(granted.body.member.githubLogin).toBe("ada");
    expect(typeof granted.body.token).toBe("string");
    expect(granted.body.member.memberId).toMatch(UUID);

    // `riabuild remote forget` needs this to name the exact session it is
    // revoking: it must be the real row id, not just present.
    const sessionRowId = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows[0]?._id;
    });
    expect(granted.body.sessionId).toBe(sessionRowId);

    // The session is real: it authenticates the next request.
    const me = await t.fetch("/api/v1/me", {
      headers: bearer(granted.body.token),
    });
    expect(me.status).toBe(200);
    expect((await json<{ member: MemberPayload }>(me)).member.role).toBe(
      "developer",
    );

    // And it was stored hashed, not raw.
    const stored = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.map((row) => row.tokenHash);
    });
    expect(stored).toHaveLength(1);
    expect(stored[0]).not.toBe(granted.body.token);
    expect(stored[0]).toBe(await sha256Hex(granted.body.token));
  });

  test("the device code is stored hashed and the user code is not", async () => {
    const t = setup();
    const { body: device } = await startDevice(t);

    const row = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliDeviceCodes").collect();
      return rows[0];
    });

    // The device code is the one that can be exchanged for a session, so a
    // dump of this table must not contain it.
    expect(row.deviceCodeHash).not.toBe(device.deviceCode);
    expect(row.deviceCodeHash).toBe(await sha256Hex(device.deviceCode));

    // The user code identifies a request and grants nothing, so it is stored
    // as-is — hashing it would only stop the dashboard from looking it up.
    expect(row.userCode).toBe(device.userCode.replace("-", ""));
  });

  test("a lowercase, dashless retype finds the same request", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const { body: device } = await startDevice(t);
    const asAda = t.withIdentity({ subject: `${userId}|session` });

    const typed = device.userCode.replace("-", "").toLowerCase();
    expect(
      await asAda.query(api.cliAuth.deviceRequest, { userCode: typed }),
    ).toMatchObject({ status: "pending" });
  });

  test("a denied request stops the CLI instead of stranding it", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const { body: device } = await startDevice(t);

    const asAda = t.withIdentity({ subject: `${userId}|session` });
    await asAda.mutation(api.cliAuth.deny, { userCode: device.userCode });

    // 200, not an error: "no" is an answer, and the CLI needs to tell the
    // difference between a refusal and a network problem.
    const denied = await poll(t, device.deviceCode);
    expect(denied.response.status).toBe(200);
    expect(denied.body.status).toBe("denied");

    const sessions = await t.run(async (ctx) =>
      ctx.db.query("cliSessions").collect(),
    );
    expect(sessions).toHaveLength(0);
  });

  test("a device code is single-use", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const { body: device } = await startDevice(t);
    const asAda = t.withIdentity({ subject: `${userId}|session` });
    await asAda.mutation(api.cliAuth.approve, { userCode: device.userCode });

    expect((await poll(t, device.deviceCode)).body.status).toBe("ok");

    const replay = await poll(t, device.deviceCode);
    expect(replay.response.status).toBe(401);
    const sessions = await t.run(async (ctx) =>
      ctx.db.query("cliSessions").collect(),
    );
    expect(sessions).toHaveLength(1);
  });

  test("an expired request cannot be approved or redeemed", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const { body: device } = await startDevice(t);

    await t.run(async (ctx) => {
      const row = (await ctx.db.query("cliDeviceCodes").collect())[0];
      await ctx.db.patch("cliDeviceCodes", row._id, { expiresAt: 1 });
    });

    const asAda = t.withIdentity({ subject: `${userId}|session` });
    expect(
      await asAda.query(api.cliAuth.deviceRequest, {
        userCode: device.userCode,
      }),
    ).toEqual({ status: "expired" });
    expect(
      await asAda.mutation(api.cliAuth.approve, { userCode: device.userCode }),
    ).toEqual({ status: "expired" });
    expect((await poll(t, device.deviceCode)).response.status).toBe(401);
  });

  test("an unknown device code is refused", async () => {
    const t = setup();
    const unknown = await poll<ApiError>(t, randomToken(32));
    expect(unknown.response.status).toBe(401);
    expect(unknown.body.error.code).toBe("unauthenticated");
  });

  test("a suspended member cannot approve a machine", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { status: "suspended" });
    const { body: device } = await startDevice(t);
    const asAda = t.withIdentity({ subject: `${userId}|session` });

    await expect(
      asAda.mutation(api.cliAuth.approve, { userCode: device.userCode }),
    ).rejects.toThrow(/suspended/i);
  });

  test("suspension between approval and the next poll is a 403", async () => {
    const t = setup();
    const { userId, rowId } = await seedMember(t);
    const { body: device } = await startDevice(t);
    const asAda = t.withIdentity({ subject: `${userId}|session` });
    await asAda.mutation(api.cliAuth.approve, { userCode: device.userCode });

    // Minutes can pass between approving and the poll that lands, and the
    // session comes into existence at the poll — so that is where status has
    // to be checked, not only at approval.
    await t.run(async (ctx) => {
      await ctx.db.patch("members", rowId, { status: "suspended" });
    });

    const blocked = await poll<ApiError>(t, device.deviceCode);
    expect(blocked.response.status).toBe(403);
    expect(blocked.body.error.code).toBe("suspended");
  });

  test("looking a code up requires signing in first", async () => {
    const t = setup();
    const { body: device } = await startDevice(t);
    // Otherwise this is an oracle anyone can walk the code space with.
    await expect(
      t.query(api.cliAuth.deviceRequest, { userCode: device.userCode }),
    ).rejects.toThrow(/signed in/i);
  });

  test("a CLI below the floor is told to upgrade before it prints anything", async () => {
    const t = setup();
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "clubria/app",
        minCliVersion: "2026.08.07",
        latestCliVersion: "2026.08.07",
        secretsUpdatedAt: 0,
      });
    });

    // This endpoint is the only place the floor reaches a machine that has
    // never signed in — /org/config needs a session it does not have yet.
    const { response, body } = await startDevice<ApiError>(t, {
      version: "2026.08.01",
    });
    expect(response.status).toBe(409);
    expect(body.error.code).toBe("cli_too_old");
  });

  test("a malformed body is a 400, not a 500", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ deviceCode: 17 }),
    });
    expect(response.status).toBe(400);
    expect((await json<ApiError>(response)).error.code).toBe("bad_request");
  });

  test("expired requests are swept rather than left to accumulate", async () => {
    const t = setup();
    await startDevice(t);
    await t.run(async (ctx) => {
      const row = (await ctx.db.query("cliDeviceCodes").collect())[0];
      // Two hours dead: past expiry plus the reaper's grace period.
      await ctx.db.patch("cliDeviceCodes", row._id, {
        expiresAt: Date.now() - 2 * 60 * 60 * 1000,
      });
    });

    expect(await t.mutation(internal.cliAuth.reapExpired, {})).toEqual({
      deleted: 1,
    });
    const left = await t.run(async (ctx) =>
      ctx.db.query("cliDeviceCodes").collect(),
    );
    expect(left).toHaveLength(0);
  });

  test("a user code already in play is never handed out twice", async () => {
    const t = setup();
    const { body: first } = await startDevice(t, { label: "build-01" });
    const taken = first.userCode.replace("-", "");

    // Reusing a live code would wire one developer's approval screen to another
    // developer's terminal, silently. The odds are 1 in 19^8, which is exactly
    // why nobody would ever find it in the field.
    const collided = await t.mutation(internal.cliAuth.startDevice, {
      deviceCodeHash: await sha256Hex(randomToken(32)),
      userCode: taken,
      deviceLabel: "laptop-02",
      cliVersion: "2026.08.07",
      expiresAt: Date.now() + 60_000,
      now: Date.now(),
    });
    expect(collided).toEqual({ status: "collision" });

    const rows = await t.run(async (ctx) =>
      ctx.db.query("cliDeviceCodes").collect(),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].deviceLabel).toBe("build-01");
  });

  test("a code freed up by expiry can be issued again", async () => {
    const t = setup();
    const { body: first } = await startDevice(t);
    const code = first.userCode.replace("-", "");
    await t.run(async (ctx) => {
      const row = (await ctx.db.query("cliDeviceCodes").collect())[0];
      await ctx.db.patch("cliDeviceCodes", row._id, { expiresAt: 1 });
    });

    // Codes are reaped rather than reserved forever, so the space has to be
    // reusable — and every lookup has to cope with two rows sharing a code.
    const reissued = await t.mutation(internal.cliAuth.startDevice, {
      deviceCodeHash: await sha256Hex(randomToken(32)),
      userCode: code,
      deviceLabel: "laptop-02",
      cliVersion: "2026.08.07",
      expiresAt: Date.now() + 60_000,
      now: Date.now(),
    });
    expect(reissued).toEqual({ status: "ok" });

    const { userId } = await seedMember(t);
    const asAda = t.withIdentity({ subject: `${userId}|session` });
    // The newest row wins, not whichever `.unique()` would have thrown over.
    expect(
      await asAda.query(api.cliAuth.deviceRequest, { userCode: code }),
    ).toMatchObject({ status: "pending", deviceLabel: "laptop-02" });
  });

  test("a live request survives the sweep", async () => {
    const t = setup();
    await startDevice(t);
    expect(await t.mutation(internal.cliAuth.reapExpired, {})).toEqual({
      deleted: 0,
    });
  });
});

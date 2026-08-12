/// <reference types="vite/client" />
import { convexTest } from "convex-test";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { Id } from "./_generated/dataModel";
import schema from "./schema";
import { randomToken, sha256Hex } from "./lib/crypto";

const modules = import.meta.glob("./**/*.ts");

type Role = "candidate" | "developer" | "lead";

function setup() {
  return convexTest(schema, modules);
}

async function seedMember(
  t: ReturnType<typeof setup>,
  overrides: {
    login?: string;
    role?: Role;
    status?: "active" | "suspended";
  } = {},
) {
  return await t.run(async (ctx) => {
    const userId = await ctx.db.insert("users", {
      name: "Ada Lovelace",
      email: "ada@clubria.dev",
    });
    // `rowId` — not `memberId` — because `members.memberId` is now a distinct
    // UUID field on the row itself; see the schema comment.
    const rowId = await ctx.db.insert("members", {
      userId,
      githubLogin: overrides.login ?? "ada",
      githubId: "1234",
      memberId: crypto.randomUUID(),
      firstName: "Ada",
      lastName: "Lovelace",
      email: "ada@clubria.dev",
      role: overrides.role ?? "developer",
      status: overrides.status ?? "active",
    });
    return { userId, rowId };
  });
}

/** Mints a live session the way `/api/v1/cli/token` would, minus the browser. */
async function issueSession(
  t: ReturnType<typeof setup>,
  memberId: Id<"members">,
  options: {
    expiresAt?: number;
    revoked?: boolean;
    deviceLabel?: string;
  } = {},
) {
  const token = randomToken(32);
  const tokenHash = await sha256Hex(token);
  await t.run(async (ctx) => {
    await ctx.db.insert("cliSessions", {
      memberId,
      tokenHash,
      deviceLabel: options.deviceLabel ?? "ada-mbp",
      cliVersion: "0.1.0",
      lastUsedAt: 0,
      expiresAt: options.expiresAt ?? Date.now() + 60_000,
      revokedAt: options.revoked === true ? Date.now() : undefined,
    });
  });
  return token;
}

function bearer(token: string, version?: string): HeadersInit {
  const headers: Record<string, string> = { Authorization: `Bearer ${token}` };
  if (version !== undefined) headers["x-riabuild-cli-version"] = version;
  return headers;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

describe("member ids", () => {
  test("a freshly inserted member row has a UUID-shaped member id", async () => {
    // NOTE: this goes through the `seedMember` fixture, not `auth.ts`'s
    // `upsertMember` — it does not exercise the `memberId: crypto.randomUUID()`
    // line in auth.ts at all. Deleting that line would not fail this test,
    // because `seedMember` mints its own id independently. Covering the real
    // auth.ts minting path means driving a sign-in through convex-test —
    // stubbing GitHub's token/user endpoints the way `stubUpstreams` does
    // below for Infisical — which is out of scope here.
    const t = setup();
    const { rowId } = await seedMember(t);
    const member = await t.run(async (ctx) => await ctx.db.get("members", rowId));
    expect(member?.memberId).toMatch(UUID);
  });

  // The backfill's own idempotency tests (which inserted a member row with no
  // `memberId`) were deleted in Task 2, when the schema field became
  // required: `convex-test` now rejects that fixture outright, and a row
  // without the field was its entire subject. `backfillMemberIds` is covered
  // by its one production deploy's returned count instead — see the comment
  // on the mutation in members.ts.
});

describe("member payloads", () => {
  test("every member payload carries the member id", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);

    const response = await t.fetch("/api/v1/me", {
      headers: bearer(token),
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.member.memberId).toMatch(UUID);
  });
});

describe("CLI login — device authorisation", () => {
  /** What the CLI does first: ask for a pair of codes. */
  async function startDevice(
    t: ReturnType<typeof setup>,
    options: { label?: string; version?: string } = {},
  ) {
    const headers: Record<string, string> = {};
    if (options.version !== undefined) {
      headers["x-riabuild-cli-version"] = options.version;
    }
    const response = await t.fetch("/api/v1/cli/device", {
      method: "POST",
      headers,
      body: JSON.stringify({ deviceLabel: options.label ?? "build-01" }),
    });
    return { response, body: await response.json() };
  }

  /** One tick of the CLI's poll loop. */
  async function poll(t: ReturnType<typeof setup>, deviceCode: string) {
    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ deviceCode }),
    });
    return { response, body: await response.json() };
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
    expect((await me.json()).member.role).toBe("developer");

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
    const unknown = await poll(t, randomToken(32));
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

    const blocked = await poll(t, device.deviceCode);
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
    const { response, body } = await startDevice(t, { version: "2026.08.01" });
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
    expect((await response.json()).error.code).toBe("bad_request");
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

describe("session authentication", () => {
  test("no bearer token is 401", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/me", {});
    expect(response.status).toBe(401);
    expect((await response.json()).error.code).toBe("unauthenticated");
  });

  test("a revoked session says so, so the CLI can re-login", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId, { revoked: true });
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    expect(response.status).toBe(401);
    expect((await response.json()).error.code).toBe("session_revoked");
  });

  test("an expired session says so", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId, { expiresAt: 1 });
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    expect(response.status).toBe(401);
    expect((await response.json()).error.code).toBe("session_expired");
  });

  test("a suspended member is 403, never 401", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { status: "suspended" });
    const token = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    // 401 would make the CLI re-authenticate, succeed, and loop forever.
    expect(response.status).toBe(403);
    expect((await response.json()).error.code).toBe("suspended");
  });

  test("a successful request records when the machine was last seen", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
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
    const token = await issueSession(t, rowId);
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
    expect((await me.json()).error.code).toBe("cli_too_old");
  });

  test("org config still answers an outdated CLI — it is how it learns", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
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
    const body = await response.json();
    expect(body.minCliVersion).toBe("2.0.0");
    expect(body.latestCliVersion).toBe("2.4.0");
  });
});

describe("org config and claude settings", () => {
  test("a fresh deployment serves defaults rather than an error", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    expect(response.status).toBe(200);
    expect((await response.json()).repoSlug).toBe("Clubria/ai-builders-hub");
  });

  test("the retired checkout path is still sent, so older CLIs can parse this", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    // Current CLIs choose the checkout location themselves — it differs per
    // platform, which one stored string cannot express. But a build released
    // before that change cannot deserialize a response missing this field, and
    // /api/v1 is add-only, so the endpoint keeps emitting a frozen value.
    expect((await response.json()).defaultProjectPath).toBe(
      "~/code/ai-builders-hub",
    );
  });

  test("a lead editing config drops the retired path from the stored row", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { role: "lead" });
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "Clubria/ai-builders-hub",
        defaultProjectPath: "~/code/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    await t
      .withIdentity({ subject: `${userId}|session` })
      .mutation(api.org.update, { repoSlug: "Clubria/ai-builders-hub" });

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    // The field is optional in the schema precisely so this write can drop it;
    // no backfill needed.
    expect(row?.defaultProjectPath).toBeUndefined();
  });

  test("claude settings come back parsed, with their timestamp", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: JSON.stringify({ env: { CLUBRIA: "1" } }),
        claudeSettingsUpdatedAt: 1234,
        repoSlug: "Clubria/ai-builders-hub",
        defaultProjectPath: "~/code",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    const response = await t.fetch("/api/v1/org/claude-settings", {
      headers: bearer(token),
    });
    const body = await response.json();
    expect(body.settings).toEqual({ env: { CLUBRIA: "1" } });
    expect(body.updatedAt).toBe(1234);
  });

  test("the default settings ask for bypass mode and pre-accept its disclaimer", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/claude-settings", {
      headers: bearer(token),
    });
    const { settings } = await response.json();

    expect(settings.theme).toBe("auto");
    expect(settings.permissions.defaultMode).toBe("bypassPermissions");
    // These two are one setting wearing two names. Claude Code downgrades
    // bypassPermissions to default unless the disclaimer has been accepted, so
    // shipping the mode alone produces a developer who thinks permissions are
    // off and gets prompted anyway.
    expect(settings.skipDangerousModePermissionPrompt).toBe(true);
  });

  test("the default settings carry the context-window status line", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);

    const response = await t.fetch("/api/v1/org/claude-settings", {
      headers: bearer(token),
    });
    // The path is load-bearing across two repositories: riabuild-cli's
    // `claude_statusline` task writes exactly this file.
    expect((await response.json()).settings.statusLine).toEqual({
      type: "command",
      command: "node ~/.riabuild/claude-statusline.js",
    });
  });

  test("the backfill adds a status line to settings a lead saved earlier", async () => {
    const t = setup();
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: JSON.stringify({ env: { CLUBRIA_ORG: "1" } }),
        claudeSettingsUpdatedAt: 1234,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    const result = await t.mutation(internal.org.backfillStatusLine, {});
    expect(result.updated).toBe(true);

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    const settings = JSON.parse(row!.claudeSettings);
    expect(settings.statusLine.command).toBe(
      "node ~/.riabuild/claude-statusline.js",
    );
    // Settings a lead already chose survive the migration.
    expect(settings.env).toEqual({ CLUBRIA_ORG: "1" });
    // The CLI re-fetches by comparing this. A backfill that left it at 1234
    // would change the database and nobody's laptop.
    expect(row!.claudeSettingsUpdatedAt).toBeGreaterThan(1234);
  });

  test("the backfill leaves a status line a lead chose alone", async () => {
    const t = setup();
    const chosen = { type: "command", command: "my-own-statusline" };
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: JSON.stringify({ statusLine: chosen }),
        claudeSettingsUpdatedAt: 1234,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    const result = await t.mutation(internal.org.backfillStatusLine, {});
    expect(result.updated).toBe(false);

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    expect(JSON.parse(row!.claudeSettings).statusLine).toEqual(chosen);
    expect(row!.claudeSettingsUpdatedAt).toBe(1234);
  });

  test("running the backfill twice is a no-op the second time", async () => {
    const t = setup();
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    expect((await t.mutation(internal.org.backfillStatusLine, {})).updated).toBe(
      true,
    );
    expect((await t.mutation(internal.org.backfillStatusLine, {})).updated).toBe(
      false,
    );

    const entries = await t.run(async (ctx) =>
      ctx.db
        .query("auditLog")
        .collect()
        .then((rows) =>
          rows.filter((row) => row.meta.via === "backfillStatusLine"),
        ),
    );
    expect(entries).toHaveLength(1);
  });

  test("the backfill refuses to guess at settings it cannot parse", async () => {
    const t = setup();
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{ not json",
        claudeSettingsUpdatedAt: 1234,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    const result = await t.mutation(internal.org.backfillStatusLine, {});
    expect(result.updated).toBe(false);
    expect(result.reason).toMatch(/dashboard/i);

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    // Replacing unreadable settings with generated ones would lose whatever the
    // lead meant to write.
    expect(row!.claudeSettings).toBe("{ not json");
  });

  /** Exactly what an org that saved before the permission keys existed holds. */
  const preBypassSettings = JSON.stringify({
    permissions: {
      deny: ["Read(./.env.local)", "Read(./.env)", "Bash(git push --force:*)"],
    },
    env: { CLUBRIA_ORG: "1" },
  });

  async function seedOrgConfig(
    t: ReturnType<typeof setup>,
    claudeSettings: string,
  ) {
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings,
        claudeSettingsUpdatedAt: 1234,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });
  }

  const storedSettings = async (t: ReturnType<typeof setup>) => {
    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    return { row: row!, settings: JSON.parse(row!.claudeSettings) };
  };

  test("the defaults backfill repairs an org that saved before bypass mode existed", async () => {
    // The regression a developer actually hit: `claude-1` started in the
    // default permission mode because the org row predated the keys, and
    // editing DEFAULT_CLAUDE_SETTINGS reached fresh deployments only.
    const t = setup();
    await seedOrgConfig(t, preBypassSettings);

    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(true);

    const { row, settings } = await storedSettings(t);
    expect(settings.permissions.defaultMode).toBe("bypassPermissions");
    // Without this the mode above is silently downgraded, so a backfill that
    // added one and not the other would look repaired and behave otherwise.
    expect(settings.skipDangerousModePermissionPrompt).toBe(true);
    expect(settings.theme).toBe("auto");
    expect(settings.statusLine.command).toBe(
      "node ~/.riabuild/claude-statusline.js",
    );
    expect(result.added).toContain("permissions.defaultMode");

    // The CLI re-fetches by comparing this. A backfill that left it at 1234
    // would change the database and nobody's laptop.
    expect(row.claudeSettingsUpdatedAt).toBeGreaterThan(1234);
  });

  test("the defaults backfill leaves every answer the org already gave", async () => {
    const t = setup();
    const chosen = {
      theme: "dark",
      permissions: { defaultMode: "acceptEdits", deny: [] },
      skipDangerousModePermissionPrompt: false,
      env: { CLUBRIA_ORG: "1", EXTRA: "kept" },
      statusLine: { type: "command", command: "my-own-statusline" },
    };
    await seedOrgConfig(t, JSON.stringify(chosen));

    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(false);

    const { row, settings } = await storedSettings(t);
    expect(settings).toEqual(chosen);
    expect(row.claudeSettingsUpdatedAt).toBe(1234);
  });

  test("the defaults backfill never restores deny rules an org removed", async () => {
    // An emptied deny list is a decision. Descending into an array to put
    // riabuild's entries back would undo it one element at a time.
    const t = setup();
    await seedOrgConfig(t, JSON.stringify({ permissions: { deny: [] } }));

    await t.mutation(internal.org.backfillClaudeDefaults, {});

    const { settings } = await storedSettings(t);
    expect(settings.permissions.deny).toEqual([]);
    // The sibling key it never answered still arrives.
    expect(settings.permissions.defaultMode).toBe("bypassPermissions");
  });

  test("running the defaults backfill twice is a no-op the second time", async () => {
    const t = setup();
    await seedOrgConfig(t, preBypassSettings);

    expect(
      (await t.mutation(internal.org.backfillClaudeDefaults, {})).updated,
    ).toBe(true);
    expect(
      (await t.mutation(internal.org.backfillClaudeDefaults, {})).updated,
    ).toBe(false);

    const entries = await t.run(async (ctx) =>
      ctx.db
        .query("auditLog")
        .collect()
        .then((rows) =>
          rows.filter((row) => row.meta.via === "backfillClaudeDefaults"),
        ),
    );
    expect(entries).toHaveLength(1);
  });

  test("the defaults backfill refuses to guess at settings it cannot parse", async () => {
    const t = setup();
    await seedOrgConfig(t, "{ not json");

    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(false);
    expect(result.reason).toMatch(/dashboard/i);

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    expect(row!.claudeSettings).toBe("{ not json");
  });

  test("the defaults backfill leaves a deployment with no stored row alone", async () => {
    const t = setup();
    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(false);

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    // Inserting one here would freeze today's defaults into the database and
    // recreate the very trap this migration exists to undo.
    expect(row).toBeNull();
  });
});

describe("secret brokering", () => {
  const realFetch = globalThis.fetch;

  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_ID", "client-id");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_SECRET", "client-secret");
    vi.stubEnv("INFISICAL_CANDIDATE_CLIENT_ID", "cand-id");
    vi.stubEnv("INFISICAL_CANDIDATE_CLIENT_SECRET", "cand-secret");
    vi.stubEnv("INFISICAL_PROJECT_ID", "proj_1");
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  /** Stands in for GitHub and Infisical so failure paths are reachable. */
  function stubUpstreams(options: {
    membership: number;
    infisical?: { status: number; body?: unknown };
    onLogin?: (body: unknown) => void;
  }) {
    globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
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
    };
  }

  test("an org member gets a short-lived token and an audit entry", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
    let loginBody: unknown = null;
    stubUpstreams({ membership: 204, onLogin: (body) => (loginBody = body) });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect(response.status).toBe(200);
    const body = await response.json();
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
    const token = await issueSession(t, rowId);
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

  test("leaving the GitHub org ends access, whatever Convex says", async () => {
    const t = setup();
    // Still `developer` and still `active` in Convex — GitHub is the gate.
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
    stubUpstreams({ membership: 404 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect(response.status).toBe(403);
    expect((await response.json()).error.code).toBe("not_org_member");
  });

  test("an unusable org token fails closed, and says it could not check", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
    vi.stubEnv("GITHUB_ORG_TOKEN", "");
    stubUpstreams({ membership: 204 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    // Not "you were removed from the org" — that sends them to the wrong person.
    expect(response.status).toBe(503);
    expect((await response.json()).error.code).toBe("org_check_unavailable");
  });

  test("an Infisical outage is translated, not forwarded", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const token = await issueSession(t, rowId);
    stubUpstreams({
      membership: 204,
      infisical: { status: 500, body: { message: "identity mi-developer" } },
    });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect(response.status).toBe(503);
    const body = await response.json();
    expect(body.error.code).toBe("upstream_error");
    expect(JSON.stringify(body)).not.toContain("mi-developer");
  });
});

describe("member administration", () => {
  test("only a lead can change roles", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { role: "developer" });
    const other = await seedMember(t, { login: "grace" });
    const asDeveloper = t.withIdentity({ subject: `${userId}|session` });

    await expect(
      asDeveloper.mutation(api.members.setRole, {
        memberId: other.rowId,
        role: "lead",
      }),
    ).rejects.toThrow(/team leads/i);
  });

  test("a promotion is recorded in the audit log", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const subject = await seedMember(t, { login: "grace", role: "candidate" });
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });

    await asLead.mutation(api.members.setRole, {
      memberId: subject.rowId,
      role: "developer",
    });

    const entries = await t.run(async (ctx) =>
      ctx.db.query("auditLog").collect(),
    );
    const promotion = entries.find(
      (entry) => entry.action === "member.role_changed",
    );
    expect(promotion?.meta).toMatchObject({ from: "candidate", to: "developer" });
  });

  test("a lead cannot demote themselves into locking everyone out", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });
    await expect(
      asLead.mutation(api.members.setRole, {
        memberId: lead.rowId,
        role: "candidate",
      }),
    ).rejects.toThrow(/another lead/i);
  });

  test("suspending kills live sessions immediately", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const subject = await seedMember(t, { login: "grace" });
    const token = await issueSession(t, subject.rowId);
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });

    await asLead.mutation(api.members.setStatus, {
      memberId: subject.rowId,
      status: "suspended",
    });

    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    expect(response.status).toBe(403);
  });

  test("org config rejects Claude settings that are not JSON", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });
    await expect(
      asLead.mutation(api.org.update, { claudeSettings: "{not json" }),
    ).rejects.toThrow(/valid JSON/i);
  });

  test("org config rejects a CLI version that is not dotted-numeric", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });

    // parseVersion maps anything unparseable to 0, so an accepted "v2026.08.04"
    // would become a silent floor of zero rather than an error.
    await expect(
      asLead.mutation(api.org.update, { latestCliVersion: "v2026.08.04" }),
    ).rejects.toThrow(/dotted-numeric/i);
    await expect(
      asLead.mutation(api.org.update, { minCliVersion: "latest" }),
    ).rejects.toThrow(/dotted-numeric/i);
  });

  test("org config refuses a floor nobody could satisfy", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });

    await asLead.mutation(api.org.update, {
      latestCliVersion: "2026.08.04",
      minCliVersion: "2026.08.04",
    });

    // Raising the floor past the newest published build would lock out every
    // developer at once, including those already on the latest release.
    await expect(
      asLead.mutation(api.org.update, { minCliVersion: "2026.09.01" }),
    ).rejects.toThrow(/nobody could satisfy/i);

    // The same floor is fine once that release exists.
    await asLead.mutation(api.org.update, {
      latestCliVersion: "2026.09.01",
      minCliVersion: "2026.09.01",
    });
    const config = await asLead.query(api.org.get);
    expect(config.minCliVersion).toBe("2026.09.01");
  });

  test("org config accepts a zero-padded release date", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });

    await asLead.mutation(api.org.update, { latestCliVersion: "2026.08.04" });
    const config = await asLead.query(api.org.get);
    expect(config.latestCliVersion).toBe("2026.08.04");
  });

  test("publishing a CLI version moves latest forward and audits it", async () => {
    const t = setup();
    const before = await t.query(api.org.get);

    const result = await t.mutation(internal.org.setLatestCliVersion, {
      version: "2026.08.04",
    });
    expect(result).toEqual({ updated: true, latestCliVersion: "2026.08.04" });

    const after = await t.query(api.org.get);
    expect(after.latestCliVersion).toBe("2026.08.04");
    // Publishing a release says nothing about what the team requires.
    expect(after.minCliVersion).toBe(before.minCliVersion);

    const entries = await t.run(
      async (ctx) => await ctx.db.query("auditLog").collect(),
    );
    const published = entries.find(
      (entry) => entry.action === "org.cli_version_published",
    );
    expect(published?.meta).toEqual({ from: "0.1.0", to: "2026.08.04" });
    // No actor: this is the release pipeline, not a person.
    expect(published?.actorId).toBeUndefined();
  });

  test("publishing never moves the CLI version backwards", async () => {
    const t = setup();
    await t.mutation(internal.org.setLatestCliVersion, {
      version: "2026.08.12",
    });

    // Re-running an older release's workflow — to retry a failed step, say —
    // must not offer every developer a downgrade.
    const older = await t.mutation(internal.org.setLatestCliVersion, {
      version: "2026.08.04",
    });
    expect(older).toEqual({ updated: false, latestCliVersion: "2026.08.12" });

    // Re-running the current one is a no-op, not a second audit entry
    // claiming something changed.
    const same = await t.mutation(internal.org.setLatestCliVersion, {
      version: "2026.08.12",
    });
    expect(same.updated).toBe(false);

    const entries = await t.run(
      async (ctx) => await ctx.db.query("auditLog").collect(),
    );
    expect(
      entries.filter((entry) => entry.action === "org.cli_version_published"),
    ).toHaveLength(1);
  });

  test("publishing rejects a version that is not dotted-numeric", async () => {
    const t = setup();
    await expect(
      t.mutation(internal.org.setLatestCliVersion, { version: "v2026.08.04" }),
    ).rejects.toThrow(/dotted-numeric/i);
  });

  test("internal member lookup returns the stored profile", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const member = await t.query(internal.members.byId, { memberId: rowId });
    expect(member?.githubLogin).toBe("ada");
  });
});

describe("revoking a session", () => {
  const realFetch = globalThis.fetch;

  // This endpoint re-verifies GitHub org membership same as /secrets/token —
  // revocation changes access, so the Convex row is never the sole gate here
  // either. Stub GitHub as reachable and membership as current.
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    globalThis.fetch = async (input: RequestInfo | URL) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) return new Response(null, { status: 204 });
      throw new Error(`unexpected fetch to ${url}`);
    };
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  test("a member can revoke their own session", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const caller = await issueSession(t, rowId);
    const victim = await issueSession(t, rowId, {
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
    expect(await response.json()).toEqual({ revoked: true });

    const revoked = await t.run(async (ctx) => await ctx.db.get("cliSessions", victimId!));
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
    const caller = await issueSession(t, mine);
    const other = await issueSession(t, theirs);
    const otherId = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.find((row) => row.memberId === theirs)?._id;
    });

    const response = await t.fetch(`/api/v1/cli/sessions/${otherId}`, {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(response.status).toBe(404);
    expect((await response.json()).error.code).toBe("session_unknown");
    expect(
      await t.run(async (ctx) => (await ctx.db.get("cliSessions", otherId!))?.revokedAt),
    ).toBeFalsy();

    // The victim's session survives untouched — the failed attempt didn't
    // revoke it, and it still authenticates.
    const stillLive = await t.fetch("/api/v1/me", { headers: bearer(other) });
    expect(stillLive.status).toBe(200);
  });

  test("an unknown session id gets the identical 404 as somebody else's session", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const caller = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/cli/sessions/not-a-real-id", {
      method: "DELETE",
      headers: bearer(caller),
    });
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({
      error: {
        code: "session_unknown",
        message: "That session no longer exists.",
        action: "Run `riabuild remote list` to see what is left.",
      },
    });
  });

  test("an org lead can revoke somebody else's session", async () => {
    const t = setup();
    const { rowId: leadRow } = await seedMember(t, { login: "lead", role: "lead" });
    const { rowId: devRow } = await seedMember(t, { login: "bob" });
    const caller = await issueSession(t, leadRow);
    const victim = await issueSession(t, devRow);
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
    const caller = await issueSession(t, rowId);
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
    const { rowId: leadRow } = await seedMember(t, { login: "lead", role: "lead" });
    const { rowId: devRow } = await seedMember(t, { login: "bob" });
    const caller = await issueSession(t, leadRow);
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

describe("announcing a release", () => {
  const realFetch = globalThis.fetch;

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  /**
   * Captures what `publishCliVersion` actually sent to GitHub, so the request
   * headers can be asserted rather than assumed.
   */
  function stubGitHub(spec: {
    status: number;
    body?: unknown;
    headers?: Record<string, string>;
  }) {
    const seen: { url?: string; headers?: Headers } = {};
    globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
      seen.url = input instanceof Request ? input.url : input.toString();
      seen.headers = new Headers(init?.headers);
      return new Response(
        spec.body === undefined ? null : JSON.stringify(spec.body),
        { status: spec.status, headers: spec.headers },
      );
    };
    return seen;
  }

  test("the release check is authenticated, so it does not spend GitHub's per-IP budget", async () => {
    const t = setup();
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    const seen = stubGitHub({ status: 200, body: { draft: false } });

    await t.action(api.release.publishCliVersion, { version: "2026.08.12.1" });

    // An unauthenticated request shares one 60-per-hour budget with every other
    // tenant on the Convex egress address. That budget ran out during the
    // 2026.08.12.1 release and stayed out for over an hour, so the release
    // published and nobody was offered it. This header is the whole fix.
    expect(seen.headers?.get("authorization")).toBe("Bearer ghp_test");
  });

  test("a rate-limited GitHub is reported as a rate limit, not as a bare 403", async () => {
    const t = setup();
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    stubGitHub({
      status: 403,
      body: { message: "API rate limit exceeded for 1.2.3.4" },
      headers: { "x-ratelimit-remaining": "0" },
    });

    // "returned 403" reads identically whether the budget ran out or the token
    // lost its access, and those need opposite responses: wait, or go and fix a
    // credential. Diagnosing the real one cost an hour of retrying the wrong.
    await expect(
      t.action(api.release.publishCliVersion, { version: "2026.08.12.1" }),
    ).rejects.toThrow(/rate limit/i);
  });

  test("a 403 that is not a rate limit still reads as a plain refusal", async () => {
    const t = setup();
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    stubGitHub({ status: 403, body: { message: "Forbidden" } });

    await expect(
      t.action(api.release.publishCliVersion, { version: "2026.08.12.1" }),
    ).rejects.toThrow(/returned 403/);
  });
});

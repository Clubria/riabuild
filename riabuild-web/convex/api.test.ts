/// <reference types="vite/client" />
import { convexTest } from "convex-test";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { Id } from "./_generated/dataModel";
import schema from "./schema";
import { randomToken, sha256Hex } from "./lib/crypto";
import {
  ED25519_FINGERPRINT,
  ED25519_PRIVATE,
  ED25519_PUBLIC,
} from "./lib/opensshKey.fixtures";

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
    const member = await t.run(
      async (ctx) => await ctx.db.get("members", rowId),
    );
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

  test("config names the environments the CLI must have on disk", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    // `check()` runs on every `riabuild --check` and must not broker a token to
    // learn which files it is looking for — brokering hits Infisical and writes
    // an audit row. So the list is served here too.
    expect((await response.json()).secretEnvironments).toEqual([
      "dev",
      "staging",
    ]);
  });

  test("a candidate's config names dev alone", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    const token = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    // Otherwise `check()` would demand a `.env.staging` that `apply()` is never
    // going to be allowed to write — a task that can never go green.
    expect((await response.json()).secretEnvironments).toEqual(["dev"]);
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

    expect(
      (await t.mutation(internal.org.backfillStatusLine, {})).updated,
    ).toBe(true);
    expect(
      (await t.mutation(internal.org.backfillStatusLine, {})).updated,
    ).toBe(false);

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
      deny: ["Read(./.env)", "Read(./.env.*)", "Bash(git push --force:*)"],
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

  test("an org still denying dotenv reads is taught the new filenames", async () => {
    // riabuild used to write one `.env.local` and now writes `.env.dev` and
    // `.env.staging`. `Read(./.env)` is an exact path, so neither new file is
    // covered — and `backfillClaudeDefaults` cannot help, because it only adds
    // keys that are *absent* and `permissions.deny` is present on every stored
    // row. Without this migration the secrets riabuild just wrote would be
    // readable by every Claude Code account on every existing deployment.
    const t = setup();
    await seedOrgConfig(
      t,
      JSON.stringify({
        permissions: {
          deny: ["Read(./.env.local)", "Read(./.env)", "Bash(git push --force:*)"],
        },
      }),
    );

    const result = await t.mutation(internal.org.denyEveryDotenvFile, {});
    expect(result.updated).toBe(true);

    const { row, settings } = await storedSettings(t);
    expect(settings.permissions.deny).toContain("Read(./.env.*)");
    // Additive only: nothing the org already had is removed, including the
    // now-redundant exact entry for the file riabuild no longer writes.
    expect(settings.permissions.deny).toContain("Read(./.env.local)");
    expect(settings.permissions.deny).toContain("Bash(git push --force:*)");
    // The CLI re-fetches by comparing this; leaving it would change the
    // database and nobody's laptop.
    expect(row.claudeSettingsUpdatedAt).toBeGreaterThan(1234);
  });

  test("an org that removed its dotenv denials is left alone", async () => {
    // The same rule the defaults backfill follows: an emptied deny list is a
    // decision, and putting an entry back one element at a time undoes it.
    // This migration teaches orgs the new *filenames*; it does not re-argue
    // whether dotenv files should be denied at all.
    const t = setup();
    const chosen = { permissions: { deny: ["Bash(git push --force:*)"] } };
    await seedOrgConfig(t, JSON.stringify(chosen));

    const result = await t.mutation(internal.org.denyEveryDotenvFile, {});
    expect(result.updated).toBe(false);

    const { row, settings } = await storedSettings(t);
    expect(settings).toEqual(chosen);
    expect(row.claudeSettingsUpdatedAt).toBe(1234);
  });

  test("running the dotenv migration twice is a no-op the second time", async () => {
    const t = setup();
    await seedOrgConfig(
      t,
      JSON.stringify({ permissions: { deny: ["Read(./.env)"] } }),
    );

    await t.mutation(internal.org.denyEveryDotenvFile, {});
    const { row: first } = await storedSettings(t);
    const second = await t.mutation(internal.org.denyEveryDotenvFile, {});

    expect(second.updated).toBe(false);
    const { row } = await storedSettings(t);
    expect(row.claudeSettingsUpdatedAt).toBe(first.claudeSettingsUpdatedAt);
  });

  test("a deployment with no stored config needs no dotenv migration", async () => {
    // It is served DEFAULT_CLAUDE_SETTINGS, which already carries the glob.
    const t = setup();
    const result = await t.mutation(internal.org.denyEveryDotenvFile, {});
    expect(result.updated).toBe(false);
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

  test("a developer is told to pull dev and staging", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
    stubUpstreams({ membership: 204 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    const body = await response.json();
    expect(body.environments).toEqual(["dev", "staging"]);
    // Still the base environment on its own, because a CLI released before
    // `environments` existed reads this field and nothing else.
    expect(body.environment).toBe("dev");
  });

  test("a candidate is told to pull dev alone", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    const token = await issueSession(t, rowId);
    stubUpstreams({ membership: 204 });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });
    expect((await response.json()).environments).toEqual(["dev"]);
  });

  test("the audit entry records every environment that was brokered", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
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
    expect(promotion?.meta).toMatchObject({
      from: "candidate",
      to: "developer",
    });
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

describe("announcing a release", () => {
  const realFetch = globalThis.fetch;

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  /** Stands in for api.github.com, and records how it was asked. */
  function stubGithub(options: {
    status: number;
    body?: unknown;
    rateLimitRemaining?: string;
  }) {
    const calls: { url: string; authorization: string | null }[] = [];
    globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
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
    };
    return calls;
  }

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

    const result = await t.action(api.release.publishCliVersion, {
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

    await t.action(api.release.publishCliVersion, { version: "2026.08.12.1" });

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
      t.action(api.release.publishCliVersion, { version: "2026.08.12.1" }),
    ).rejects.toThrow(/rate limit for GITHUB_ORG_TOKEN is exhausted/i);

    expect((await t.query(api.org.get)).latestCliVersion).toBe("0.1.0");
  });

  test("a release GitHub has never heard of is not announced", async () => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    const t = setup();
    stubGithub({ status: 404 });

    await expect(
      t.action(api.release.publishCliVersion, { version: "2026.09.01" }),
    ).rejects.toThrow(/Cut the release before announcing it/i);
  });

  test("a draft release is not announced", async () => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    const t = setup();
    stubGithub({ status: 200, body: { draft: true } });

    await expect(
      t.action(api.release.publishCliVersion, { version: "2026.09.01" }),
    ).rejects.toThrow(/still a draft/i);
  });
});

describe("signing a server in from the laptop", () => {
  const realFetch = globalThis.fetch;

  // Delegation hands out a live 90-day credential, so it re-verifies GitHub
  // org membership exactly the way /secrets/token does. Stub GitHub as
  // reachable and membership as current unless a test says otherwise.
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    globalThis.fetch = async (input: RequestInfo | URL) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com"))
        return new Response(null, { status: 204 });
      throw new Error(`unexpected fetch to ${url}`);
    };
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  function delegate(
    t: ReturnType<typeof setup>,
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
    const laptop = await issueSession(t, rowId, { deviceLabel: "ada-mbp" });

    const response = await delegate(t, laptop);
    expect(response.status).toBe(200);
    const body = await response.json();
    expect(typeof body.token).toBe("string");
    expect(body.token).not.toBe(laptop);
    expect(body.expiresAt).toBeGreaterThan(Date.now());
    expect(body.member.githubLogin).toBe("ada");

    // The minted token is a working session in its own right — this is what
    // the server's own riabuild runs as.
    const asServer = await t.fetch("/api/v1/me", { headers: bearer(body.token) });
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
      await t.run(async (ctx) => await ctx.db.query("cliDeviceCodes").collect()),
    ).toHaveLength(0);
  });

  test("the returned session id is the one that revokes it", async () => {
    // `riabuild remote forget` names this exact row through
    // `DELETE /api/v1/cli/sessions/<id>`. A session it cannot name is a live
    // credential on a shared box that nothing can take back.
    const t = setup();
    const { rowId } = await seedMember(t);
    const laptop = await issueSession(t, rowId);
    const { token, sessionId } = await (await delegate(t, laptop)).json();

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
    const laptop = await issueSession(t, rowId);
    const { token } = await (await delegate(t, laptop)).json();

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
    const laptop = await issueSession(t, rowId);
    const { token: server } = await (await delegate(t, laptop)).json();

    const again = await delegate(t, server, { deviceLabel: "build-02" });
    expect(again.status).toBe(403);
    expect((await again.json()).error.code).toBe("delegation_not_permitted");

    // Refused, not merely unreported: no third session came into existence.
    expect(
      await t.run(async (ctx) => await ctx.db.query("cliSessions").collect()),
    ).toHaveLength(2);
  });

  test("leaving the GitHub org ends delegation, whatever Convex says", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const laptop = await issueSession(t, rowId);
    globalThis.fetch = async () => new Response(null, { status: 404 });

    const response = await delegate(t, laptop);
    expect(response.status).toBe(403);
    expect((await response.json()).error.code).toBe("not_org_member");
    expect(
      await t.run(async (ctx) => await ctx.db.query("cliSessions").collect()),
    ).toHaveLength(1);
  });

  test("a revoked laptop session mints nothing", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const laptop = await issueSession(t, rowId, { revoked: true });
    expect((await delegate(t, laptop)).status).toBe(401);
  });

  test("a suspended member mints nothing, and is told to stop rather than retry", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { status: "suspended" });
    const laptop = await issueSession(t, rowId);
    const response = await delegate(t, laptop);
    // 403, not 401: signing in again would succeed and change nothing.
    expect(response.status).toBe(403);
    expect((await response.json()).error.code).toBe("suspended");
  });

  test("delegation is written to the audit log", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const laptop = await issueSession(t, rowId);
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
    const laptop = await issueSession(t, rowId);
    const response = await delegate(t, laptop, { deviceLabel: 42 });
    expect(response.status).toBe(400);
    expect((await response.json()).error.code).toBe("bad_request");
  });

  test("a CLI below the floor is told to upgrade rather than handed a token", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const laptop = await issueSession(t, rowId);
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
    expect((await response.json()).error.code).toBe("cli_too_old");
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
      if (url.includes("api.github.com"))
        return new Response(null, { status: 204 });
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
    const { rowId: leadRow } = await seedMember(t, {
      login: "lead",
      role: "lead",
    });
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
    const { rowId: leadRow } = await seedMember(t, {
      login: "lead",
      role: "lead",
    });
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

describe("the team's shared servers", () => {
  const realFetch = globalThis.fetch;

  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  /** Stands in for GitHub's org membership check. 204 is "yes". */
  function stubMembership(status: number) {
    globalThis.fetch = async (input: RequestInfo | URL) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        return new Response(null, { status });
      }
      throw new Error(`unexpected fetch to ${url}`);
    };
  }

  async function seedServer(
    t: ReturnType<typeof setup>,
    lead: Id<"members">,
    overrides: Partial<{
      name: string;
      host: string;
      port: number;
      user: string;
    }> = {},
  ) {
    await t.run(async (ctx) => {
      const now = Date.now();
      await ctx.db.insert("sharedServers", {
        name: overrides.name ?? "gpu",
        host: overrides.host ?? "gpu.internal",
        port: overrides.port ?? 2222,
        user: overrides.user ?? "ada",
        createdBy: lead,
        createdAt: now,
        updatedAt: now,
      });
    });
  }

  test("a developer gets every shared server, with its row id", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedServer(t, rowId);
    await seedServer(t, rowId, {
      name: "build",
      host: "build.internal",
      port: 22,
    });
    const token = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    const body = await response.json();
    // Sorted by name, so the picker's numbering is stable between runs rather
    // than following whatever order the rows happen to come back in.
    expect(body.servers.map((server: { name: string }) => server.name)).toEqual(
      ["build", "gpu"],
    );
    expect(body.servers[1]).toMatchObject({
      name: "gpu",
      host: "gpu.internal",
      port: 2222,
      user: "ada",
    });
    // The id is what the CLI keys its own state by — it has to be there, and it
    // has to survive a rename and an address edit, which is what a row id does.
    expect(typeof body.servers[1].id).toBe("string");
    expect(body.servers[1].id.length).toBeGreaterThan(0);
  });

  test("a lead gets them too", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "lead" });
    await seedServer(t, rowId);
    const token = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect((await response.json()).servers).toHaveLength(1);
  });

  test("a candidate gets an empty list rather than a refusal", async () => {
    // 200 and { servers: [] }, never 403. `riabuild remote` is also how a
    // candidate reaches the server they set up themselves, and refusing the
    // whole request would take that away in order to enforce a rule about
    // servers they were never going to see.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    await seedServer(t, rowId);
    const token = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ servers: [] });
  });

  test("someone who has left the GitHub org gets 403, not a server list", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedServer(t, rowId);
    const token = await issueSession(t, rowId);
    stubMembership(404);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(403);
    const body = await response.json();
    expect(body.error.code).toBe("not_org_member");
  });

  test("no session at all gets 401", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/remotes/shared", {});
    expect(response.status).toBe(401);
  });

  test("a revoked session gets 401", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId, { revoked: true });

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(401);
  });

  test("a CLI below the version floor is told to upgrade", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion: "2026.09.01",
        latestCliVersion: "2026.09.01",
        secretsUpdatedAt: 0,
      });
    });

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token, "2026.08.01"),
    });

    expect(response.status).toBe(409);
  });

  test("an empty table is an empty list, not an error", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ servers: [] });
  });
});

describe("the SSH keys the org issues", () => {
  const realFetch = globalThis.fetch;

  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  /** Stands in for GitHub's org membership check. 204 is "yes". */
  function stubMembership(status: number) {
    globalThis.fetch = async (input: RequestInfo | URL) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        return new Response(null, { status });
      }
      throw new Error(`unexpected fetch to ${url}`);
    };
  }

  async function seedKey(
    t: ReturnType<typeof setup>,
    lead: Id<"members">,
    issuedTo: Id<"members">[],
    overrides: Partial<{ label: string; privateKey: string }> = {},
  ) {
    await t.run(async (ctx) => {
      const now = Date.now();
      await ctx.db.insert("issuedKeys", {
        label: overrides.label ?? "prod-bastion",
        privateKey: overrides.privateKey ?? ED25519_PRIVATE,
        publicKey: ED25519_PUBLIC,
        fingerprint: ED25519_FINGERPRINT,
        keyType: "ssh-ed25519",
        issuedTo,
        createdBy: lead,
        createdAt: now,
        updatedAt: now,
      });
    });
  }

  test("a developer gets the keys issued to them, whole", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedKey(t, rowId, [rowId]);
    const token = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    const body = await response.json();
    expect(body.keys).toHaveLength(1);
    expect(body.keys[0]).toMatchObject({
      label: "prod-bastion",
      keyType: "ssh-ed25519",
      publicKey: ED25519_PUBLIC,
      fingerprint: ED25519_FINGERPRINT,
    });
    // The private half travels in the same response. A second, separately
    // authorised fetch would be theatre — same session, same bearer token,
    // same connection — and the CLI needs every key it is entitled to in
    // order to probe them anyway.
    expect(body.keys[0].privateKey).toContain("BEGIN OPENSSH PRIVATE KEY");
    expect(typeof body.keys[0].id).toBe("string");
  });

  test("a developer gets nothing from a key issued to somebody else", async () => {
    // The whole authorisation model in one assertion: entitlement is a list on
    // the row, and a member not on it is not served, whatever their role.
    const t = setup();
    const { rowId: ada } = await seedMember(t, { role: "developer" });
    const { rowId: alan } = await seedMember(t, {
      role: "developer",
      login: "alan",
    });
    await seedKey(t, ada, [alan]);
    const token = await issueSession(t, ada);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ keys: [] });
  });

  test("a candidate gets an empty list rather than a refusal", async () => {
    // 200 and `{ keys: [] }`, never 403 — the rule /api/v1/remotes/shared
    // already sets. `riabuild remote` is also how a candidate reaches the
    // server they set up themselves.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    await seedKey(t, rowId, [rowId]);
    const token = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ keys: [] });
  });

  test("someone who has left the GitHub org gets 403, not a private key", async () => {
    // The one that matters most on this endpoint. This is the only response in
    // riabuild carrying a durable credential, so `members.role` being stale
    // must not be enough to keep it flowing.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedKey(t, rowId, [rowId]);
    const token = await issueSession(t, rowId);
    stubMembership(404);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(403);
    const body = await response.json();
    expect(body.error.code).toBe("not_org_member");
    expect(JSON.stringify(body)).not.toContain("BEGIN OPENSSH");
  });

  test("no session at all gets 401", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/issued-keys", {});
    expect(response.status).toBe(401);
  });

  test("a revoked session gets 401", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedKey(t, rowId, [rowId]);
    const token = await issueSession(t, rowId, { revoked: true });

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(401);
  });

  test("a served fetch is written to the audit log by label", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedKey(t, rowId, [rowId]);
    await seedKey(t, rowId, [rowId], { label: "gpu-box" });
    const token = await issueSession(t, rowId);
    stubMembership(204);

    await t.fetch("/api/v1/issued-keys", { headers: bearer(token) });

    const audit = await t.run(async (ctx) =>
      ctx.db.query("auditLog").collect(),
    );
    const served = audit.find((row) => row.action === "issued_key.served");
    expect(served?.meta.keys).toBe("gpu-box,prod-bastion");
    expect(served?.meta.count).toBe("2");
    expect(JSON.stringify(audit)).not.toContain("BEGIN OPENSSH");
  });

  test("a candidate's refused fetch is not logged as a fetch", async () => {
    // Nothing was served, so there is nothing to have taken a copy of. A row
    // here would make the log read as though a candidate had been handed keys.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    await seedKey(t, rowId, [rowId]);
    const token = await issueSession(t, rowId);
    stubMembership(204);

    await t.fetch("/api/v1/issued-keys", { headers: bearer(token) });

    const audit = await t.run(async (ctx) =>
      ctx.db.query("auditLog").collect(),
    );
    expect(audit.find((row) => row.action === "issued_key.served")).toBe(
      undefined,
    );
  });

  test("no keys at all is an empty list, not an error", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ keys: [] });
  });
});

describe("the team's ngrok authtoken", () => {
  const realFetch = globalThis.fetch;

  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    globalThis.fetch = realFetch;
  });

  /** Stands in for the org-membership check every brokering route re-runs. */
  function stubGithub(membership: number) {
    globalThis.fetch = async (input: RequestInfo | URL) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        return new Response(null, { status: membership });
      }
      throw new Error(`unexpected fetch to ${url}`);
    };
  }

  async function setToken(t: ReturnType<typeof setup>, token: string) {
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
    const session = await issueSession(t, rowId);
    stubGithub(204);

    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(200);
    expect((await response.json()).token).toBe("2abcDEF_the_org_token");

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
    const session = await issueSession(t, rowId);
    stubGithub(204);

    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(404);
    // The code, not just the status: a route that does not exist is also a 404,
    // and the CLI rewords this one for the developer.
    expect((await response.json()).error.code).toBe("not_configured");
  });

  test("a developer removed from the GitHub org loses the tunnel today", async () => {
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");
    const { rowId } = await seedMember(t, { login: "ada" });
    const session = await issueSession(t, rowId);
    // Their Convex row still says active; GitHub is the identity.
    stubGithub(404);

    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(403);
  });

  test("a machine with no session is refused", async () => {
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");

    const response = await t.fetch("/api/v1/org/ngrok-token", {});
    expect(response.status).toBe(401);
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

  test("config says when the token was set, and never what it is", async () => {
    const t = setup();
    await setToken(t, "2abcDEF_the_org_token");
    const { rowId } = await seedMember(t, { login: "ada" });
    const session = await issueSession(t, rowId);

    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(session),
    });
    const body = await response.json();
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
    const session = await issueSession(t, rowId);
    stubGithub(204);
    const response = await t.fetch("/api/v1/org/ngrok-token", {
      headers: bearer(session),
    });
    expect(response.status).toBe(404);
  });
});

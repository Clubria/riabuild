import { beforeEach, describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { Id } from "./_generated/dataModel";
import {
  ApiError,
  bearer,
  currentVersion,
  issueSession,
  json,
  seedMember,
  setup,
  stubMembership,
  TestConvex,
} from "./testing.fixtures";

/**
 * `POST /api/v1/usage` and the rollup a lead reads out of it.
 *
 * The decisions worth pinning are the merge rule and who may read the result.
 * Everything else here is shape.
 */

type Accepted = { accepted: number };

type Sample = {
  harness?: string;
  accountId?: string;
  sessionId?: string;
  model?: string;
  costUsd?: number;
  durationMs?: number;
  apiDurationMs?: number;
  linesAdded?: number;
  linesRemoved?: number;
  fiveHourPct?: number;
  fiveHourResetsAt?: number;
  sevenDayPct?: number;
  sevenDayResetsAt?: number;
  /** Not part of the wire format. Sent by one test to prove it is ignored. */
  memberId?: string;
};

const ACCOUNT = "0f9c1e5b-7d2a-4864-9c1e-5b8d3a7f2c94";

function sample(overrides: Sample = {}): Sample {
  return {
    harness: "claude",
    accountId: ACCOUNT,
    sessionId: "sess-1",
    model: "claude-opus-5",
    ...overrides,
  };
}

async function post(
  t: TestConvex,
  token: string,
  samples: Sample[],
): Promise<Response> {
  return await t.fetch("/api/v1/usage", {
    method: "POST",
    headers: { ...bearer(token), "content-type": "application/json" },
    body: JSON.stringify({ samples }),
  });
}

/** Every row for one member, as the assertions want to read them. */
async function storedRows(t: TestConvex, memberId: Id<"members">) {
  return await t.run(async (ctx) => {
    const rows = await ctx.db
      .query("usageSessions")
      .withIndex("by_member_observed", (q) => q.eq("memberId", memberId))
      .collect();
    return rows.map((row) => ({
      accountId: row.accountId,
      sessionId: row.sessionId,
      harness: row.harness,
      model: row.model,
      costUsd: row.costUsd,
      durationMs: row.durationMs,
      linesAdded: row.linesAdded,
      linesRemoved: row.linesRemoved,
      fiveHourPct: row.fiveHourPct,
      sevenDayPct: row.sevenDayPct,
    }));
  });
}

describe("POST /api/v1/usage", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  test("a first sample becomes one row", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await post(t, token, [
      sample({ costUsd: 1.25, durationMs: 4000, linesAdded: 10 }),
    ]);

    expect(response.status).toBe(200);
    expect(await json<Accepted>(response)).toEqual({ accepted: 1 });
    expect(await storedRows(t, rowId)).toEqual([
      {
        accountId: ACCOUNT,
        sessionId: "sess-1",
        harness: "claude",
        model: "claude-opus-5",
        costUsd: 1.25,
        durationMs: 4000,
        linesAdded: 10,
        linesRemoved: undefined,
        fiveHourPct: undefined,
        sevenDayPct: undefined,
      },
    ]);
  });

  /**
   * The correctness core. These are cumulative-per-session totals, so the
   * second sample *contains* the first — adding them together would report
   * roughly one session per message in it.
   */
  test("a second sample for one session merges, and never sums", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    await post(t, token, [
      sample({ costUsd: 1.25, durationMs: 4000, linesAdded: 10 }),
    ]);
    await post(t, token, [
      sample({ costUsd: 3, durationMs: 9000, linesAdded: 42 }),
    ]);

    const rows = await storedRows(t, rowId);
    expect(rows).toHaveLength(1);
    expect(rows[0].costUsd).toBe(3);
    expect(rows[0].durationMs).toBe(9000);
    expect(rows[0].linesAdded).toBe(42);
    // The sums, spelled out, so a regression to `+` fails here by name.
    expect(rows[0].costUsd).not.toBe(4.25);
    expect(rows[0].durationMs).not.toBe(13000);
  });

  /**
   * Three windows on one laptop flush independently, so samples overtake each
   * other in flight. The older one arriving second must not walk the total
   * back.
   */
  test("a lower sample does not lower what is stored", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    await post(t, token, [
      sample({ costUsd: 9.5, durationMs: 60_000, linesRemoved: 30 }),
    ]);
    await post(t, token, [
      sample({ costUsd: 0.5, durationMs: 1_000, linesRemoved: 1 }),
    ]);

    const rows = await storedRows(t, rowId);
    expect(rows[0].costUsd).toBe(9.5);
    expect(rows[0].durationMs).toBe(60_000);
    expect(rows[0].linesRemoved).toBe(30);
  });

  /**
   * The exception to the rule above, and the reason it is written as an
   * exception: a rate-limit percentage falls when its window resets. Kept as a
   * maximum it would report somebody as permanently out of headroom because of
   * one busy afternoon.
   */
  test("a rate-limit percentage takes the newest value, even downwards", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    await post(t, token, [sample({ fiveHourPct: 88, sevenDayPct: 61 })]);
    await post(t, token, [sample({ fiveHourPct: 4, sevenDayPct: 62 })]);

    const rows = await storedRows(t, rowId);
    expect(rows[0].fiveHourPct).toBe(4);
    expect(rows[0].sevenDayPct).toBe(62);
  });

  test("two accounts and two sessions are four rows", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    await post(t, token, [
      sample({ sessionId: "a" }),
      sample({ sessionId: "b" }),
      sample({ accountId: "other-account", sessionId: "a" }),
      sample({ accountId: "other-account", sessionId: "b" }),
    ]);

    expect(await storedRows(t, rowId)).toHaveLength(4);
  });

  /**
   * There is no `memberId` on the wire, and a body that invents one must not
   * become one. The session already proved who this is.
   */
  test("a memberId in the body is ignored, not honoured", async () => {
    const t = setup();
    const { rowId: mine } = await seedMember(t, {
      login: "ada",
      role: "developer",
    });
    const { rowId: theirs } = await seedMember(t, {
      login: "grace",
      githubId: "5678",
      role: "developer",
    });
    const { token } = await issueSession(t, mine);
    stubMembership(204);

    const response = await post(t, token, [sample({ memberId: theirs })]);

    expect(response.status).toBe(200);
    expect(await storedRows(t, mine)).toHaveLength(1);
    expect(await storedRows(t, theirs)).toHaveLength(0);
  });

  test("more than 200 samples is refused, and nothing is stored", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const many = Array.from({ length: 201 }, (_, i) =>
      sample({ sessionId: `sess-${i}` }),
    );
    const response = await post(t, token, many);

    expect(response.status).toBe(400);
    const body = await json<ApiError>(response);
    expect(body.error.code).toBe("bad_request");
    expect(body.error.message).toContain("201");
    expect(body.error.action).not.toBe("");
    // Refused rather than truncated: the CLI clears what the server accepted.
    expect(await storedRows(t, rowId)).toHaveLength(0);
  });

  test("exactly 200 samples is accepted", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const many = Array.from({ length: 200 }, (_, i) =>
      sample({ sessionId: `sess-${i}` }),
    );
    const response = await post(t, token, many);

    expect(response.status).toBe(200);
    expect(await json<Accepted>(response)).toEqual({ accepted: 200 });
  });

  test.each([
    ["no samples array", {}],
    ["samples is not an array", { samples: "nope" }],
    ["a sample with no session id", { samples: [{ harness: "claude" }] }],
    [
      "a cost that is not a number",
      { samples: [{ ...sample(), costUsd: "1.25" }] },
    ],
    ["a negative duration", { samples: [{ ...sample(), durationMs: -1 }] }],
  ])("%s is a 400", async (_name, body) => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/usage", {
      method: "POST",
      headers: { ...bearer(token), "content-type": "application/json" },
      body: JSON.stringify(body),
    });

    expect(response.status).toBe(400);
    expect((await json<ApiError>(response)).error.code).toBe("bad_request");
  });

  /**
   * A field this deployment has not learned about yet must not turn a working
   * CLI into a 400 — riabuild upgrades on every developer's own schedule, and
   * the fleet is always mixed.
   */
  test("an unknown field in a sample is ignored", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/usage", {
      method: "POST",
      headers: { ...bearer(token), "content-type": "application/json" },
      body: JSON.stringify({
        samples: [
          { ...sample(), somethingNewer: 42, workspace: "secret-repo" },
        ],
      }),
    });

    expect(response.status).toBe(200);
    expect(await storedRows(t, rowId)).toHaveLength(1);
  });

  test("no session is a 401", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/usage", {
      method: "POST",
      // The version header, but no bearer token: the floor is checked before
      // the session, so without it this would be a 409 about being out of date.
      headers: { ...currentVersion, "content-type": "application/json" },
      body: JSON.stringify({ samples: [sample()] }),
    });
    expect(response.status).toBe(401);
  });

  /** Identity is GitHub: a departed developer stops filing rows today. */
  test("losing org membership is a 403", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(404);

    const response = await post(t, token, [sample()]);

    expect(response.status).toBe(403);
    expect((await json<ApiError>(response)).error.code).toBe("not_org_member");
    expect(await storedRows(t, rowId)).toHaveLength(0);
  });

  /**
   * Not a flood of them, and not one per sample. `auditLog` is the record of
   * changes to access, read by a human scrolling a list; a flush a minute per
   * active developer would bury every row in it that matters.
   */
  test("nothing is written to the audit log", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    await post(t, token, [sample(), sample({ sessionId: "sess-2" })]);

    const rows = await t.run(
      async (ctx) => await ctx.db.query("auditLog").collect(),
    );
    expect(rows).toHaveLength(0);
  });
});

describe("the lead rollup", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  async function signedIn(t: TestConvex, userId: Id<"users">) {
    return t.withIdentity({ subject: `${userId}|session` });
  }

  test("a lead sees one row per member, summed across their sessions", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "grace", role: "lead" });
    const dev = await seedMember(t, {
      login: "ada",
      githubId: "5678",
      role: "developer",
    });
    const { token } = await issueSession(t, dev.rowId);
    stubMembership(204);

    await post(t, token, [
      sample({ sessionId: "a", costUsd: 1.5, linesAdded: 10, linesRemoved: 2 }),
      sample({
        sessionId: "b",
        costUsd: 2.25,
        linesAdded: 5,
        linesRemoved: 1,
        fiveHourPct: 42,
        fiveHourResetsAt: 1_800_000_000,
        sevenDayPct: 12,
      }),
    ]);

    const asLead = await signedIn(t, lead.userId);
    const result = await asLead.query(api.usage.rollup, {});

    expect(result.windowDays).toBe(7);
    expect(result.rows).toHaveLength(1);
    expect(result.rows[0]).toMatchObject({
      githubLogin: "ada",
      sessions: 2,
      // Summed *across* sessions, which is the one place summing is right.
      costUsd: 3.75,
      linesAdded: 15,
      linesRemoved: 3,
      fiveHourPct: 42,
      fiveHourResetsAt: 1_800_000_000,
      sevenDayPct: 12,
      truncated: false,
    });
  });

  test("a member with nothing in the window is not a row", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "grace", role: "lead" });
    await seedMember(t, { login: "ada", githubId: "5678", role: "developer" });

    const asLead = await signedIn(t, lead.userId);
    const result = await asLead.query(api.usage.rollup, {});

    expect(result.rows).toEqual([]);
  });

  test("a window older than the retention cap is clamped rather than honoured", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "grace", role: "lead" });
    const asLead = await signedIn(t, lead.userId);

    const result = await asLead.query(api.usage.rollup, { windowDays: 10_000 });

    expect(result.windowDays).toBe(90);
  });

  test.each(["developer", "candidate"] as const)(
    "a %s cannot read the rollup",
    async (role) => {
      const t = setup();
      const { userId } = await seedMember(t, { login: "ada", role });
      const asThem = await signedIn(t, userId);

      await expect(asThem.query(api.usage.rollup, {})).rejects.toThrow(
        /Only team leads/,
      );
    },
  );

  test("a signed-out visitor cannot read the rollup", async () => {
    const t = setup();
    await expect(t.query(api.usage.rollup, {})).rejects.toThrow(
      /Not signed in/,
    );
  });

  test("a suspended lead cannot read the rollup", async () => {
    const t = setup();
    const { userId } = await seedMember(t, {
      login: "grace",
      role: "lead",
      status: "suspended",
    });
    const asThem = await signedIn(t, userId);

    await expect(asThem.query(api.usage.rollup, {})).rejects.toThrow(
      /suspended/,
    );
  });
});

describe("the ninety-day reaper", () => {
  test("it deletes what has aged out and keeps what has not", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const nowSeconds = Math.floor(Date.now() / 1000);
    const day = 24 * 60 * 60;

    await t.run(async (ctx) => {
      for (const [sessionId, age] of [
        ["fresh", 1],
        ["a-month", 30],
        ["ancient", 120],
      ] as const) {
        await ctx.db.insert("usageSessions", {
          memberId: rowId,
          accountId: ACCOUNT,
          sessionId,
          harness: "claude",
          observedAt: nowSeconds - age * day,
        });
      }
    });

    const result = await t.mutation(internal.usage.reapOld, {});

    expect(result).toEqual({ deleted: 1 });
    const left = await storedRows(t, rowId);
    expect(left.map((row) => row.sessionId).sort()).toEqual([
      "a-month",
      "fresh",
    ]);
  });
});

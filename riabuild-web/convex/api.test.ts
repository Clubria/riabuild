/// <reference types="vite/client" />
import { convexTest } from "convex-test";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { api, internal } from "./_generated/api";
import { Id } from "./_generated/dataModel";
import schema from "./schema";
import { pkceChallenge, randomToken, sha256Hex } from "./lib/crypto";

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
    const memberId = await ctx.db.insert("members", {
      userId,
      githubLogin: overrides.login ?? "ada",
      githubId: "1234",
      firstName: "Ada",
      lastName: "Lovelace",
      email: "ada@clubria.dev",
      role: overrides.role ?? "developer",
      status: overrides.status ?? "active",
    });
    return { userId, memberId };
  });
}

/** Mints a live session the way `/api/v1/cli/token` would, minus the browser. */
async function issueSession(
  t: ReturnType<typeof setup>,
  memberId: Id<"members">,
  options: { expiresAt?: number; revoked?: boolean } = {},
) {
  const token = randomToken(32);
  const tokenHash = await sha256Hex(token);
  await t.run(async (ctx) => {
    await ctx.db.insert("cliSessions", {
      memberId,
      tokenHash,
      deviceLabel: "ada-mbp",
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

describe("CLI login — loopback code exchange", () => {
  test("approves in the browser, redeems in the terminal", async () => {
    const t = setup();
    const { userId, memberId } = await seedMember(t);
    const verifier = randomToken(32);

    const asAda = t.withIdentity({ subject: `${userId}|session` });
    const { code } = await asAda.action(api.cliAuth.authorize, {
      challenge: await pkceChallenge(verifier),
      deviceLabel: "ada-mbp",
      cliVersion: "0.1.0",
    });

    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ code, verifier }),
    });
    expect(response.status).toBe(200);

    const body = await response.json();
    expect(body.member.githubLogin).toBe("ada");
    expect(typeof body.token).toBe("string");

    // The session is real: it authenticates the next request.
    const me = await t.fetch("/api/v1/me", { headers: bearer(body.token) });
    expect(me.status).toBe(200);
    expect((await me.json()).member.role).toBe("developer");

    // And it was stored hashed, not raw.
    const stored = await t.run(async (ctx) => {
      const rows = await ctx.db.query("cliSessions").collect();
      return rows.map((row) => row.tokenHash);
    });
    expect(stored).toHaveLength(1);
    expect(stored[0]).not.toBe(body.token);
    expect(stored[0]).toBe(await sha256Hex(body.token));
    expect(memberId).toBeDefined();
  });

  test("a code presented with the wrong verifier is refused", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const asAda = t.withIdentity({ subject: `${userId}|session` });
    const { code } = await asAda.action(api.cliAuth.authorize, {
      challenge: await pkceChallenge(randomToken(32)),
      deviceLabel: "ada-mbp",
      cliVersion: "0.1.0",
    });

    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ code, verifier: randomToken(32) }),
    });
    expect(response.status).toBe(401);
    const sessions = await t.run(async (ctx) =>
      ctx.db.query("cliSessions").collect(),
    );
    expect(sessions).toHaveLength(0);
  });

  test("a code is spent even by a failed attempt", async () => {
    const t = setup();
    const { userId } = await seedMember(t);
    const verifier = randomToken(32);
    const asAda = t.withIdentity({ subject: `${userId}|session` });
    const { code } = await asAda.action(api.cliAuth.authorize, {
      challenge: await pkceChallenge(verifier),
      deviceLabel: "ada-mbp",
      cliVersion: "0.1.0",
    });

    // Wrong verifier first, correct verifier second: the code must not survive
    // the failed attempt for someone who intercepted it to retry.
    await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ code, verifier: randomToken(32) }),
    });
    const retry = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ code, verifier }),
    });
    expect(retry.status).toBe(401);
  });

  test("a suspended member cannot approve a machine", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { status: "suspended" });
    const asAda = t.withIdentity({ subject: `${userId}|session` });
    await expect(
      asAda.action(api.cliAuth.authorize, {
        challenge: await pkceChallenge(randomToken(32)),
        deviceLabel: "ada-mbp",
        cliVersion: "0.1.0",
      }),
    ).rejects.toThrow(/suspended/i);
  });

  test("a malformed body is a 400, not a 500", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/cli/token", {
      method: "POST",
      body: JSON.stringify({ code: 17 }),
    });
    expect(response.status).toBe(400);
    expect((await response.json()).error.code).toBe("bad_request");
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
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId, { revoked: true });
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    expect(response.status).toBe(401);
    expect((await response.json()).error.code).toBe("session_revoked");
  });

  test("an expired session says so", async () => {
    const t = setup();
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId, { expiresAt: 1 });
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    expect(response.status).toBe(401);
    expect((await response.json()).error.code).toBe("session_expired");
  });

  test("a suspended member is 403, never 401", async () => {
    const t = setup();
    const { memberId } = await seedMember(t, { status: "suspended" });
    const token = await issueSession(t, memberId);
    const response = await t.fetch("/api/v1/me", { headers: bearer(token) });
    // 401 would make the CLI re-authenticate, succeed, and loop forever.
    expect(response.status).toBe(403);
    expect((await response.json()).error.code).toBe("suspended");
  });

  test("a successful request records when the machine was last seen", async () => {
    const t = setup();
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    expect(response.status).toBe(200);
    expect((await response.json()).repoSlug).toBe("Clubria/ai-builders-hub");
  });

  test("claude settings come back parsed, with their timestamp", async () => {
    const t = setup();
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t, { role: "candidate" });
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t, { role: "developer" });
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId);
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
    const { memberId } = await seedMember(t);
    const token = await issueSession(t, memberId);
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
        memberId: other.memberId,
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
      memberId: subject.memberId,
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
        memberId: lead.memberId,
        role: "candidate",
      }),
    ).rejects.toThrow(/another lead/i);
  });

  test("suspending kills live sessions immediately", async () => {
    const t = setup();
    const lead = await seedMember(t, { login: "lead", role: "lead" });
    const subject = await seedMember(t, { login: "grace" });
    const token = await issueSession(t, subject.memberId);
    const asLead = t.withIdentity({ subject: `${lead.userId}|session` });

    await asLead.mutation(api.members.setStatus, {
      memberId: subject.memberId,
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

  test("internal member lookup returns the stored profile", async () => {
    const t = setup();
    const { memberId } = await seedMember(t);
    const member = await t.query(internal.members.byId, { memberId });
    expect(member?.githubLogin).toBe("ada");
  });
});

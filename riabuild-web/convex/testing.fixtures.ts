/// <reference types="vite/client" />
import { convexTest } from "convex-test";
import { afterEach, vi } from "vitest";
import { Id } from "./_generated/dataModel";
import schema from "./schema";
import { randomToken, sha256Hex } from "./lib/crypto";

/**
 * The fixtures every Convex test suite shares.
 *
 * `setup`, `seedMember` and `issueSession` used to be copy-pasted into five
 * files, and the copies had already drifted: three seeded every member with
 * `githubId: "1234"`, one derived it from the login and one prefixed it with
 * `gh-`, which decides whether `findByGithub` matches. One definition with the
 * divergence named in an argument is the point of this file — a caller that
 * needs a distinct id per login now says so, instead of owning a fork of the
 * fixture that says it silently.
 *
 * The name carries two dots on purpose. Convex's bundler skips any file in
 * `convex/` whose basename has more than one, which is what keeps a module
 * importing `convex-test` and `vitest` out of the deployment — the same reason
 * `lib/opensshKey.fixtures.ts` is spelled that way.
 */

export const modules = import.meta.glob("./**/*.ts");

export type Role = "candidate" | "developer" | "lead";

export function setup() {
  return convexTest(schema, modules);
}

/** The handle `convexTest` hands back, named so fixtures can take it. */
export type TestConvex = ReturnType<typeof setup>;

/**
 * `Response.json()` is declared `Promise<any>` by the DOM lib, so every
 * assertion read out of one — `body.error.code`, `body.member.role` — was an
 * unsafe member access by construction, and the suites were exempted from the
 * five `no-unsafe-*` rules wholesale because of it. Naming the shape at the
 * read costs one type argument and puts them back under the same rules as
 * everything else: a renamed field is a compile error now, rather than an
 * assertion that quietly starts comparing `undefined`.
 */
export async function json<T>(response: Response): Promise<T> {
  return (await response.json()) as T;
}

/** The error envelope every `/api/v1` route sends. */
export type ApiError = {
  error: { code: string; message: string; action?: string };
};

/** The member every payload that carries one carries. */
export type MemberPayload = {
  memberId: string;
  githubLogin: string;
  role: Role;
};

/** `POST /api/v1/cli/device`. */
export type DeviceCodes = {
  deviceCode: string;
  userCode: string;
  verificationUriComplete: string;
  interval: number;
};

/** `POST /api/v1/cli/token`, once the developer has answered. */
export type TokenGrant = {
  status: string;
  token: string;
  sessionId: string;
  member: MemberPayload;
};

/** `POST /api/v1/cli/sessions` — the session a laptop mints for a server. */
export type DelegatedSession = {
  token: string;
  sessionId: string;
  expiresAt: number;
  member: MemberPayload;
};

/** `GET /api/v1/org/config`. */
export type OrgConfigBody = {
  repoSlug: string;
  defaultProjectPath: string;
  minCliVersion: string;
  latestCliVersion: string;
  secretEnvironments: string[];
  ngrokAuthTokenUpdatedAt: number;
};

/**
 * The Claude settings, as a lead's row stores them and as
 * `GET /api/v1/org/claude-settings` sends them. Only the keys asserted on:
 * this is a test's view of the payload, not the product's schema.
 */
export type ClaudeSettings = {
  theme: string;
  permissions: { defaultMode: string; deny: string[] };
  skipDangerousModePermissionPrompt: boolean;
  statusLine: { type: string; command: string };
  env: Record<string, string>;
  /**
   * The session's model — `opus`, with `env.CLAUDE_CODE_SUBAGENT_MODEL` set to
   * `sonnet` beside it.
   *
   * Optional because a row stored before `backfillClaudeDefaults` ran genuinely
   * has no `model` at its top level, which is the state the backfill tests seed
   * and the reason that key arrives through `fillMissing` rather than being
   * present on every row the way `env` is.
   */
  model?: string;
};

/** `JSON.parse` is `any` for the same reason `Response.json()` is. */
export function parseSettings(stored: string): ClaudeSettings {
  return JSON.parse(stored) as ClaudeSettings;
}

/** `POST /api/v1/secrets/token`. */
export type SecretsToken = {
  token: string;
  projectId: string;
  expiresAt: number;
  environment: string;
  environments: string[];
};

/** `GET /api/v1/remotes/shared`. */
export type SharedServers = {
  servers: {
    id: string;
    name: string;
    host: string;
    port: number;
    user: string;
  }[];
};

/** `GET /api/v1/issued-keys`. */
export type IssuedKeys = {
  keys: {
    id: string;
    label: string;
    keyType: string;
    publicKey: string;
    privateKey: string;
    fingerprint: string;
  }[];
};

/**
 * One member row, plus the `users` row a browser identity needs.
 *
 * `githubId` is an argument rather than a constant because it is the field
 * `findByGithub` matches on first: a suite that signs two different logins in
 * needs two ids, and one that only ever seeds one member does not care. The
 * default is the shared `"1234"` the majority of call sites already assumed.
 */
export async function seedMember(
  t: TestConvex,
  overrides: {
    login?: string;
    githubId?: string;
    role?: Role;
    status?: "active" | "suspended";
  } = {},
) {
  const login = overrides.login ?? "ada";
  return await t.run(async (ctx) => {
    const userId = await ctx.db.insert("users", {
      name: "Ada Lovelace",
      email: `${login}@clubria.dev`,
    });
    // `rowId` — not `memberId` — because `members.memberId` is a distinct UUID
    // field on the row itself; see the schema comment.
    const rowId = await ctx.db.insert("members", {
      userId,
      githubLogin: login,
      githubId: overrides.githubId ?? "1234",
      memberId: crypto.randomUUID(),
      firstName: "Ada",
      lastName: "Lovelace",
      email: `${login}@clubria.dev`,
      role: overrides.role ?? "developer",
      status: overrides.status ?? "active",
    });
    return { userId, rowId };
  });
}

/** Mints a live session the way `/api/v1/cli/token` would, minus the browser. */
export async function issueSession(
  t: TestConvex,
  memberId: Id<"members">,
  options: {
    expiresAt?: number;
    revoked?: boolean;
    deviceLabel?: string;
    origin?: "device" | "delegated";
  } = {},
) {
  const token = randomToken(32);
  const tokenHash = await sha256Hex(token);
  const sessionId = await t.run(async (ctx) => {
    return await ctx.db.insert("cliSessions", {
      memberId,
      tokenHash,
      deviceLabel: options.deviceLabel ?? "ada-mbp",
      cliVersion: "0.1.0",
      lastUsedAt: 0,
      expiresAt: options.expiresAt ?? Date.now() + 60_000,
      revokedAt: options.revoked === true ? Date.now() : undefined,
      origin: options.origin,
    });
  });
  return { token, sessionId };
}

/** A version no floor will ever refuse. */
export const CURRENT_VERSION = "9999.0.0";

/**
 * What a real CLI sends: a bearer token *and* the version it is.
 *
 * The version defaults to one no floor will ever refuse, because an omitted
 * `x-riabuild-cli-version` is no longer an exemption — it counts as version
 * `0` and every route turns it away with a 409. Leaving it off by default
 * would mean most of every suite testing the version floor over and over
 * instead of the thing each test is named after. Passing `null` omits the
 * header on purpose, which is how the tests that are about the floor ask for
 * it.
 */
export function bearer(
  token: string,
  version: string | null = CURRENT_VERSION,
): HeadersInit {
  const headers: Record<string, string> = { Authorization: `Bearer ${token}` };
  if (version !== null) headers["x-riabuild-cli-version"] = version;
  return headers;
}

/** The same, for a request that carries no session at all. */
export const currentVersion: HeadersInit = {
  "x-riabuild-cli-version": CURRENT_VERSION,
};

/**
 * Stands in for an upstream riabuild-web calls out to — api.github.com,
 * Infisical — for the length of one test.
 *
 * Through `vi.stubGlobal` rather than by assigning `globalThis.fetch`, because
 * the `afterEach` below then puts the real one back whatever happens. Eight
 * describe blocks used to save `globalThis.fetch` themselves and restore it in
 * a teardown of their own; a block that forgot one, or a test that threw
 * before the assignment landed, leaked its stub into whatever ran next in the
 * same worker — and the test that then failed was never the test that broke
 * it.
 */
export type FetchStub = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export function stubFetch(handler: FetchStub): void {
  vi.stubGlobal("fetch", handler);
}

/**
 * Stands in for GitHub's org membership check, which every route that hands
 * out access re-runs. 204 is "yes"; anything else is the failure the caller
 * wants to see. Any other URL throws rather than reaching the network.
 */
export function stubMembership(status: number): void {
  stubFetch(async (input) => {
    const url = input instanceof Request ? input.url : input.toString();
    if (url.includes("api.github.com")) {
      return new Response(null, { status });
    }
    throw new Error(`unexpected fetch to ${url}`);
  });
}

/** The audit trail, as the suites that assert on it want to read it. */
export async function auditRows(t: TestConvex) {
  return await t.run(async (ctx) => {
    const rows = await ctx.db.query("auditLog").collect();
    return rows.map((row) => ({ action: row.action, meta: row.meta }));
  });
}

/**
 * Every stub taken out through this module is undone here, and nowhere else.
 *
 * Registered at import time, so it lands on the root suite of whichever test
 * file pulled the fixtures in — which is every file that can reach
 * `stubFetch`. There is deliberately no way to take a stub from here without
 * also getting the teardown for it.
 */
afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

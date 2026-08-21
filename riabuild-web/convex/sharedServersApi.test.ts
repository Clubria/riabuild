import { beforeEach, describe, expect, test, vi } from "vitest";
import { Id } from "./_generated/dataModel";
import {
  ApiError,
  bearer,
  currentVersion,
  issueSession,
  json,
  seedMember,
  setup,
  SharedServers,
  stubMembership,
  TestConvex,
} from "./testing.fixtures";

/**
 * `GET /api/v1/remotes/shared`: what a CLI is told about the team's servers,
 * which is an address and never a credential.
 *
 * Split out of the old `api.test.ts`. `sharedServers.test.ts` covers the
 * dashboard side — who may edit the list and what a lead is allowed to type.
 */

describe("the team's shared servers", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  async function seedServer(
    t: TestConvex,
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
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    const body = await json<SharedServers>(response);
    // Sorted by name, so the picker's numbering is stable between runs rather
    // than following whatever order the rows happen to come back in.
    expect(body.servers.map((server) => server.name)).toEqual(["build", "gpu"]);
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
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect((await json<SharedServers>(response)).servers).toHaveLength(1);
  });

  test("a candidate gets an empty list rather than a refusal", async () => {
    // 200 and { servers: [] }, never 403. `riabuild remote` is also how a
    // candidate reaches the server they set up themselves, and refusing the
    // whole request would take that away in order to enforce a rule about
    // servers they were never going to see.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    await seedServer(t, rowId);
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await json<SharedServers>(response)).toEqual({ servers: [] });
  });

  test("someone who has left the GitHub org gets 403, not a server list", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedServer(t, rowId);
    const { token } = await issueSession(t, rowId);
    stubMembership(404);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(403);
    const body = await json<ApiError>(response);
    expect(body.error.code).toBe("not_org_member");
  });

  test("no session at all gets 401", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: currentVersion,
    });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("unauthenticated");
  });

  test("a revoked session gets 401", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId, { revoked: true });

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(401);
  });

  test("a CLI below the version floor is told to upgrade", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
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
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/remotes/shared", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await json<SharedServers>(response)).toEqual({ servers: [] });
  });
});

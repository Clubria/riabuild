import { beforeEach, describe, expect, test, vi } from "vitest";
import { Id } from "./_generated/dataModel";
import {
  ED25519_FINGERPRINT,
  ED25519_PRIVATE,
  ED25519_PUBLIC,
} from "./lib/opensshKey.fixtures";
import {
  ApiError,
  bearer,
  currentVersion,
  issueSession,
  IssuedKeys,
  json,
  seedMember,
  setup,
  stubMembership,
  TestConvex,
} from "./testing.fixtures";

/**
 * `GET /api/v1/issued-keys`: the one response that hands a CLI a durable
 * private key, so the org re-check and the audit row are doing the whole job.
 *
 * Split out of the old `api.test.ts`. `issuedKeys.test.ts` covers the
 * dashboard side — pasting a key, deriving its public half, and who it is
 * issued to.
 */

describe("the SSH keys the org issues", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
  });

  async function seedKey(
    t: TestConvex,
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
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    const body = await json<IssuedKeys>(response);
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
    const { token } = await issueSession(t, ada);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await json<IssuedKeys>(response)).toEqual({ keys: [] });
  });

  test("a candidate gets an empty list rather than a refusal", async () => {
    // 200 and `{ keys: [] }`, never 403 — the rule /api/v1/remotes/shared
    // already sets. `riabuild remote` is also how a candidate reaches the
    // server they set up themselves.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    await seedKey(t, rowId, [rowId]);
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await json<IssuedKeys>(response)).toEqual({ keys: [] });
  });

  test("someone who has left the GitHub org gets 403, not a private key", async () => {
    // The one that matters most on this endpoint. This is the only response in
    // riabuild carrying a durable credential, so `members.role` being stale
    // must not be enough to keep it flowing.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedKey(t, rowId, [rowId]);
    const { token } = await issueSession(t, rowId);
    stubMembership(404);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(403);
    const body = await json<ApiError>(response);
    expect(body.error.code).toBe("not_org_member");
    expect(JSON.stringify(body)).not.toContain("BEGIN OPENSSH");
  });

  test("no session at all gets 401", async () => {
    const t = setup();
    const response = await t.fetch("/api/v1/issued-keys", {
      headers: currentVersion,
    });
    expect(response.status).toBe(401);
    expect((await json<ApiError>(response)).error.code).toBe("unauthenticated");
  });

  test("a revoked session gets 401", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    await seedKey(t, rowId, [rowId]);
    const { token } = await issueSession(t, rowId, { revoked: true });

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
    const { token } = await issueSession(t, rowId);
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
    const { token } = await issueSession(t, rowId);
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
    const { token } = await issueSession(t, rowId);
    stubMembership(204);

    const response = await t.fetch("/api/v1/issued-keys", {
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    expect(await json<IssuedKeys>(response)).toEqual({ keys: [] });
  });
});

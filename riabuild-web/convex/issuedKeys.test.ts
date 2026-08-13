/// <reference types="vite/client" />
import { convexTest } from "convex-test";
import { describe, expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import schema from "./schema";
import {
  ED25519_FINGERPRINT,
  ED25519_PRIVATE,
  ED25519_PUBLIC,
  ENCRYPTED_PRIVATE,
  RSA_FINGERPRINT,
  RSA_PRIVATE,
} from "./lib/opensshKey.fixtures";

const modules = import.meta.glob("./**/*.ts");

function setup() {
  return convexTest(schema, modules);
}

async function seedMember(
  t: ReturnType<typeof setup>,
  login: string,
  role: "candidate" | "developer" | "lead",
) {
  return await t.run(async (ctx) => {
    const userId = await ctx.db.insert("users", {
      name: login,
      email: `${login}@clubria.dev`,
    });
    const rowId = await ctx.db.insert("members", {
      userId,
      githubLogin: login,
      githubId: login,
      memberId: crypto.randomUUID(),
      firstName: login,
      lastName: "Tester",
      email: `${login}@clubria.dev`,
      role,
      status: "active",
    });
    return { userId, rowId };
  });
}

async function asLead(t: ReturnType<typeof setup>) {
  const { userId, rowId } = await seedMember(t, "grace", "lead");
  return { as: t.withIdentity({ subject: `${userId}|session` }), id: rowId };
}

async function auditRows(t: ReturnType<typeof setup>) {
  return await t.run(async (ctx) => {
    const rows = await ctx.db.query("auditLog").collect();
    return rows.map((row) => ({ action: row.action, meta: row.meta }));
  });
}

const BASTION = { label: "prod-bastion", privateKey: ED25519_PRIVATE };

describe("what a lead may read", () => {
  test("never returns a private key to the dashboard", async () => {
    // The single most important test in this file. `list` returns a projection
    // that has no such field, rather than a document with the field stripped
    // at a call site a later caller could forget.
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.issuedKeys.create, BASTION);

    const rows = await lead.query(api.issuedKeys.list, {});

    expect(rows).toHaveLength(1);
    expect(rows[0]).not.toHaveProperty("privateKey");
    expect(JSON.stringify(rows)).not.toContain("BEGIN OPENSSH");
  });

  test("a developer cannot read the dashboard's list", async () => {
    const t = setup();
    await asLead(t);
    const { userId } = await seedMember(t, "ada", "developer");
    const developer = t.withIdentity({ subject: `${userId}|session` });

    await expect(developer.query(api.issuedKeys.list, {})).rejects.toThrow(
      /lead/i,
    );
  });

  test("a developer cannot add a key", async () => {
    const t = setup();
    const { userId } = await seedMember(t, "ada", "developer");
    const developer = t.withIdentity({ subject: `${userId}|session` });

    await expect(
      developer.mutation(api.issuedKeys.create, BASTION),
    ).rejects.toThrow(/lead/i);
  });
});

describe("what is stored", () => {
  test("derives the public half and ignores whatever the client claims", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);

    const id = await lead.mutation(api.issuedKeys.create, BASTION);

    const row = await t.run(async (ctx) => ctx.db.get("issuedKeys", id));
    expect(row?.publicKey).toBe(ED25519_PUBLIC);
    expect(row?.fingerprint).toBe(ED25519_FINGERPRINT);
    expect(row?.keyType).toBe("ssh-ed25519");
    expect(row?.privateKey).toBe(ED25519_PRIVATE);
  });

  test("refuses a passphrase-protected key at the door", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);

    await expect(
      lead.mutation(api.issuedKeys.create, {
        label: "nope",
        privateKey: ENCRYPTED_PRIVATE,
      }),
    ).rejects.toThrow(/passphrase/i);
  });

  test("refuses a label two keys could not be told apart by", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.issuedKeys.create, BASTION);

    await expect(
      lead.mutation(api.issuedKeys.create, {
        label: "prod-bastion",
        privateKey: RSA_PRIVATE,
      }),
    ).rejects.toThrow(/already/i);
  });

  test("refuses a label that is not a label", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);

    for (const label of ["", " ", "a".repeat(33), "has space", "sl/ash"]) {
      await expect(
        lead.mutation(api.issuedKeys.create, {
          label,
          privateKey: ED25519_PRIVATE,
        }),
      ).rejects.toThrow(/name/i);
    }
  });

  test("keeps the label and the grants when a key is replaced", async () => {
    // `replaceKey` is how rotation happens: the same row, the same people, a
    // new secret. A lead who had to delete and re-add would silently drop
    // everyone's access in between.
    const t = setup();
    const { as: lead } = await asLead(t);
    const { rowId: ada } = await seedMember(t, "ada", "developer");
    const id = await lead.mutation(api.issuedKeys.create, BASTION);
    await lead.mutation(api.issuedKeys.setIssuedTo, { id, issuedTo: [ada] });

    await lead.mutation(api.issuedKeys.replaceKey, {
      id,
      privateKey: RSA_PRIVATE,
    });

    const row = await t.run(async (ctx) => ctx.db.get("issuedKeys", id));
    expect(row?.label).toBe("prod-bastion");
    expect(row?.issuedTo).toEqual([ada]);
    expect(row?.fingerprint).toBe(RSA_FINGERPRINT);
    expect(row?.keyType).toBe("ssh-rsa");
  });

  test("refuses a grant to a member who does not exist", async () => {
    // A dangling id would serve nobody and be invisible in the panel — the
    // lead would believe someone had a key they could not use.
    const t = setup();
    const { as: lead } = await asLead(t);
    const id = await lead.mutation(api.issuedKeys.create, BASTION);
    const { rowId: ada } = await seedMember(t, "ada", "developer");
    await t.run(async (ctx) => ctx.db.delete("members", ada));

    await expect(
      lead.mutation(api.issuedKeys.setIssuedTo, { id, issuedTo: [ada] }),
    ).rejects.toThrow(/no longer/i);
  });
});

describe("what a developer is served", () => {
  test("serves a member only the keys issued to them, and audits it by label", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const { rowId: ada } = await seedMember(t, "ada", "developer");
    const { rowId: alan } = await seedMember(t, "alan", "developer");

    const bastion = await lead.mutation(api.issuedKeys.create, BASTION);
    const gpu = await lead.mutation(api.issuedKeys.create, {
      label: "gpu-box",
      privateKey: RSA_PRIVATE,
    });
    await lead.mutation(api.issuedKeys.setIssuedTo, {
      id: bastion,
      issuedTo: [ada],
    });
    await lead.mutation(api.issuedKeys.setIssuedTo, {
      id: gpu,
      issuedTo: [alan],
    });

    const served = await t.run(async (ctx) =>
      ctx.runMutation(internal.issuedKeys.serveForApi, { memberId: ada }),
    );

    expect(served.map((key) => key.label)).toEqual(["prod-bastion"]);
    expect(served[0].privateKey).toContain("BEGIN OPENSSH");
    expect(served[0].fingerprint).toBe(ED25519_FINGERPRINT);

    // A log of grants answers who was *entitled* to a key. This answers who
    // took a copy of one, which is the question actually asked afterwards.
    const audit = await auditRows(t);
    const fetch = audit.find((row) => row.action === "issued_key.served");
    expect(fetch?.meta.keys).toBe("prod-bastion");
    expect(fetch?.meta.count).toBe("1");
  });

  test("serves nothing to a member named on no row", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const { rowId: ada } = await seedMember(t, "ada", "developer");
    await lead.mutation(api.issuedKeys.create, BASTION);

    const served = await t.run(async (ctx) =>
      ctx.runMutation(internal.issuedKeys.serveForApi, { memberId: ada }),
    );

    expect(served).toEqual([]);
    const fetch = (await auditRows(t)).find(
      (row) => row.action === "issued_key.served",
    );
    expect(fetch?.meta.count).toBe("0");
  });

  test("serves keys in a stable order", async () => {
    // The CLI probes them in the order it receives them, and a run that
    // reordered on every fetch would get in with a different key each time.
    const t = setup();
    const { as: lead } = await asLead(t);
    const { rowId: ada } = await seedMember(t, "ada", "developer");
    for (const [label, privateKey] of [
      ["zulu", ED25519_PRIVATE],
      ["alpha", RSA_PRIVATE],
    ] as const) {
      const id = await lead.mutation(api.issuedKeys.create, {
        label,
        privateKey,
      });
      await lead.mutation(api.issuedKeys.setIssuedTo, { id, issuedTo: [ada] });
    }

    const served = await t.run(async (ctx) =>
      ctx.runMutation(internal.issuedKeys.serveForApi, { memberId: ada }),
    );

    expect(served.map((key) => key.label)).toEqual(["alpha", "zulu"]);
  });
});

describe("the audit trail", () => {
  test("writes a row for every change, naming the key", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const { rowId: ada } = await seedMember(t, "ada", "developer");

    const id = await lead.mutation(api.issuedKeys.create, BASTION);
    await lead.mutation(api.issuedKeys.setIssuedTo, { id, issuedTo: [ada] });
    await lead.mutation(api.issuedKeys.replaceKey, {
      id,
      privateKey: RSA_PRIVATE,
    });
    await lead.mutation(api.issuedKeys.setIssuedTo, { id, issuedTo: [] });
    await lead.mutation(api.issuedKeys.remove, { id });

    const audit = await auditRows(t);
    expect(audit.map((row) => row.action)).toEqual([
      "issued_key.created",
      "issued_key.issued",
      "issued_key.replaced",
      "issued_key.issued",
      "issued_key.removed",
    ]);
    expect(audit[0].meta).toMatchObject({
      label: "prod-bastion",
      fingerprint: ED25519_FINGERPRINT,
    });
    expect(audit[1].meta).toMatchObject({ added: "ada", removed: "" });
    expect(audit[2].meta).toMatchObject({
      from: ED25519_FINGERPRINT,
      fingerprint: RSA_FINGERPRINT,
    });
    expect(audit[3].meta).toMatchObject({ added: "", removed: "ada" });
  });

  test("never writes a private key into the audit log", async () => {
    // An audit row is the one place a secret could leak into something read
    // casually, exported, and kept far longer than the key itself.
    const t = setup();
    const { as: lead } = await asLead(t);
    const id = await lead.mutation(api.issuedKeys.create, BASTION);
    await lead.mutation(api.issuedKeys.remove, { id });

    expect(JSON.stringify(await auditRows(t))).not.toContain("BEGIN OPENSSH");
  });
});

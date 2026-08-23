import { describe, expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import { claimOrCreateMember } from "./members";
import { ED25519_PRIVATE } from "./lib/opensshKey.fixtures";
import {
  Role,
  seedMember as seedMemberRow,
  setup,
  TestConvex,
} from "./testing.fixtures";

/**
 * An invitation and the sign-in that adopts it: the row a lead creates before
 * anyone has authenticated, and what `claimOrCreateMember` does when the
 * person it names finally arrives.
 */

/**
 * A `gh-` prefixed `githubId` per login, so a seeded row is never mistaken for
 * one `signIn` below is about to claim — `findByGithub` matches that field
 * before it looks at the login.
 */
async function seedMember(t: TestConvex, login: string, role: Role) {
  return await seedMemberRow(t, { login, githubId: `gh-${login}`, role });
}

async function asLead(t: TestConvex) {
  const { userId, rowId } = await seedMember(t, "grace", "lead");
  return { as: t.withIdentity({ subject: `${userId}|session` }), id: rowId };
}

/** The sign-in, as far as the `members` table is concerned. */
async function signIn(
  t: TestConvex,
  args: {
    githubLogin: string;
    githubId: string;
    name?: string;
    email?: string;
    isBootstrapLead?: boolean;
  },
) {
  return await t.run(async (ctx) => {
    const userId = await ctx.db.insert("users", {
      name: args.name ?? args.githubLogin,
      email: args.email ?? `${args.githubLogin}@clubria.dev`,
    });
    await claimOrCreateMember(ctx, {
      userId,
      githubLogin: args.githubLogin,
      githubId: args.githubId,
      name: args.name ?? args.githubLogin,
      email: args.email ?? `${args.githubLogin}@clubria.dev`,
      isBootstrapLead: args.isBootstrapLead ?? false,
    });
    return userId;
  });
}

async function memberRows(t: TestConvex) {
  return await t.run(async (ctx) => await ctx.db.query("members").collect());
}

async function auditActions(t: TestConvex) {
  return await t.run(async (ctx) => {
    const rows = await ctx.db.query("auditLog").collect();
    return rows.map((row) => row.action);
  });
}

const PRIYA = { githubLogin: "priya", githubId: "4815162342" };

describe("recording a role before anyone signs in", () => {
  test("a lead can invite somebody who has never been here", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);

    await lead.mutation(api.members.invite, { ...PRIYA, role: "developer" });

    const listed = await lead.query(api.members.list, {});
    const priya = listed.find((m) => m.githubLogin === "priya");
    expect(priya).toBeDefined();
    expect(priya?.role).toBe("developer");
    expect(priya?.invited).toBe(true);
  });

  test("nobody but a lead can", async () => {
    const t = setup();
    await asLead(t);
    const { userId } = await seedMember(t, "ada", "developer");
    const developer = t.withIdentity({ subject: `${userId}|session` });

    await expect(
      developer.mutation(api.members.invite, { ...PRIYA, role: "lead" }),
    ).rejects.toThrow(/Only team leads/);
  });

  test("inviting somebody who is already here is refused", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await seedMember(t, "ada", "developer");

    await expect(
      lead.mutation(api.members.invite, {
        githubLogin: "ada",
        githubId: "gh-ada",
        role: "lead",
      }),
    ).rejects.toThrow(/already a member/);
  });

  test("inviting the same person twice is refused", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.members.invite, { ...PRIYA, role: "developer" });

    await expect(
      lead.mutation(api.members.invite, { ...PRIYA, role: "lead" }),
    ).rejects.toThrow(/already been invited/);
  });

  test("a login GitHub would not issue is refused", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);

    await expect(
      lead.mutation(api.members.invite, {
        githubLogin: "not a login",
        githubId: "1",
        role: "developer",
      }),
    ).rejects.toThrow(/not a GitHub username/);
  });
});

describe("what an invitation does not grant", () => {
  /**
   * The invariant the whole feature rests on. An invited row has no `userId`,
   * and `viewerMember` looks up `by_userId` — so if an absent field ever
   * started matching a real user id, an invited `lead` would become a live
   * administrator for whoever signed in next.
   */
  test("an invited lead is nobody's session", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.members.invite, { ...PRIYA, role: "lead" });

    const strangerId = await t.run(
      async (ctx) =>
        await ctx.db.insert("users", {
          name: "stranger",
          email: "stranger@clubria.dev",
        }),
    );
    const stranger = t.withIdentity({ subject: `${strangerId}|session` });

    expect(await stranger.query(api.members.viewer, {})).toBeNull();
    await expect(stranger.query(api.members.list, {})).rejects.toThrow(
      /Not signed in|Only team leads/,
    );
  });
});

describe("issuing a key before the person arrives", () => {
  test("a key can be granted in the same act as the invitation", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const keyId = await lead.mutation(api.issuedKeys.create, {
      label: "prod-bastion",
      privateKey: ED25519_PRIVATE,
    });

    await lead.mutation(api.members.invite, {
      ...PRIYA,
      role: "developer",
      issuedKeys: [keyId],
    });

    const [key] = await lead.query(api.issuedKeys.list, {});
    const [priya] = (await lead.query(api.members.list, {})).filter(
      (m) => m.githubLogin === "priya",
    );
    expect(key.issuedTo).toEqual([priya._id]);
    expect(await auditActions(t)).toContain("issued_key.issued");
  });

  /**
   * The end-to-end claim this feature makes: a key granted days before
   * somebody's first sign-in is served to their CLI on the first run, with no
   * lead action in between.
   */
  test("the key reaches them on their first run", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const keyId = await lead.mutation(api.issuedKeys.create, {
      label: "prod-bastion",
      privateKey: ED25519_PRIVATE,
    });
    await lead.mutation(api.members.invite, {
      ...PRIYA,
      role: "developer",
      issuedKeys: [keyId],
    });

    await signIn(t, PRIYA);

    const priya = (await memberRows(t)).find((m) => m.githubLogin === "priya");
    const served = await t.mutation(internal.issuedKeys.serveForApi, {
      memberId: priya!._id,
    });
    expect(served.map((row) => row.label)).toEqual(["prod-bastion"]);
  });
});

describe("the sign-in that claims an invitation", () => {
  test("adopts the row instead of making a second one", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.members.invite, { ...PRIYA, role: "developer" });
    const before = (await memberRows(t)).find((m) => m.githubLogin === "priya");

    await signIn(t, { ...PRIYA, name: "Priya Raman" });

    const rows = (await memberRows(t)).filter((m) => m.githubLogin === "priya");
    expect(rows).toHaveLength(1);
    expect(rows[0]._id).toBe(before!._id);
    expect(rows[0].userId).toBeDefined();
    expect(rows[0].role).toBe("developer");
    // The invitation left these blank; the sign-in is what fills them.
    expect(rows[0].firstName).toBe("Priya");
    expect(rows[0].lastName).toBe("Raman");
    expect(await auditActions(t)).toContain("member.joined");
  });

  test("no longer reads as invited afterwards", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.members.invite, { ...PRIYA, role: "developer" });
    await signIn(t, PRIYA);

    const priya = (await lead.query(api.members.list, {})).find(
      (m) => m.githubLogin === "priya",
    );
    expect(priya?.invited).toBe(false);
  });

  /**
   * The reason adoption matches on the numeric id. Somebody can rename their
   * GitHub account between the invitation and their first sign-in, and a login
   * match alone would leave the invitation stranded — still holding their key.
   */
  test("still claims the invitation after a GitHub rename", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.members.invite, { ...PRIYA, role: "lead" });

    await signIn(t, { githubLogin: "priya-r", githubId: PRIYA.githubId });

    const rows = await memberRows(t);
    const claimed = rows.filter((m) => m.githubId === PRIYA.githubId);
    expect(claimed).toHaveLength(1);
    expect(claimed[0].githubLogin).toBe("priya-r");
    expect(claimed[0].role).toBe("lead");
    expect(claimed[0].userId).toBeDefined();
  });

  test("somebody nobody invited still arrives as a candidate", async () => {
    const t = setup();
    await asLead(t);

    await signIn(t, { githubLogin: "wren", githubId: "99" });

    const wren = (await memberRows(t)).find((m) => m.githubLogin === "wren");
    expect(wren?.role).toBe("candidate");
    expect(await auditActions(t)).toContain("member.created");
  });

  test("a bootstrap lead is still promoted over the invited role", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    await lead.mutation(api.members.invite, { ...PRIYA, role: "candidate" });

    await signIn(t, { ...PRIYA, isBootstrapLead: true });

    const priya = (await memberRows(t)).find((m) => m.githubLogin === "priya");
    expect(priya?.role).toBe("lead");
  });

  /**
   * A claimed row belongs to whoever claimed it. Matching must never hand a
   * second sign-in somebody else's member row, and their access with it.
   */
  test("never takes a row that already has a user behind it", async () => {
    const t = setup();
    await asLead(t);
    const { rowId } = await seedMember(t, "ada", "lead");

    await signIn(t, { githubLogin: "ada", githubId: "someone-else" });

    const rows = await memberRows(t);
    const ada = rows.filter((m) => m.githubLogin === "ada");
    // Two distinct people who happen to collide are two rows, and the original
    // keeps its role rather than being handed over.
    expect(ada.find((m) => m._id === rowId)?.role).toBe("lead");
  });
});

describe("withdrawing an invitation", () => {
  test("removes the row and takes its key grants with it", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const keyId = await lead.mutation(api.issuedKeys.create, {
      label: "prod-bastion",
      privateKey: ED25519_PRIVATE,
    });
    const memberId = await lead.mutation(api.members.invite, {
      ...PRIYA,
      role: "developer",
      issuedKeys: [keyId],
    });

    await lead.mutation(api.members.removeInvite, { memberId });

    expect((await memberRows(t)).some((m) => m.githubLogin === "priya")).toBe(
      false,
    );
    const [key] = await lead.query(api.issuedKeys.list, {});
    // Left behind, this id makes `setIssuedTo` throw for everyone, on behalf of
    // somebody who no longer exists.
    expect(key.issuedTo).toEqual([]);
    expect(await auditActions(t)).toContain("member.invite_withdrawn");
  });

  test("refuses somebody who has already signed in", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const { rowId } = await seedMember(t, "ada", "developer");

    await expect(
      lead.mutation(api.members.removeInvite, { memberId: rowId }),
    ).rejects.toThrow(/already signed in/);
  });

  test("a lead who withdraws can re-issue that key to somebody else", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const { rowId: adaId } = await seedMember(t, "ada", "developer");
    const keyId = await lead.mutation(api.issuedKeys.create, {
      label: "prod-bastion",
      privateKey: ED25519_PRIVATE,
    });
    const memberId = await lead.mutation(api.members.invite, {
      ...PRIYA,
      role: "developer",
      issuedKeys: [keyId],
    });
    await lead.mutation(api.members.removeInvite, { memberId });

    // The regression this guards: a dangling id would make this throw.
    await lead.mutation(api.issuedKeys.setIssuedTo, {
      id: keyId,
      issuedTo: [adaId],
    });

    const [key] = await lead.query(api.issuedKeys.list, {});
    expect(key.issuedTo).toEqual([adaId]);
  });

  test("nobody but a lead can withdraw", async () => {
    const t = setup();
    const { as: lead } = await asLead(t);
    const memberId = await lead.mutation(api.members.invite, {
      ...PRIYA,
      role: "developer",
    });
    const { userId } = await seedMember(t, "ada", "developer");
    const developer = t.withIdentity({ subject: `${userId}|session` });

    await expect(
      developer.mutation(api.members.removeInvite, { memberId }),
    ).rejects.toThrow(/Only team leads/);
  });
});

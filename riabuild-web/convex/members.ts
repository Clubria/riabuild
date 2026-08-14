import { v } from "convex/values";
import { getAuthUserId } from "@convex-dev/auth/server";
import {
  internalMutation,
  internalQuery,
  mutation,
  query,
  MutationCtx,
  QueryCtx,
} from "./_generated/server";
import { Doc, Id } from "./_generated/dataModel";
import { roleValidator, statusValidator } from "./schema";

export const memberView = v.object({
  _id: v.id("members"),
  memberId: v.string(),
  githubLogin: v.string(),
  githubId: v.string(),
  firstName: v.string(),
  lastName: v.string(),
  email: v.string(),
  role: roleValidator,
  status: statusValidator,
  joinedAt: v.number(),
  /**
   * True while nobody has signed in as this person yet — a lead invited them
   * and recorded a role, and possibly a key, in advance.
   *
   * Derived from the absence of `userId` rather than stored, so it cannot drift
   * away from the thing it describes. `joinedAt` is the row's creation time and
   * for an invited row means "invited at"; the panel labels it accordingly
   * rather than the two of them disagreeing about which day matters.
   */
  invited: v.boolean(),
});

export function toView(member: Doc<"members">) {
  return {
    _id: member._id,
    memberId: member.memberId,
    githubLogin: member.githubLogin,
    githubId: member.githubId,
    firstName: member.firstName,
    lastName: member.lastName,
    email: member.email,
    role: member.role,
    status: member.status,
    joinedAt: member._creationTime,
    invited: member.userId === undefined,
  };
}

export async function viewerMember(
  ctx: QueryCtx,
): Promise<Doc<"members"> | null> {
  const userId = await getAuthUserId(ctx);
  if (userId === null) return null;
  return await ctx.db
    .query("members")
    .withIndex("by_userId", (q) => q.eq("userId", userId))
    .unique();
}

/** Throws for anyone who is not an active lead. Used by every admin mutation. */
export async function requireLead(ctx: QueryCtx): Promise<Doc<"members">> {
  const member = await viewerMember(ctx);
  if (member === null) throw new Error("Not signed in.");
  if (member.status !== "active") throw new Error("Your account is suspended.");
  if (member.role !== "lead") {
    throw new Error("Only team leads can do that.");
  }
  return member;
}

export const viewer = query({
  args: {},
  returns: v.union(memberView, v.null()),
  handler: async (ctx) => {
    const member = await viewerMember(ctx);
    return member === null ? null : toView(member);
  },
});

export const updateProfile = mutation({
  args: {
    firstName: v.string(),
    lastName: v.string(),
    email: v.string(),
  },
  returns: v.null(),
  handler: async (ctx, args) => {
    const member = await viewerMember(ctx);
    if (member === null) throw new Error("Not signed in.");

    const email = args.email.trim();
    if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) {
      throw new Error("That does not look like an email address.");
    }

    await ctx.db.patch("members", member._id, {
      firstName: args.firstName.trim(),
      lastName: args.lastName.trim(),
      email,
    });
    return null;
  },
});

export const list = query({
  args: {},
  returns: v.array(memberView),
  handler: async (ctx) => {
    await requireLead(ctx);
    const members = await ctx.db.query("members").take(200);
    return members.map(toView);
  },
});

export const setRole = mutation({
  args: { memberId: v.id("members"), role: roleValidator },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const subject = await ctx.db.get("members", args.memberId);
    if (subject === null) throw new Error("No such member.");
    if (subject._id === actor._id && args.role !== "lead") {
      // Otherwise the last lead can quietly lock every lead out of the org.
      throw new Error("Ask another lead to change your own role.");
    }
    if (subject.role === args.role) return null;

    await ctx.db.patch("members", subject._id, { role: args.role });
    await writeAudit(ctx, {
      actorId: actor._id,
      subjectId: subject._id,
      action: "member.role_changed",
      meta: {
        githubLogin: subject.githubLogin,
        from: subject.role,
        to: args.role,
      },
    });
    return null;
  },
});

export const setStatus = mutation({
  args: { memberId: v.id("members"), status: statusValidator },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const subject = await ctx.db.get("members", args.memberId);
    if (subject === null) throw new Error("No such member.");
    if (subject._id === actor._id && args.status === "suspended") {
      throw new Error("You cannot suspend yourself.");
    }
    if (subject.status === args.status) return null;

    await ctx.db.patch("members", subject._id, { status: args.status });

    // Suspension has to reach live CLI sessions, not just the next sign-in.
    if (args.status === "suspended") {
      const sessions = await ctx.db
        .query("cliSessions")
        .withIndex("by_memberId", (q) => q.eq("memberId", subject._id))
        .take(100);
      for (const session of sessions) {
        if (session.revokedAt === undefined) {
          await ctx.db.patch("cliSessions", session._id, {
            revokedAt: Date.now(),
          });
        }
      }
    }

    await writeAudit(ctx, {
      actorId: actor._id,
      subjectId: subject._id,
      action: "member.status_changed",
      meta: {
        githubLogin: subject.githubLogin,
        from: subject.status,
        to: args.status,
      },
    });
    return null;
  },
});

/** GitHub's own rule for a login, so a picked one always passes. */
const GITHUB_LOGIN = /^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/;

/**
 * Records a person's role — and optionally the keys they hold — before they
 * have ever signed in.
 *
 * The row this writes is a real `members` row with no `userId`, which is what
 * makes the SSH half of this feature free: `issuedKeys.issuedTo` holds
 * `Id<"members">`, and the id written here is the same id the row keeps when
 * `auth.ts:upsertMember` adopts it. Nothing has to be migrated at sign-in,
 * which is the one moment where a mistake would silently drop somebody's
 * access.
 *
 * It grants no access on its own. An invited row cannot match `by_userId`, so
 * `viewerMember` — and `requireLead` above it, and every `/api/v1` route behind
 * a session — is unreachable until a real sign-in claims it. An invited `lead`
 * is a decision recorded in advance, not access granted in advance.
 */
export const invite = mutation({
  args: {
    githubLogin: v.string(),
    githubId: v.string(),
    role: roleValidator,
    issuedKeys: v.optional(v.array(v.id("issuedKeys"))),
  },
  returns: v.id("members"),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);

    const githubLogin = args.githubLogin.trim();
    if (!GITHUB_LOGIN.test(githubLogin)) {
      throw new Error(`${args.githubLogin} is not a GitHub username.`);
    }
    const githubId = args.githubId.trim();
    if (githubId === "") {
      throw new Error(
        "That person came without a GitHub id. Reload the page and pick them again.",
      );
    }

    const existing = await findByGithub(ctx, { githubId, githubLogin });
    if (existing !== null) {
      throw new Error(
        existing.userId === undefined
          ? `@${existing.githubLogin} has already been invited.`
          : `@${existing.githubLogin} is already a member.`,
      );
    }

    const memberId = await ctx.db.insert("members", {
      githubLogin,
      githubId,
      memberId: crypto.randomUUID(),
      // Left empty on purpose: these are the developer's to state, and the
      // sign-in fills them from GitHub. Guessing a name from a login would put
      // a wrong one in front of them on the profile screen, already filled in.
      firstName: "",
      lastName: "",
      email: "",
      role: args.role,
      status: "active",
    });

    for (const keyId of args.issuedKeys ?? []) {
      const key = await ctx.db.get("issuedKeys", keyId);
      if (key === null) {
        throw new Error(
          "One of the keys you picked is already gone. Reload the page and try again.",
        );
      }
      if (key.issuedTo.some((id) => id === memberId)) continue;
      await ctx.db.patch("issuedKeys", keyId, {
        issuedTo: [...key.issuedTo, memberId],
        updatedAt: Date.now(),
      });
      await writeAudit(ctx, {
        actorId: actor._id,
        subjectId: memberId,
        action: "issued_key.issued",
        meta: { label: key.label, added: githubLogin, removed: "" },
      });
    }

    await writeAudit(ctx, {
      actorId: actor._id,
      subjectId: memberId,
      action: "member.invited",
      meta: { githubLogin, role: args.role },
    });
    return memberId;
  },
});

/**
 * Withdraws an invitation, and refuses to touch anyone who has arrived.
 *
 * Deleting a member who has signed in would leave live `cliSessions` rows
 * pointing at a member that no longer exists; suspending is the way to remove
 * one of those, and it revokes their sessions on the way past.
 *
 * The grant sweep is not tidiness. A dangling id in `issuedKeys.issuedTo` makes
 * `setIssuedTo` throw "one of the people you picked is no longer a member",
 * which would lock a lead out of editing that key's grants forever — on behalf
 * of somebody who no longer exists.
 */
export const removeInvite = mutation({
  args: { memberId: v.id("members") },
  returns: v.null(),
  handler: async (ctx, args) => {
    const actor = await requireLead(ctx);
    const subject = await ctx.db.get("members", args.memberId);
    if (subject === null) return null;
    if (subject.userId !== undefined) {
      throw new Error(
        `@${subject.githubLogin} has already signed in, so there is no invitation left to withdraw. Suspend them instead.`,
      );
    }

    const keys = await ctx.db.query("issuedKeys").take(200);
    for (const key of keys) {
      if (!key.issuedTo.some((id) => id === subject._id)) continue;
      await ctx.db.patch("issuedKeys", key._id, {
        issuedTo: key.issuedTo.filter((id) => id !== subject._id),
        updatedAt: Date.now(),
      });
    }

    await ctx.db.delete("members", subject._id);
    await writeAudit(ctx, {
      actorId: actor._id,
      action: "member.invite_withdrawn",
      meta: { githubLogin: subject.githubLogin, role: subject.role },
    });
    return null;
  },
});

/**
 * The row an arriving developer should claim, if a lead made one for them.
 *
 * `githubId` first, and it is the match that matters: a developer can rename
 * their GitHub account between the invitation and their first sign-in, and the
 * numeric id cannot change. The login fallback exists for a row invited without
 * a usable id, which nothing writes today — but a duplicate member row is a
 * silent failure, and this costs one indexed lookup to rule out.
 */
export async function findByGithub(
  ctx: QueryCtx,
  args: { githubId: string; githubLogin: string },
): Promise<Doc<"members"> | null> {
  if (args.githubId !== "") {
    const byId = await ctx.db
      .query("members")
      .withIndex("by_githubId", (q) => q.eq("githubId", args.githubId))
      .first();
    if (byId !== null) return byId;
  }
  const wanted = args.githubLogin.toLowerCase();
  const candidates = await ctx.db.query("members").take(200);
  return (
    candidates.find((row) => row.githubLogin.toLowerCase() === wanted) ?? null
  );
}

export const auditLog = query({
  args: { limit: v.optional(v.number()) },
  returns: v.array(
    v.object({
      _id: v.id("auditLog"),
      action: v.string(),
      at: v.number(),
      actorLogin: v.union(v.string(), v.null()),
      subjectLogin: v.union(v.string(), v.null()),
      meta: v.record(v.string(), v.string()),
    }),
  ),
  handler: async (ctx, args) => {
    await requireLead(ctx);
    const entries = await ctx.db
      .query("auditLog")
      .withIndex("by_at")
      .order("desc")
      .take(Math.min(args.limit ?? 50, 200));

    const logins = new Map<Id<"members">, string>();
    const loginFor = async (id: Id<"members"> | undefined) => {
      if (id === undefined) return null;
      const cached = logins.get(id);
      if (cached !== undefined) return cached;
      const member = await ctx.db.get("members", id);
      const login = member?.githubLogin ?? "(deleted)";
      logins.set(id, login);
      return login;
    };

    return await Promise.all(
      entries.map(async (entry) => ({
        _id: entry._id,
        action: entry.action,
        at: entry.at,
        actorLogin: await loginFor(entry.actorId),
        subjectLogin: await loginFor(entry.subjectId),
        meta: entry.meta,
      })),
    );
  },
});

export const byId = internalQuery({
  args: { memberId: v.id("members") },
  returns: v.union(memberView, v.null()),
  handler: async (ctx, args) => {
    const member = await ctx.db.get("members", args.memberId);
    return member === null ? null : toView(member);
  },
});

/**
 * One-shot: gives every member row a `memberId` so the field can be made
 * required. Idempotent, and returns how many rows it changed so the deploy
 * step can be checked rather than assumed.
 *
 * Exists for exactly one production deploy — see the three-step sequence in
 * `docs/deploying.md` §7 (deploy optional, run this against `--prod`, deploy
 * required) — and is verified by that deploy's returned count rather than by
 * this suite: once `members.memberId` is required in the schema,
 * `convex-test` refuses to construct the memberId-less row this mutation
 * exists to fix, so there is no way to exercise it here without a fixture
 * that lies about the schema it is testing against.
 */
export const backfillMemberIds = internalMutation({
  args: {},
  returns: v.number(),
  handler: async (ctx) => {
    const members = await ctx.db.query("members").collect();
    let filled = 0;
    for (const member of members) {
      if (member.memberId !== undefined) continue;
      await ctx.db.patch("members", member._id, { memberId: crypto.randomUUID() });
      filled += 1;
    }
    return filled;
  },
});

function splitName(name: string): { firstName: string; lastName: string } {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return { firstName: "", lastName: "" };
  return {
    firstName: parts[0],
    lastName: parts.slice(1).join(" "),
  };
}

/**
 * What a GitHub sign-in does to the `members` table: claim the row a lead made
 * for this person, or make one.
 *
 * It lives here rather than in `auth.ts` because it is the other half of
 * [`invite`] above — the two decide together what a member row means, and
 * reading either alone leaves the adoption rule looking optional. It takes
 * `isBootstrapLead` as an argument rather than reading
 * `RIABUILD_BOOTSTRAP_LEADS` itself, which keeps every environment lookup in
 * `auth.ts` and makes this callable from a test.
 */
export async function claimOrCreateMember(
  ctx: MutationCtx,
  args: {
    userId: Id<"users">;
    githubLogin: string;
    githubId: string;
    name: string;
    email: string;
    isBootstrapLead: boolean;
  },
): Promise<void> {
  const { isBootstrapLead } = args;

  let existing = await ctx.db
    .query("members")
    .withIndex("by_userId", (q) => q.eq("userId", args.userId))
    .unique();

  /**
   * Nobody has signed in as this person before — but a lead may have gone
   * ahead of them and recorded a role, and issued them a key, already.
   *
   * Adopting that row rather than inserting a second one is the whole design:
   * `issuedKeys.issuedTo` holds the invited row's id, so a grant made days ago
   * survives this moment without anything being migrated. A fresh insert here
   * would leave the invitation stranded beside the real member, still holding
   * the key, while the developer arrived as a candidate with nothing.
   *
   * Only an *unclaimed* row is adopted. A row already carrying a different
   * `userId` is somebody else's, however it was matched, and taking it would
   * hand this sign-in their access.
   */
  if (existing === null) {
    const invited = await findByGithub(ctx, {
      githubId: args.githubId,
      githubLogin: args.githubLogin,
    });
    if (invited !== null && invited.userId === undefined) {
      const { firstName, lastName } = splitName(args.name);
      await ctx.db.patch("members", invited._id, {
        userId: args.userId,
        githubLogin: args.githubLogin,
        githubId: args.githubId || invited.githubId,
        // Only where the invitation left a blank. A lead who filled somebody's
        // name in should not have GitHub's version put back over it the moment
        // that person signs in.
        ...(invited.firstName === "" && invited.lastName === ""
          ? { firstName, lastName }
          : {}),
        ...(invited.email === "" ? { email: args.email } : {}),
      });
      await ctx.db.insert("auditLog", {
        subjectId: invited._id,
        action: "member.joined",
        meta: {
          githubLogin: args.githubLogin,
          role: invited.role,
          source: "invite",
        },
        at: Date.now(),
      });
      // Falls through to the bootstrap check below, so an invited candidate
      // named in RIABUILD_BOOTSTRAP_LEADS is still promoted.
      existing = { ...invited, userId: args.userId };
    }
  }

  if (existing === null) {
    const { firstName, lastName } = splitName(args.name);
    const memberId = await ctx.db.insert("members", {
      userId: args.userId,
      githubLogin: args.githubLogin,
      githubId: args.githubId,
      memberId: crypto.randomUUID(),
      firstName,
      lastName,
      email: args.email,
      role: isBootstrapLead ? "lead" : "candidate",
      status: "active",
    });
    await ctx.db.insert("auditLog", {
      subjectId: memberId,
      action: "member.created",
      meta: {
        githubLogin: args.githubLogin,
        role: isBootstrapLead ? "lead" : "candidate",
        source: isBootstrapLead ? "bootstrap" : "signup",
      },
      at: Date.now(),
    });
    return;
  }

  // A developer can rename their GitHub account; the numeric id cannot change.
  // Profile fields they may have corrected in the dashboard are left alone.
  await ctx.db.patch("members", existing._id, {
    githubLogin: args.githubLogin,
    githubId: args.githubId || existing.githubId,
  });

  if (isBootstrapLead && existing.role !== "lead") {
    await ctx.db.patch("members", existing._id, { role: "lead" });
    await ctx.db.insert("auditLog", {
      subjectId: existing._id,
      action: "member.role_changed",
      meta: {
        githubLogin: args.githubLogin,
        from: existing.role,
        to: "lead",
        source: "bootstrap",
      },
      at: Date.now(),
    });
  }
}

export async function writeAudit(
  ctx: MutationCtx,
  entry: {
    actorId?: Id<"members">;
    subjectId?: Id<"members">;
    action: string;
    meta: Record<string, string>;
  },
): Promise<void> {
  await ctx.db.insert("auditLog", {
    actorId: entry.actorId,
    subjectId: entry.subjectId,
    action: entry.action,
    meta: entry.meta,
    at: Date.now(),
  });
}

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
async function requireLead(ctx: QueryCtx): Promise<Doc<"members">> {
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
 * Task 2's brief (deploy optional, run this against `--prod`, deploy
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

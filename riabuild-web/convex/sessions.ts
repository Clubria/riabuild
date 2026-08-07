import { v } from "convex/values";
import {
  internalMutation,
  mutation,
  query,
  MutationCtx,
} from "./_generated/server";
import { Doc, Id } from "./_generated/dataModel";
import { memberView, toView, viewerMember, writeAudit } from "./members";

export const SESSION_TTL_MS = 90 * 24 * 60 * 60 * 1000;

const sessionView = v.object({
  _id: v.id("cliSessions"),
  deviceLabel: v.string(),
  cliVersion: v.string(),
  createdAt: v.number(),
  lastUsedAt: v.number(),
  expiresAt: v.number(),
  revokedAt: v.union(v.number(), v.null()),
});

function toSessionView(session: Doc<"cliSessions">) {
  return {
    _id: session._id,
    deviceLabel: session.deviceLabel,
    cliVersion: session.cliVersion,
    createdAt: session._creationTime,
    lastUsedAt: session.lastUsedAt,
    expiresAt: session.expiresAt,
    revokedAt: session.revokedAt ?? null,
  };
}

export const listMine = query({
  args: {},
  returns: v.array(sessionView),
  handler: async (ctx) => {
    const member = await viewerMember(ctx);
    if (member === null) return [];
    const sessions = await ctx.db
      .query("cliSessions")
      .withIndex("by_memberId", (q) => q.eq("memberId", member._id))
      .order("desc")
      .take(50);
    return sessions.map(toSessionView);
  },
});

export const revoke = mutation({
  args: { sessionId: v.id("cliSessions") },
  returns: v.null(),
  handler: async (ctx, args) => {
    const member = await viewerMember(ctx);
    if (member === null) throw new Error("Not signed in.");
    const session = await ctx.db.get("cliSessions", args.sessionId);
    if (session === null || session.memberId !== member._id) {
      // Same error either way: do not confirm the existence of other people's
      // sessions to someone guessing ids.
      throw new Error("No such session.");
    }
    if (session.revokedAt !== undefined) return null;

    await ctx.db.patch("cliSessions", session._id, { revokedAt: Date.now() });
    await writeAudit(ctx, {
      actorId: member._id,
      subjectId: member._id,
      action: "cli.session_revoked",
      meta: { deviceLabel: session.deviceLabel },
    });
    return null;
  },
});

/**
 * Revokes a session on behalf of an authenticated `/api/v1` caller (the CLI's
 * `riabuild remote forget`, not the dashboard's self-service `revoke` above).
 *
 * A member may revoke their own session. A lead may revoke anyone's — the
 * same power `members.setStatus` already grants indirectly by suspending, so
 * this is not a new capability, just a more targeted one for pulling a single
 * compromised or orphaned remote-server credential without suspending the
 * whole account.
 *
 * "not_found" and "forbidden" are deliberately distinct return values here —
 * useful for a test to tell apart — but `http.ts` must map both to the same
 * 404 response. A session id that exists but belongs to somebody else must
 * read identically to one that does not exist at all, or this endpoint
 * becomes an oracle for probing which session ids are live.
 */
export const revokeById = internalMutation({
  args: {
    sessionId: v.string(),
    actorId: v.id("members"),
    isLead: v.boolean(),
  },
  returns: v.union(
    v.literal("ok"),
    v.literal("not_found"),
    v.literal("forbidden"),
  ),
  handler: async (ctx, args) => {
    const id = ctx.db.normalizeId("cliSessions", args.sessionId);
    if (id === null) return "not_found";
    const session = await ctx.db.get("cliSessions", id);
    if (session === null) return "not_found";
    if (session.memberId !== args.actorId && !args.isLead) return "forbidden";

    if (session.revokedAt === undefined) {
      await ctx.db.patch("cliSessions", session._id, { revokedAt: Date.now() });
      await writeAudit(ctx, {
        actorId: args.actorId,
        subjectId: session.memberId,
        action: "session.revoked",
        meta: { deviceLabel: session.deviceLabel },
      });
    }
    return "ok";
  },
});

/**
 * The authentication step every `/api/v1` request starts with.
 *
 * A mutation rather than a query because it updates `lastUsedAt` — the dashboard
 * needs to show "last used" for a session someone is deciding whether to revoke.
 */
export const authenticate = internalMutation({
  args: { tokenHash: v.string(), now: v.number() },
  returns: v.union(
    v.object({
      status: v.literal("ok"),
      sessionId: v.id("cliSessions"),
      member: memberView,
    }),
    v.object({
      status: v.union(
        v.literal("unknown"),
        v.literal("revoked"),
        v.literal("expired"),
        v.literal("suspended"),
      ),
    }),
  ),
  handler: async (ctx, args) => {
    const session = await ctx.db
      .query("cliSessions")
      .withIndex("by_tokenHash", (q) => q.eq("tokenHash", args.tokenHash))
      .unique();
    if (session === null) return { status: "unknown" as const };

    const member = await ctx.db.get("members", session.memberId);
    if (member === null) return { status: "unknown" as const };

    // Account state outranks token state. Suspending someone also revokes their
    // sessions, and reporting the revocation would tell them to sign in again —
    // an instruction that cannot work and hides the real reason.
    if (member.status !== "active") return { status: "suspended" as const };
    if (session.revokedAt !== undefined) return { status: "revoked" as const };
    if (session.expiresAt <= args.now) return { status: "expired" as const };

    await ctx.db.patch("cliSessions", session._id, { lastUsedAt: args.now });

    return {
      status: "ok" as const,
      sessionId: session._id,
      member: toView(member),
    };
  },
});

export async function createSession(
  ctx: MutationCtx,
  args: {
    memberId: Id<"members">;
    tokenHash: string;
    deviceLabel: string;
    cliVersion: string;
  },
): Promise<Id<"cliSessions">> {
  const now = Date.now();
  return await ctx.db.insert("cliSessions", {
    memberId: args.memberId,
    tokenHash: args.tokenHash,
    deviceLabel: args.deviceLabel,
    cliVersion: args.cliVersion,
    lastUsedAt: now,
    expiresAt: now + SESSION_TTL_MS,
  });
}

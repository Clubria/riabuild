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
  /**
   * Resolved here rather than passed through as an optional, so the dashboard
   * never has to know that an absent field means `device`.
   */
  origin: v.union(v.literal("device"), v.literal("delegated")),
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
    origin: session.origin ?? ("device" as const),
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
    /** Absent means `device` — see the schema comment on `cliSessions.origin`. */
    origin?: "device" | "delegated";
    delegatedFrom?: Id<"cliSessions">;
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
    origin: args.origin,
    delegatedFrom: args.delegatedFrom,
  });
}

/**
 * Mints a second session for the member who already holds `parentSessionId`.
 * Called only by `POST /api/v1/cli/sessions`, which does the hashing and has
 * already authenticated the parent and re-verified org membership.
 *
 * This is how a laptop signs a *server* in. The developer approved one device
 * code, in a browser, on the machine in front of them; asking them to approve
 * a second one for the server their laptop is provisioning was a round trip
 * that proved nothing the first one had not already proved.
 *
 * **One hop, and the hop is checked here rather than in `http.ts`.** The rule
 * is a fact about the row, so it is enforced where the row is — an endpoint
 * that forgot to ask would otherwise be a delegation chain. A delegated
 * session lives on a server's disk under a Unix account several developers
 * share; letting it mint would mean a co-tenant who read one token could keep
 * minting replacements after `riabuild remote forget` revoked it, and the
 * blast-radius argument for writing a token to that disk at all would be
 * false.
 */
export const delegate = internalMutation({
  args: {
    parentSessionId: v.id("cliSessions"),
    tokenHash: v.string(),
    deviceLabel: v.string(),
    cliVersion: v.string(),
  },
  returns: v.union(
    v.object({
      status: v.literal("ok"),
      sessionId: v.id("cliSessions"),
      expiresAt: v.number(),
    }),
    v.object({
      status: v.union(
        v.literal("not_permitted"),
        v.literal("revoked"),
        v.literal("expired"),
      ),
    }),
  ),
  handler: async (ctx, args) => {
    const parent = await ctx.db.get("cliSessions", args.parentSessionId);
    // Unknown is refused rather than trusted: `authenticate` just read this
    // row, so a miss here is not a state worth guessing at.
    if (parent === null) return { status: "not_permitted" as const };
    if (parent.origin === "delegated") {
      return { status: "not_permitted" as const };
    }

    // Re-read, not inherited. `authenticate` checked `revokedAt` and
    // `expiresAt` in an earlier transaction, and `POST /api/v1/cli/sessions`
    // spends a GitHub round trip between the two — so a session revoked in
    // that window would otherwise mint a fresh ninety-day credential on its
    // way out, which is precisely the window `riabuild remote forget` exists
    // to close. Every gate is re-read next to the row it is a fact about.
    if (parent.revokedAt !== undefined) return { status: "revoked" as const };
    const now = Date.now();
    if (parent.expiresAt <= now) return { status: "expired" as const };

    const sessionId = await createSession(ctx, {
      memberId: parent.memberId,
      tokenHash: args.tokenHash,
      deviceLabel: args.deviceLabel,
      cliVersion: args.cliVersion,
      origin: "delegated",
      delegatedFrom: parent._id,
    });
    const session = await ctx.db.get("cliSessions", sessionId);

    await writeAudit(ctx, {
      actorId: parent.memberId,
      subjectId: parent.memberId,
      action: "cli.session_delegated",
      meta: { deviceLabel: args.deviceLabel, cliVersion: args.cliVersion },
    });

    return {
      status: "ok" as const,
      sessionId,
      expiresAt: session?.expiresAt ?? now + SESSION_TTL_MS,
    };
  },
});

/**
 * How long a dead session row is kept before it is deleted.
 *
 * An hour, matching `cliAuth`'s sweep of abandoned device codes: the row is
 * already useless the moment it expires, and the grace exists so a request in
 * flight against a just-expired session still finds the row and is told
 * `session_expired` rather than `unauthenticated`. Those say different things
 * to a developer, and only one of them is true.
 */
const REAP_GRACE_MS = 60 * 60 * 1000;

/**
 * Deletes sessions that expired more than an hour ago. Scheduled hourly by
 * `crons.ts`.
 *
 * Nothing reaped these before, which is what made the bounded reads elsewhere
 * reachable at all: `listMine` takes 50, and `members.setStatus` used to take
 * 100 and stop. Both numbers are only safe on a table that does not grow
 * without bound, and a ninety-day TTL with one row per delegated server means
 * a long-lived member's history outruns them.
 *
 * Expiry alone is the cut. A revoked session expires within ninety days and is
 * swept by the same pass, whereas deleting it the moment it is revoked would
 * take "revoked <date>" off the dashboard's session list — which is the
 * evidence a developer looks at to confirm the credential they were worried
 * about is actually gone.
 */
export const reapDead = internalMutation({
  args: {},
  returns: v.object({ deleted: v.number() }),
  handler: async (ctx) => {
    const cutoff = Date.now() - REAP_GRACE_MS;
    // TODO(schema): once `cliSessions` carries a `by_expiresAt` index this
    // becomes
    //   .withIndex("by_expiresAt", (q) => q.lt("expiresAt", cutoff))
    // exactly as `cliAuth.reapExpired` does over `cliDeviceCodes`. `filter`
    // walks the table; the index would not.
    const dead = await ctx.db
      .query("cliSessions")
      // eslint-disable-next-line @convex-dev/no-filter-in-query
      .filter((q) => q.lt(q.field("expiresAt"), cutoff))
      .take(500);
    for (const session of dead) {
      await ctx.db.delete("cliSessions", session._id);
    }
    return { deleted: dead.length };
  },
});

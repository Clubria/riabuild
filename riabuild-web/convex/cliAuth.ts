/**
 * Device authorisation — the flow `riabuild login` uses to sign a machine in.
 *
 * The CLI asks `POST /api/v1/cli/device` for a pair of codes, prints the short
 * one, and polls `POST /api/v1/cli/token` until a human approves. Nothing here
 * listens on a socket and nothing travels through a browser redirect, so the
 * flow works identically on a laptop and over SSH.
 *
 * The two codes have different jobs and that split is the whole security model:
 * the `userCode` *identifies* a request and is stored in plaintext because
 * holding one grants nothing, while the `deviceCode` *authenticates* it, is
 * held only by the CLI process, and is stored as a hash.
 */
import { v } from "convex/values";
import { internalMutation, mutation, query } from "./_generated/server";
import { Doc, Id } from "./_generated/dataModel";
import { MutationCtx, QueryCtx } from "./_generated/server";
import { memberView, viewerMember, writeAudit } from "./members";
import { createSession } from "./sessions";
import { normaliseUserCode } from "./lib/crypto";

/**
 * Long enough to walk to another machine, find the dashboard and sign in to
 * GitHub; short enough that an abandoned code is not sitting there at lunch.
 */
export const DEVICE_CODE_TTL_MS = 15 * 60 * 1000;

/** Seconds between polls. Returned to the CLI rather than hard-coded there. */
export const POLL_INTERVAL_SECONDS = 5;

/** Expired rows are swept an hour after they die — see `crons.ts`. */
const REAP_GRACE_MS = 60 * 60 * 1000;

/**
 * The newest row for a user code.
 *
 * Not `.unique()`: rows are reaped rather than reserved forever, so a code can
 * legitimately recur across rows and `.unique()` would throw the first time one
 * did. The newest row is the only one that can still be acted on.
 */
async function byUserCode(
  ctx: QueryCtx,
  userCode: string,
): Promise<Doc<"cliDeviceCodes"> | null> {
  if (userCode === "") return null;
  return await ctx.db
    .query("cliDeviceCodes")
    .withIndex("by_userCode", (q) => q.eq("userCode", userCode))
    .order("desc")
    .first();
}

/**
 * What a pending request looks like to whoever is being asked to approve it.
 *
 * Deliberately not the `memberId`: this is read by someone who is not yet known
 * to be the right person, and the device label is what they are checking
 * against their own terminal.
 */
const deviceRequestView = v.union(
  v.object({
    status: v.literal("pending"),
    deviceLabel: v.string(),
    cliVersion: v.string(),
    requestedAt: v.number(),
    expiresAt: v.number(),
  }),
  v.object({
    status: v.union(
      v.literal("unknown"),
      v.literal("expired"),
      v.literal("used"),
    ),
  }),
);

/**
 * Whether a request can still be acted on, carrying the row when it can.
 *
 * The row travels with the verdict rather than being re-fetched by the caller
 * so that "pending" and "there is a row here" are the same fact, checked once.
 */
type Classified =
  | { status: "pending"; record: Doc<"cliDeviceCodes"> }
  | { status: "unknown" | "expired" | "used" };

function classify(
  record: Doc<"cliDeviceCodes"> | null,
  now: number,
): Classified {
  if (record === null) return { status: "unknown" };
  if (record.consumedAt !== undefined || record.deniedAt !== undefined) {
    return { status: "used" };
  }
  if (record.expiresAt <= now) return { status: "expired" };
  // Already approved: the CLI's next poll will collect it. Showing it as
  // pending would invite a second approval that changes nothing.
  if (record.approvedAt !== undefined) return { status: "used" };
  return { status: "pending", record };
}

/**
 * Looks up a code typed into the dashboard.
 *
 * Requires a signed-in viewer. Not because the answer is sensitive — a device
 * label is not a secret — but because an open endpoint that says whether an
 * arbitrary code exists is a code-space oracle, and there is no reason to ship
 * one when everybody who legitimately reaches this screen has signed in already.
 */
export const deviceRequest = query({
  args: { userCode: v.string() },
  returns: deviceRequestView,
  handler: async (ctx, args) => {
    const member = await viewerMember(ctx);
    if (member === null) throw new Error("Not signed in.");

    const found = classify(
      await byUserCode(ctx, normaliseUserCode(args.userCode)),
      Date.now(),
    );
    if (found.status !== "pending") return { status: found.status };

    return {
      status: "pending" as const,
      deviceLabel: found.record.deviceLabel,
      cliVersion: found.record.cliVersion,
      requestedAt: found.record._creationTime,
      expiresAt: found.record.expiresAt,
    };
  },
});

const decisionResult = v.object({
  status: v.union(
    v.literal("ok"),
    v.literal("unknown"),
    v.literal("expired"),
    v.literal("used"),
  ),
});

async function decide(
  ctx: MutationCtx,
  userCode: string,
  decision: "approve" | "deny",
) {
  const member = await viewerMember(ctx);
  if (member === null) throw new Error("Not signed in.");
  if (member.status !== "active") {
    throw new Error("Your riabuild account is suspended.");
  }

  const now = Date.now();
  const found = classify(await byUserCode(ctx, normaliseUserCode(userCode)), now);
  if (found.status !== "pending") return { status: found.status };
  const record = found.record;

  await ctx.db.patch(
    "cliDeviceCodes",
    record._id,
    decision === "approve"
      ? { memberId: member._id, approvedAt: now }
      : { memberId: member._id, deniedAt: now },
  );

  await writeAudit(ctx, {
    actorId: member._id,
    subjectId: member._id,
    action: decision === "approve" ? "cli.device_approved" : "cli.device_denied",
    meta: { deviceLabel: record.deviceLabel, cliVersion: record.cliVersion },
  });

  return { status: "ok" as const };
}

/**
 * Approve a pending request.
 *
 * A mutation rather than an action: both codes are minted in the HTTP action,
 * so nothing on this path needs the action runtime's `crypto`.
 */
export const approve = mutation({
  args: { userCode: v.string() },
  returns: decisionResult,
  handler: async (ctx, args) => await decide(ctx, args.userCode, "approve"),
});

export const deny = mutation({
  args: { userCode: v.string() },
  returns: decisionResult,
  handler: async (ctx, args) => await decide(ctx, args.userCode, "deny"),
});

/**
 * Records a fresh request. Called only by `POST /api/v1/cli/device`, which
 * mints both codes and hashes the device code.
 *
 * Returns a collision rather than inserting when a live row already holds the
 * user code. At 19^8 codes against a fifteen-minute window this never fires,
 * but the failure it prevents — one developer's approval screen wired to
 * another developer's terminal — is silent and severe.
 */
export const startDevice = internalMutation({
  args: {
    deviceCodeHash: v.string(),
    userCode: v.string(),
    deviceLabel: v.string(),
    cliVersion: v.string(),
    expiresAt: v.number(),
    now: v.number(),
  },
  returns: v.object({
    status: v.union(v.literal("ok"), v.literal("collision")),
  }),
  handler: async (ctx, args) => {
    const existing = await byUserCode(ctx, args.userCode);
    if (classify(existing, args.now).status === "pending") {
      return { status: "collision" as const };
    }

    await ctx.db.insert("cliDeviceCodes", {
      deviceCodeHash: args.deviceCodeHash,
      userCode: args.userCode,
      deviceLabel: args.deviceLabel,
      cliVersion: args.cliVersion,
      expiresAt: args.expiresAt,
    });
    return { status: "ok" as const };
  },
});

/**
 * One poll from the CLI. Called only by `POST /api/v1/cli/token`, which does
 * the hashing and maps the result onto the wire.
 */
export const redeem = internalMutation({
  args: { deviceCodeHash: v.string(), tokenHash: v.string(), now: v.number() },
  returns: v.union(
    v.object({
      status: v.literal("ok"),
      sessionId: v.id("cliSessions"),
      expiresAt: v.number(),
      member: memberView,
    }),
    v.object({
      status: v.union(
        v.literal("pending"),
        v.literal("denied"),
        v.literal("unknown"),
        v.literal("expired"),
        v.literal("consumed"),
        v.literal("suspended"),
      ),
    }),
  ),
  handler: async (ctx, args) => {
    const record = await ctx.db
      .query("cliDeviceCodes")
      .withIndex("by_deviceCodeHash", (q) =>
        q.eq("deviceCodeHash", args.deviceCodeHash),
      )
      .unique();
    if (record === null) return { status: "unknown" as const };

    if (record.consumedAt !== undefined) return { status: "consumed" as const };
    if (record.deniedAt !== undefined) return { status: "denied" as const };
    if (record.expiresAt <= args.now) return { status: "expired" as const };
    if (record.approvedAt === undefined || record.memberId === undefined) {
      // The ordinary case: the developer has not got to the browser yet.
      return { status: "pending" as const };
    }

    const member = await ctx.db.get("members", record.memberId);
    if (member === null) return { status: "unknown" as const };
    // Re-checked here and not only at approval: minutes can pass between the
    // two, and this is the moment a session actually comes into existence.
    if (member.status !== "active") return { status: "suspended" as const };

    // Burn the code before minting anything. A device code is single-use even
    // if everything after this throws.
    await ctx.db.patch("cliDeviceCodes", record._id, { consumedAt: args.now });

    const sessionId: Id<"cliSessions"> = await createSession(ctx, {
      memberId: member._id,
      tokenHash: args.tokenHash,
      deviceLabel: record.deviceLabel,
      cliVersion: record.cliVersion,
    });
    const session = await ctx.db.get("cliSessions", sessionId);

    await writeAudit(ctx, {
      actorId: member._id,
      subjectId: member._id,
      action: "cli.session_created",
      meta: { deviceLabel: record.deviceLabel, cliVersion: record.cliVersion },
    });

    return {
      status: "ok" as const,
      sessionId,
      expiresAt: session?.expiresAt ?? args.now,
      member: {
        _id: member._id,
        githubLogin: member.githubLogin,
        githubId: member.githubId,
        firstName: member.firstName,
        lastName: member.lastName,
        email: member.email,
        role: member.role,
        status: member.status,
        joinedAt: member._creationTime,
      },
    };
  },
});

/**
 * Deletes dead requests. Scheduled hourly by `crons.ts`.
 *
 * Every `riabuild login` leaves a row whether or not a human ever looks at it,
 * so abandoned requests are the common case here rather than the exception —
 * the endpoint that writes them is unauthenticated and needs no approval to
 * have been given.
 */
export const reapExpired = internalMutation({
  args: {},
  returns: v.object({ deleted: v.number() }),
  handler: async (ctx) => {
    const cutoff = Date.now() - REAP_GRACE_MS;
    const dead = await ctx.db
      .query("cliDeviceCodes")
      .withIndex("by_expiresAt", (q) => q.lt("expiresAt", cutoff))
      .take(500);
    for (const record of dead) {
      await ctx.db.delete("cliDeviceCodes", record._id);
    }
    return { deleted: dead.length };
  },
});

import { v } from "convex/values";
import {
  action,
  internalMutation,
  internalQuery,
} from "./_generated/server";
import { internal } from "./_generated/api";
import { Id } from "./_generated/dataModel";
import { memberView, viewerMember, writeAudit } from "./members";
import { createSession } from "./sessions";
import { randomToken, sha256Hex } from "./lib/crypto";

/** A one-time code is useless within minutes of being minted. */
export const CODE_TTL_MS = 5 * 60 * 1000;

export const viewerForAuthorize = internalQuery({
  args: {},
  returns: v.union(memberView, v.null()),
  handler: async (ctx) => {
    const member = await viewerMember(ctx);
    if (member === null) return null;
    return {
      _id: member._id,
      githubLogin: member.githubLogin,
      githubId: member.githubId,
      firstName: member.firstName,
      lastName: member.lastName,
      email: member.email,
      role: member.role,
      status: member.status,
      joinedAt: member._creationTime,
    };
  },
});

export const storeCode = internalMutation({
  args: {
    codeHash: v.string(),
    challenge: v.string(),
    memberId: v.id("members"),
    deviceLabel: v.string(),
    cliVersion: v.string(),
    expiresAt: v.number(),
  },
  returns: v.null(),
  handler: async (ctx, args) => {
    await ctx.db.insert("cliAuthCodes", {
      codeHash: args.codeHash,
      challenge: args.challenge,
      memberId: args.memberId,
      deviceLabel: args.deviceLabel,
      cliVersion: args.cliVersion,
      expiresAt: args.expiresAt,
    });
    await writeAudit(ctx, {
      actorId: args.memberId,
      subjectId: args.memberId,
      action: "cli.code_issued",
      meta: { deviceLabel: args.deviceLabel, cliVersion: args.cliVersion },
    });
    return null;
  },
});

/**
 * Called by the /cli/authorize screen once the developer approves.
 *
 * An action rather than a mutation so the code is minted with the action
 * runtime's `crypto.getRandomValues` and only its hash ever reaches the
 * database. The raw code goes to the browser, which hands it to the CLI's
 * loopback listener.
 */
export const authorize = action({
  args: {
    challenge: v.string(),
    deviceLabel: v.string(),
    cliVersion: v.string(),
  },
  returns: v.object({ code: v.string(), expiresAt: v.number() }),
  handler: async (ctx, args) => {
    const member = await ctx.runQuery(internal.cliAuth.viewerForAuthorize, {});
    if (member === null) throw new Error("Not signed in.");
    if (member.status !== "active") {
      throw new Error("Your riabuild account is suspended.");
    }
    if (args.challenge.length < 32) {
      throw new Error("The CLI sent a malformed PKCE challenge.");
    }

    const code = randomToken(32);
    const expiresAt = Date.now() + CODE_TTL_MS;
    await ctx.runMutation(internal.cliAuth.storeCode, {
      codeHash: await sha256Hex(code),
      challenge: args.challenge,
      memberId: member._id,
      deviceLabel: args.deviceLabel.slice(0, 80) || "unknown device",
      cliVersion: args.cliVersion.slice(0, 32) || "unknown",
      expiresAt,
    });

    return { code, expiresAt };
  },
});

/**
 * Redeem a one-time code for a session. Called only by
 * `POST /api/v1/cli/token`, which does the hashing.
 */
export const redeem = internalMutation({
  args: {
    codeHash: v.string(),
    /** base64url(SHA-256(verifier)) as computed by the HTTP action. */
    computedChallenge: v.string(),
    tokenHash: v.string(),
    now: v.number(),
  },
  returns: v.union(
    v.object({
      status: v.literal("ok"),
      sessionId: v.id("cliSessions"),
      expiresAt: v.number(),
      member: memberView,
    }),
    v.object({
      status: v.union(
        v.literal("unknown"),
        v.literal("consumed"),
        v.literal("expired"),
        v.literal("pkce_mismatch"),
        v.literal("suspended"),
      ),
    }),
  ),
  handler: async (ctx, args) => {
    const record = await ctx.db
      .query("cliAuthCodes")
      .withIndex("by_codeHash", (q) => q.eq("codeHash", args.codeHash))
      .unique();
    if (record === null) return { status: "unknown" as const };

    // Burn the code before any other check. A code that was presented once is
    // spent, whether or not the presenter got anything for it.
    if (record.consumedAt !== undefined) return { status: "consumed" as const };
    await ctx.db.patch("cliAuthCodes", record._id, { consumedAt: args.now });

    if (record.expiresAt <= args.now) return { status: "expired" as const };
    if (record.challenge !== args.computedChallenge) {
      return { status: "pkce_mismatch" as const };
    }

    const member = await ctx.db.get("members", record.memberId);
    if (member === null) return { status: "unknown" as const };
    if (member.status !== "active") return { status: "suspended" as const };

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
      meta: {
        deviceLabel: record.deviceLabel,
        cliVersion: record.cliVersion,
      },
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

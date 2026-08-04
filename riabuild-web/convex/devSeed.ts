import { v } from "convex/values";
import { internalMutation } from "./_generated/server";
import { roleValidator } from "./schema";
import { SESSION_TTL_MS } from "./sessions";

/**
 * Seeds a member and a live CLI session for end-to-end tests against a local
 * backend, where there is no browser to complete the OAuth flow.
 *
 * Three things keep this from being a back door:
 *   - it is an `internalMutation`, so no client can call it;
 *   - reaching it at all requires the deployment admin key;
 *   - it refuses to run unless `RIABUILD_DEV_SEED=1` is set on the deployment,
 *     which production never sets.
 *
 * It still stores the session token hashed, because a test fixture that takes a
 * shortcut around the security model tests the wrong system.
 */
export const seedForE2e = internalMutation({
  args: {
    githubLogin: v.string(),
    tokenHash: v.string(),
    role: v.optional(roleValidator),
  },
  returns: v.object({ memberId: v.id("members"), sessionId: v.id("cliSessions") }),
  handler: async (ctx, args) => {
    if (process.env.RIABUILD_DEV_SEED !== "1") {
      throw new Error(
        "devSeed is disabled. Set RIABUILD_DEV_SEED=1 on this deployment to use it.",
      );
    }

    const existing = await ctx.db
      .query("members")
      .withIndex("by_githubLogin", (q) => q.eq("githubLogin", args.githubLogin))
      .unique();

    const memberId =
      existing?._id ??
      (await (async () => {
        const userId = await ctx.db.insert("users", {
          name: args.githubLogin,
          email: `${args.githubLogin}@example.invalid`,
        });
        return await ctx.db.insert("members", {
          userId,
          githubLogin: args.githubLogin,
          githubId: "0",
          firstName: args.githubLogin,
          lastName: "(seeded)",
          email: `${args.githubLogin}@example.invalid`,
          role: args.role ?? "developer",
          status: "active",
        });
      })());

    if (existing !== null && args.role !== undefined) {
      await ctx.db.patch("members", memberId, { role: args.role });
    }

    const now = Date.now();
    const sessionId = await ctx.db.insert("cliSessions", {
      memberId,
      tokenHash: args.tokenHash,
      deviceLabel: "e2e",
      cliVersion: "0.1.0",
      lastUsedAt: now,
      expiresAt: now + SESSION_TTL_MS,
    });

    return { memberId, sessionId };
  },
});

import { v } from "convex/values";
import { internalMutation } from "./_generated/server";
import { writeAudit } from "./members";

/**
 * Audit writes reachable from HTTP actions, which cannot touch the database
 * directly. Brokering a secret token is an access event and is logged like one.
 */
export const record = internalMutation({
  args: {
    memberId: v.id("members"),
    action: v.string(),
    meta: v.record(v.string(), v.string()),
  },
  returns: v.null(),
  handler: async (ctx, args) => {
    await writeAudit(ctx, {
      actorId: args.memberId,
      subjectId: args.memberId,
      action: args.action,
      meta: args.meta,
    });
    return null;
  },
});

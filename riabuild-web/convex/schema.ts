import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";
import { authTables } from "@convex-dev/auth/server";

export const roleValidator = v.union(
  v.literal("candidate"),
  v.literal("developer"),
  v.literal("lead"),
);

export const statusValidator = v.union(
  v.literal("active"),
  v.literal("suspended"),
);

/**
 * Identity lives in GitHub; only authorization lives here.
 *
 * Every token-shaped value in this schema is stored as a SHA-256 hex digest.
 * A dump of this database must not hand out live sessions.
 */
export default defineSchema({
  ...authTables,

  members: defineTable({
    userId: v.id("users"),
    githubLogin: v.string(),
    githubId: v.string(),
    /**
     * Immutable, ours, and independent of GitHub. Names a developer's
     * directory on a shared server, so it must outlive a GitHub rename.
     * Required — `members.backfillMemberIds` fills existing rows before this
     * field is required in production. See `docs/deploying.md` §7 for the
     * deploy order this depends on.
     *
     * Not the same thing as `cliSessions.memberId` below: that one is a
     * document reference (`v.id("members")`); this one is a UUID string
     * stored on the row itself. Same name, unrelated types — do not unify.
     */
    memberId: v.string(),
    firstName: v.string(),
    lastName: v.string(),
    email: v.string(),
    role: roleValidator,
    status: statusValidator,
  })
    .index("by_userId", ["userId"])
    .index("by_githubLogin", ["githubLogin"]),

  /** Live CLI sessions. `tokenHash` is the lookup key — the raw token is never stored. */
  cliSessions: defineTable({
    /** A document reference — not the UUID `members.memberId` above. Same name, different type. */
    memberId: v.id("members"),
    tokenHash: v.string(),
    deviceLabel: v.string(),
    cliVersion: v.string(),
    lastUsedAt: v.number(),
    expiresAt: v.number(),
    revokedAt: v.optional(v.number()),
  })
    .index("by_tokenHash", ["tokenHash"])
    .index("by_memberId", ["memberId"]),

  /**
   * Pending device-authorisation requests: one row per `riabuild login`, minted
   * by POST /api/v1/cli/device and redeemed once by POST /api/v1/cli/token.
   * Separate from `cliSessions` on purpose — an abandoned login must never look
   * like a live session.
   *
   * A row is created *before* anyone is known, which is the inversion that
   * matters when reading this table: `memberId`, `approvedAt` and `deniedAt`
   * stay empty until a human acts on the request, and most rows never fill them
   * in. `by_expiresAt` exists for the hourly sweep in `crons.ts` that keeps
   * abandoned logins from accumulating forever.
   */
  cliDeviceCodes: defineTable({
    /** SHA-256 of the secret the CLI polls with. The raw value is never stored. */
    deviceCodeHash: v.string(),
    /**
     * The short code the developer reads off their terminal, normalised to
     * uppercase without its dash. Plaintext on purpose: it identifies a request
     * but cannot be exchanged for anything, so hashing it would only stop the
     * dashboard from looking it up.
     */
    userCode: v.string(),
    deviceLabel: v.string(),
    cliVersion: v.string(),
    expiresAt: v.number(),
    memberId: v.optional(v.id("members")),
    approvedAt: v.optional(v.number()),
    deniedAt: v.optional(v.number()),
    consumedAt: v.optional(v.number()),
  })
    .index("by_deviceCodeHash", ["deviceCodeHash"])
    .index("by_userCode", ["userCode"])
    .index("by_expiresAt", ["expiresAt"]),

  /** Single row. Edited by leads in the dashboard, read by every CLI launch. */
  orgConfig: defineTable({
    /** Org Claude Code settings, stored and served as verbatim JSON text. */
    claudeSettings: v.string(),
    claudeSettingsUpdatedAt: v.number(),
    repoSlug: v.string(),
    /**
     * Retired — the CLI now picks the checkout location per platform. Optional
     * rather than deleted so the row written before this change still validates;
     * the next `replace` drops it. See RETIRED_DEFAULT_PROJECT_PATH in org.ts.
     */
    defaultProjectPath: v.optional(v.string()),
    minCliVersion: v.string(),
    latestCliVersion: v.string(),
    /** Bumped when secrets rotate; the CLI treats an older .env.local as stale. */
    secretsUpdatedAt: v.number(),
  }),

  auditLog: defineTable({
    actorId: v.optional(v.id("members")),
    action: v.string(),
    subjectId: v.optional(v.id("members")),
    meta: v.record(v.string(), v.string()),
    at: v.number(),
  }).index("by_at", ["at"]),
});

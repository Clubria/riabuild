import { v } from "convex/values";
import { internalMutation } from "./_generated/server";
import { roleValidator } from "./schema";
import { SESSION_TTL_MS } from "./sessions";

function requireDevSeed() {
  if (process.env.RIABUILD_DEV_SEED !== "1") {
    throw new Error(
      "devSeed is disabled. Set RIABUILD_DEV_SEED=1 on this deployment to use it.",
    );
  }
}

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
    requireDevSeed();

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

/**
 * Points a local deployment at whatever the end-to-end run needs the org to
 * look like: which repository to clone, which versions to demand, which Claude
 * settings to serve.
 *
 * Separate from `seedForE2e` because the two answer different questions — who
 * is signed in, versus what the org has configured — and `e2e/run.sh` sets them
 * at different moments for different reasons.
 *
 * `latestCliVersion` is worth choosing deliberately. A locally built binary
 * reports `9999.0.0-dev`, so any real date leaves it ahead of the published
 * version and `update::decide` leaves it alone. Seed a version above that and
 * the run shells out to `brew upgrade` and replaces the binary under test.
 *
 * Same three gates as `seedForE2e`: internal, admin key, RIABUILD_DEV_SEED=1.
 */
export const seedOrgConfigForE2e = internalMutation({
  args: {
    repoSlug: v.string(),
    claudeSettings: v.string(),
    minCliVersion: v.string(),
    latestCliVersion: v.string(),
    secretsUpdatedAt: v.optional(v.number()),
  },
  returns: v.id("orgConfig"),
  handler: async (ctx, args) => {
    requireDevSeed();

    const next = {
      claudeSettings: args.claudeSettings,
      claudeSettingsUpdatedAt: Date.now(),
      repoSlug: args.repoSlug,
      minCliVersion: args.minCliVersion,
      latestCliVersion: args.latestCliVersion,
      secretsUpdatedAt: args.secretsUpdatedAt ?? 0,
    };

    // `replace`, not `patch`: it drops the retired `defaultProjectPath` if an
    // earlier row still carries it, which is what org.ts documents as the way
    // that field finally goes away.
    const row = await ctx.db.query("orgConfig").first();
    if (row === null) return await ctx.db.insert("orgConfig", next);
    await ctx.db.replace("orgConfig", row._id, next);
    return row._id;
  },
});

/**
 * Populates a local deployment with an org worth looking at: a developer, a
 * candidate, a suspended member, and sessions that are active, expired and
 * revoked.
 *
 * The smoke suite needs this because a freshly signed-in dev account sees empty
 * tables everywhere, and an empty table proves only that the empty state works.
 * The fixture scenarios cover shapes in isolation; this covers the same shapes
 * surviving a real round trip through Convex.
 *
 * Same three gates as `seedForE2e`: it is internal, reaching it needs the
 * deployment admin key, and it refuses without `RIABUILD_DEV_SEED=1`.
 */
export const seedOrgForDev = internalMutation({
  args: {},
  returns: v.object({ members: v.number(), sessions: v.number() }),
  handler: async (ctx) => {
    requireDevSeed();

    const people = [
      {
        login: "dana",
        firstName: "Dana",
        lastName: "Ruiz",
        role: "developer" as const,
        status: "active" as const,
      },
      {
        login: "sam",
        firstName: "Sam",
        lastName: "Tran",
        role: "candidate" as const,
        status: "active" as const,
      },
      {
        login: "rowan",
        firstName: "Rowan",
        lastName: "Fitzgerald-Whitmore",
        role: "developer" as const,
        status: "suspended" as const,
      },
    ];

    const now = Date.now();
    let members = 0;
    let sessions = 0;

    for (const person of people) {
      const existing = await ctx.db
        .query("members")
        .withIndex("by_githubLogin", (q) => q.eq("githubLogin", person.login))
        .unique();
      if (existing !== null) continue;

      const userId = await ctx.db.insert("users", {
        name: `${person.firstName} ${person.lastName}`,
        email: `${person.login}@example.invalid`,
      });
      const memberId = await ctx.db.insert("members", {
        userId,
        githubLogin: person.login,
        githubId: `dev-${person.login}`,
        firstName: person.firstName,
        lastName: person.lastName,
        email: `${person.login}@example.invalid`,
        role: person.role,
        status: person.status,
      });
      members += 1;

      await ctx.db.insert("auditLog", {
        subjectId: memberId,
        action: "member.created",
        meta: { githubLogin: person.login, source: "devSeed" },
        at: now - members * 60_000,
      });
    }

    // Sessions hang off whoever exists rather than a hard-coded login, so this
    // works whatever name the local dev account signed in under.
    const owner = await ctx.db.query("members").first();
    if (owner !== null) {
      const shapes = [
        { label: "dev-active", expiresAt: now + SESSION_TTL_MS },
        { label: "dev-expired", expiresAt: now - 1000 },
        {
          label: "dev-revoked",
          expiresAt: now + SESSION_TTL_MS,
          revokedAt: now,
        },
      ];
      for (const shape of shapes) {
        await ctx.db.insert("cliSessions", {
          memberId: owner._id,
          // Deliberately not a hash of anything: these rows exist to be listed
          // and revoked in the UI, never to authenticate a request.
          tokenHash: `devseed-${shape.label}-not-a-real-token-hash`,
          deviceLabel: shape.label,
          cliVersion: "2026.08.04",
          lastUsedAt: now - 5 * 60_000,
          expiresAt: shape.expiresAt,
          ...(shape.revokedAt !== undefined
            ? { revokedAt: shape.revokedAt }
            : {}),
        });
        sessions += 1;
      }
    }

    return { members, sessions };
  },
});

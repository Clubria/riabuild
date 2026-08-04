import { v } from "convex/values";
import {
  internalQuery,
  mutation,
  query,
  QueryCtx,
} from "./_generated/server";
import { viewerMember, writeAudit } from "./members";

/**
 * Defaults exist so a fresh deployment serves a coherent config before any lead
 * has opened the settings screen. A CLI that gets a 404 for org config cannot do
 * anything useful, and "set this up first" is a worse first run than sane
 * defaults a lead can correct.
 */
export const DEFAULT_CLAUDE_SETTINGS = JSON.stringify(
  {
    permissions: {
      deny: ["Read(./.env.local)", "Read(./.env)", "Bash(git push --force:*)"],
    },
    env: { CLUBRIA_ORG: "1" },
  },
  null,
  2,
);

const configView = v.object({
  claudeSettings: v.string(),
  claudeSettingsUpdatedAt: v.number(),
  repoSlug: v.string(),
  defaultProjectPath: v.string(),
  minCliVersion: v.string(),
  latestCliVersion: v.string(),
  secretsUpdatedAt: v.number(),
});

export type OrgConfig = {
  claudeSettings: string;
  claudeSettingsUpdatedAt: number;
  repoSlug: string;
  defaultProjectPath: string;
  minCliVersion: string;
  latestCliVersion: string;
  secretsUpdatedAt: number;
};

export async function loadConfig(ctx: QueryCtx): Promise<OrgConfig> {
  const row = await ctx.db.query("orgConfig").first();
  if (row !== null) {
    return {
      claudeSettings: row.claudeSettings,
      claudeSettingsUpdatedAt: row.claudeSettingsUpdatedAt,
      repoSlug: row.repoSlug,
      defaultProjectPath: row.defaultProjectPath,
      minCliVersion: row.minCliVersion,
      latestCliVersion: row.latestCliVersion,
      secretsUpdatedAt: row.secretsUpdatedAt,
    };
  }
  return {
    claudeSettings: DEFAULT_CLAUDE_SETTINGS,
    claudeSettingsUpdatedAt: 0,
    repoSlug: "Clubria/ai-builders-hub",
    defaultProjectPath: "~/code/ai-builders-hub",
    minCliVersion: "0.1.0",
    latestCliVersion: "0.1.0",
    secretsUpdatedAt: 0,
  };
}

export const get = query({
  args: {},
  returns: configView,
  handler: async (ctx) => await loadConfig(ctx),
});

export const forApi = internalQuery({
  args: {},
  returns: configView,
  handler: async (ctx) => await loadConfig(ctx),
});

export const update = mutation({
  args: {
    claudeSettings: v.optional(v.string()),
    repoSlug: v.optional(v.string()),
    defaultProjectPath: v.optional(v.string()),
    minCliVersion: v.optional(v.string()),
    latestCliVersion: v.optional(v.string()),
    /** Set when secrets rotate; forces every developer's .env.local to refresh. */
    markSecretsRotated: v.optional(v.boolean()),
  },
  returns: v.null(),
  handler: async (ctx, args) => {
    const member = await viewerMember(ctx);
    if (member === null) throw new Error("Not signed in.");
    if (member.role !== "lead" || member.status !== "active") {
      throw new Error("Only team leads can change org config.");
    }

    if (args.claudeSettings !== undefined) {
      try {
        JSON.parse(args.claudeSettings);
      } catch {
        // The CLI hands this file straight to `claude --settings`. Invalid JSON
        // here breaks every developer's launcher at once.
        throw new Error("Claude settings must be valid JSON.");
      }
    }

    const now = Date.now();
    const current = await loadConfig(ctx);
    const next = {
      claudeSettings: args.claudeSettings ?? current.claudeSettings,
      claudeSettingsUpdatedAt:
        args.claudeSettings !== undefined &&
        args.claudeSettings !== current.claudeSettings
          ? now
          : current.claudeSettingsUpdatedAt,
      repoSlug: args.repoSlug ?? current.repoSlug,
      defaultProjectPath: args.defaultProjectPath ?? current.defaultProjectPath,
      minCliVersion: args.minCliVersion ?? current.minCliVersion,
      latestCliVersion: args.latestCliVersion ?? current.latestCliVersion,
      secretsUpdatedAt: args.markSecretsRotated
        ? now
        : current.secretsUpdatedAt,
    };

    const row = await ctx.db.query("orgConfig").first();
    if (row === null) {
      await ctx.db.insert("orgConfig", next);
    } else {
      await ctx.db.replace("orgConfig", row._id, next);
    }

    await writeAudit(ctx, {
      actorId: member._id,
      action: "org.config_updated",
      meta: {
        fields: Object.keys(args)
          .filter((key) => args[key as keyof typeof args] !== undefined)
          .join(","),
      },
    });
    return null;
  },
});

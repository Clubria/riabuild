import { v } from "convex/values";
import {
  internalMutation,
  internalQuery,
  mutation,
  query,
  QueryCtx,
} from "./_generated/server";
import { viewerMember, writeAudit } from "./members";
import { compareVersions } from "./lib/version";

/**
 * The context-window bar every Clubria developer gets by default.
 *
 * This is a pointer, not a program. The script it names is compiled into the
 * riabuild binary and written to `~/.riabuild/claude-statusline.js` by the
 * `claude_statusline` setup task, so what actually executes on a laptop arrives
 * through a signed Homebrew release. Changing this string cannot make a
 * developer's machine run something new — only `brew upgrade` can.
 *
 * `node` resolves to the Node riabuild installed: it shares `PATH` with the `c`
 * launcher inside the Clubria environment shell.
 */
export const DEFAULT_STATUS_LINE = {
  type: "command",
  command: "node ~/.riabuild/claude-statusline.js",
};

/**
 * Defaults exist so a fresh deployment serves a coherent config before any lead
 * has opened the settings screen. A CLI that gets a 404 for org config cannot do
 * anything useful, and "set this up first" is a worse first run than sane
 * defaults a lead can correct.
 *
 * Every key here is settings data Claude Code reads from the file each account
 * launcher — `claude`, `claude-1` … `claude-N` — passes to `--settings` (source
 * `flagSettings`, verified against 2.1.223). Nothing here is written into
 * anyone's own `settings.json`.
 *
 * `statusLine` is the one key that names a program, and it still carries none:
 * the script it points at ships inside the riabuild binary. See
 * `DEFAULT_STATUS_LINE`.
 *
 * `skipDangerousModePermissionPrompt` is what accepting the bypass-permissions
 * disclaimer sets. Without it Claude Code silently downgrades the mode —
 * "Permission mode downgraded to default — bypass requires accepting the
 * disclaimer interactively first" — so shipping `defaultMode` without it would
 * look configured and behave otherwise.
 *
 * Trusting the checkout is deliberately *not* here: `hasTrustDialogAccepted` is
 * per-project state in `.claude.json`, not a settings key, and no settings file
 * can express it. `claude_trust` in the CLI does that half.
 */
export const DEFAULT_CLAUDE_SETTINGS = JSON.stringify(
  {
    theme: "auto",
    permissions: {
      defaultMode: "bypassPermissions",
      deny: ["Read(./.env.local)", "Read(./.env)", "Bash(git push --force:*)"],
    },
    skipDangerousModePermissionPrompt: true,
    env: { CLUBRIA_ORG: "1" },
    statusLine: DEFAULT_STATUS_LINE,
  },
  null,
  2,
);

/**
 * Retired. Where a checkout goes is now the CLI's decision, because one stored
 * string cannot be right on macOS (`~/Documents/Clubria/<repo>`) and Linux
 * (`~/code/<repo>`) at the same time — see `paths::default_project_dir` in
 * riabuild-cli. A developer who wants somewhere else passes
 * `riabuild --project <path>`.
 *
 * Still emitted by `/api/v1/org/config`: CLIs released before this change
 * require the field to deserialize the response at all, and the `/api/v1`
 * contract is add-only. Delete it once no installed CLI predates the change.
 */
export const RETIRED_DEFAULT_PROJECT_PATH = "~/code/ai-builders-hub";

const configView = v.object({
  claudeSettings: v.string(),
  claudeSettingsUpdatedAt: v.number(),
  repoSlug: v.string(),
  minCliVersion: v.string(),
  latestCliVersion: v.string(),
  secretsUpdatedAt: v.number(),
});

export type OrgConfig = {
  claudeSettings: string;
  claudeSettingsUpdatedAt: number;
  repoSlug: string;
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
      minCliVersion: row.minCliVersion,
      latestCliVersion: row.latestCliVersion,
      secretsUpdatedAt: row.secretsUpdatedAt,
    };
  }
  return {
    claudeSettings: DEFAULT_CLAUDE_SETTINGS,
    claudeSettingsUpdatedAt: 0,
    repoSlug: "Clubria/ai-builders-hub",
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

    // parseVersion is forgiving by design — it maps anything unparseable to 0
    // so a malformed value can never wedge a developer out of their
    // environment. That forgiveness is wrong on the way in: "v2026.08.04" or
    // "latest" would silently become a floor of zero.
    for (const [field, value] of [
      ["minCliVersion", args.minCliVersion],
      ["latestCliVersion", args.latestCliVersion],
    ] as const) {
      if (value !== undefined && !/^\d+(\.\d+)*$/.test(value.trim())) {
        throw new Error(
          `${field} must be a dotted-numeric version like 2026.08.04 — no "v" prefix.`,
        );
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
      minCliVersion: args.minCliVersion ?? current.minCliVersion,
      latestCliVersion: args.latestCliVersion ?? current.latestCliVersion,
      secretsUpdatedAt: args.markSecretsRotated
        ? now
        : current.secretsUpdatedAt,
    };

    // A floor above the newest published build locks out every developer at
    // once, including anyone already on the latest release, and names no
    // version they could upgrade to. The CLI would obey it — that is what a
    // floor is for — so nothing downstream can soften this, and recovering
    // means editing the database by hand.
    if (compareVersions(next.minCliVersion, next.latestCliVersion) > 0) {
      throw new Error(
        `minCliVersion ${next.minCliVersion} is newer than latestCliVersion ` +
          `${next.latestCliVersion}, so nobody could satisfy it. Publish that ` +
          `release first, or lower the floor.`,
      );
    }

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

/**
 * Publishes a newly released CLI version, called by the release workflow.
 *
 * Until this existed, cutting a release and telling developers about it were
 * separate acts, and only the first was automated. A release nobody is offered
 * is invisible: the CLI learns what to upgrade to from `/api/v1/org/config`,
 * never from GitHub, so a forgotten field left every machine on the old build
 * with nothing anywhere reporting a problem.
 *
 * Internal on purpose. It is reachable with a deploy key from CI and from the
 * Convex dashboard, and by no browser client — a version bump is not a thing a
 * signed-in user should be able to trigger by calling an endpoint.
 *
 * Deliberately does **not** touch `minCliVersion`. Raising the floor blocks
 * people mid-workday, and the moment it happens automatically is the moment it
 * happens by accident.
 */
export const setLatestCliVersion = internalMutation({
  args: { version: v.string() },
  returns: v.object({
    updated: v.boolean(),
    latestCliVersion: v.string(),
  }),
  handler: async (ctx, args) => {
    const version = args.version.trim();
    if (!/^\d+(\.\d+)*$/.test(version)) {
      throw new Error(
        `version must be dotted-numeric like 2026.08.04 — got "${version}".`,
      );
    }

    const current = await loadConfig(ctx);

    // Never move backwards. Re-running an old release's workflow — to retry a
    // failed tap push, say — must not offer every developer a downgrade, and
    // re-running the current one must be a no-op rather than a second audit
    // entry claiming something changed.
    if (compareVersions(version, current.latestCliVersion) <= 0) {
      return { updated: false, latestCliVersion: current.latestCliVersion };
    }

    const next = { ...current, latestCliVersion: version };
    const row = await ctx.db.query("orgConfig").first();
    if (row === null) {
      await ctx.db.insert("orgConfig", next);
    } else {
      await ctx.db.replace("orgConfig", row._id, next);
    }

    // No actorId: this is the release pipeline, not a person. The audit view
    // already renders an entry without one.
    await writeAudit(ctx, {
      action: "org.cli_version_published",
      meta: { from: current.latestCliVersion, to: version },
    });

    return { updated: true, latestCliVersion: version };
  },
});

/**
 * Adds the status line to an org that saved its settings before riabuild had
 * one.
 *
 * `DEFAULT_CLAUDE_SETTINGS` is only ever read by a deployment with no
 * `orgConfig` row, so on any org where a lead has pressed save even once, a new
 * default reaches nobody. Run this once per deployment:
 *
 *     npx convex run org:backfillStatusLine --prod
 *
 * Internal on purpose, like `setLatestCliVersion`: reachable from CI and the
 * Convex dashboard, and from no browser client.
 *
 * Conservative by design. An org that already names a status line is left
 * alone, whichever one it names. Overwriting a lead's deliberate choice because
 * a migration ran a second time is worse than the migration doing nothing.
 */
export const backfillStatusLine = internalMutation({
  args: {},
  returns: v.object({ updated: v.boolean(), reason: v.string() }),
  handler: async (ctx) => {
    const row = await ctx.db.query("orgConfig").first();
    if (row === null) {
      return {
        updated: false,
        reason: "No stored config — the served default already carries it.",
      };
    }

    let settings: Record<string, unknown>;
    try {
      const parsed: unknown = JSON.parse(row.claudeSettings);
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed))
        throw new Error("not an object");
      settings = parsed as Record<string, unknown>;
    } catch {
      // `org.update` rejects invalid JSON, so this means the row predates that
      // check or was written by hand. Guessing at a repair here would replace
      // settings nobody can see with settings nobody chose.
      return {
        updated: false,
        reason:
          "Stored settings are not a JSON object. Fix them in the dashboard first.",
      };
    }

    if (settings.statusLine !== undefined) {
      return { updated: false, reason: "A status line is already configured." };
    }

    settings.statusLine = DEFAULT_STATUS_LINE;
    await ctx.db.patch("orgConfig", row._id, {
      claudeSettings: JSON.stringify(settings, null, 2),
      // Moving this is the point: the CLI decides whether to re-fetch by
      // comparing it, so a settings change that leaves it alone never lands.
      claudeSettingsUpdatedAt: Date.now(),
    });

    // No actorId: this is a migration, not a person.
    await writeAudit(ctx, {
      action: "org.config_updated",
      meta: { fields: "claudeSettings", via: "backfillStatusLine" },
    });

    return { updated: true, reason: "Status line added." };
  },
});

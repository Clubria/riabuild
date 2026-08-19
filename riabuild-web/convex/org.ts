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
 *
 * Opening on the agents view is *not* here for the same reason, and it is the
 * one most likely to be added by mistake, because `/config` presents it beside
 * settings that do belong in a settings file. `defaultToAgentsView` is global
 * config in `.claude.json` — Claude Code reads it as
 * `getGlobalConfig().defaultToAgentsView === true` and it is absent from the
 * settings schema (verified against 2.1.231) — so a key of that name saved here
 * would be served to every laptop, layered by every launcher, and read by
 * nothing. `claude_agents_view` in the CLI does it. The same is true of
 * `--exclude-dynamic-system-prompt-sections`, which has no settings key at all
 * and is passed on the launcher's command line by `tasks::shims`.
 */
export const DEFAULT_CLAUDE_SETTINGS = JSON.stringify(
  {
    theme: "auto",
    permissions: {
      defaultMode: "bypassPermissions",
      // `Read(./.env.*)` rather than one entry per environment: riabuild now
      // writes `.env.dev` and `.env.staging`, and a deployment is free to name
      // others. An exact-path entry would have to be edited in the dashboard
      // every time one is added, and would silently leave the new file readable
      // until someone did. `.env.local` stays covered by the same glob.
      deny: [
        "Read(./.env)",
        "Read(./.env.*)",
        "Bash(git push --force:*)",
      ],
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

/**
 * What `forApi` hands the HTTP layer: everything, including the secret.
 *
 * Kept apart from `publicConfigView` below on purpose. The two used to be one
 * validator, and one validator serving both a browser and the CLI is how a
 * secret ends up in a browser by omission rather than by decision.
 */
const configView = v.object({
  claudeSettings: v.string(),
  claudeSettingsUpdatedAt: v.number(),
  repoSlug: v.string(),
  minCliVersion: v.string(),
  latestCliVersion: v.string(),
  secretsUpdatedAt: v.number(),
  ngrokAuthToken: v.string(),
  ngrokAuthTokenUpdatedAt: v.number(),
});

/** What a signed-in browser may see: the ngrok token as a hint, not a value. */
const publicConfigView = v.object({
  claudeSettings: v.string(),
  claudeSettingsUpdatedAt: v.number(),
  repoSlug: v.string(),
  minCliVersion: v.string(),
  latestCliVersion: v.string(),
  secretsUpdatedAt: v.number(),
  ngrokAuthTokenHint: v.string(),
  ngrokAuthTokenUpdatedAt: v.number(),
});

export type OrgConfig = {
  claudeSettings: string;
  claudeSettingsUpdatedAt: number;
  repoSlug: string;
  minCliVersion: string;
  latestCliVersion: string;
  secretsUpdatedAt: number;
  ngrokAuthToken: string;
  ngrokAuthTokenUpdatedAt: number;
};

/**
 * Enough of the token for a lead to recognise the one they pasted, and not
 * enough to use.
 *
 * A lead never needs the secret back — the same rule an issued SSH key follows,
 * where the row is readable as a fingerprint. Empty means none is set, which is
 * what the settings screen says in words.
 */
export function ngrokAuthTokenHint(token: string): string {
  return token === "" ? "" : `…${token.slice(-4)}`;
}

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
      ngrokAuthToken: row.ngrokAuthToken ?? "",
      ngrokAuthTokenUpdatedAt: row.ngrokAuthTokenUpdatedAt ?? 0,
    };
  }
  return {
    claudeSettings: DEFAULT_CLAUDE_SETTINGS,
    claudeSettingsUpdatedAt: 0,
    repoSlug: "Clubria/ai-builders-hub",
    minCliVersion: "0.1.0",
    latestCliVersion: "0.1.0",
    secretsUpdatedAt: 0,
    ngrokAuthToken: "",
    ngrokAuthTokenUpdatedAt: 0,
  };
}

export const get = query({
  args: {},
  returns: publicConfigView,
  handler: async (ctx) => {
    const { ngrokAuthToken, ...config } = await loadConfig(ctx);
    return { ...config, ngrokAuthTokenHint: ngrokAuthTokenHint(ngrokAuthToken) };
  },
});

export const forApi = internalQuery({
  args: {},
  returns: configView,
  handler: async (ctx) => await loadConfig(ctx),
});

/**
 * Each half of `owner/repo`, as GitHub itself allows it.
 *
 * `repoSlug` was the one field this mutation stored without checking, and it is
 * the field the CLI hands to `gh repo clone` and uses to *name a directory*. So
 * `-upload-pack=…/x` was an argv option rather than a repository, and
 * `Clubria/..` was the parent of the directory riabuild meant to clone into —
 * with the brokered `.env` files landing there.
 *
 * `api::Repo::parse` refuses the same values on the CLI side, because a value a
 * developer now types at the picker never reaches this mutation at all. Neither
 * check makes the other redundant: this one keeps a lead's typo out of every
 * developer's machine, and that one is what makes a developer's answer safe.
 */
const REPO_HALF = /^[A-Za-z0-9._-]+$/;

/** GitHub's own ceiling is 39 for a login and 100 for a repository name. */
const REPO_HALF_MAX = 100;

function checkRepoSlug(raw: string): void {
  const halves = raw.trim().split("/");
  if (halves.length !== 2) {
    throw new Error(
      'The repository must be written "owner/repo", e.g. Clubria/ai-builders-hub.',
    );
  }
  for (const half of halves) {
    if (
      half.length === 0 ||
      half.length > REPO_HALF_MAX ||
      half === "." ||
      half === ".." ||
      half.startsWith("-") ||
      !REPO_HALF.test(half)
    ) {
      throw new Error(
        `"${half}" cannot be half of a repository name — letters, digits, dot, ` +
          "dash and underscore only, and not starting with a dash.",
      );
    }
  }
}

export const update = mutation({
  args: {
    claudeSettings: v.optional(v.string()),
    repoSlug: v.optional(v.string()),
    minCliVersion: v.optional(v.string()),
    latestCliVersion: v.optional(v.string()),
    /** Set when secrets rotate; forces every developer's .env.<environment> to refresh. */
    markSecretsRotated: v.optional(v.boolean()),
    /**
     * The team's ngrok authtoken. An empty string clears it, which puts the
     * team back to unconfigured rather than serving a token that authenticates
     * nothing.
     */
    ngrokAuthToken: v.optional(v.string()),
  },
  returns: v.null(),
  handler: async (ctx, args) => {
    const member = await viewerMember(ctx);
    if (member === null) throw new Error("Not signed in.");
    if (member.role !== "lead" || member.status !== "active") {
      throw new Error("Only team leads can change org config.");
    }

    if (args.repoSlug !== undefined) {
      checkRepoSlug(args.repoSlug);
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
    // Trimmed rather than refused: a token is pasted, and a pasted line carries
    // whitespace. It reaches a shell's `$(...)` on a laptop, where a stray
    // newline is the difference between authenticated and not.
    const nextNgrokToken =
      args.ngrokAuthToken === undefined
        ? current.ngrokAuthToken
        : args.ngrokAuthToken.trim();
    const next = {
      claudeSettings: args.claudeSettings ?? current.claudeSettings,
      claudeSettingsUpdatedAt:
        args.claudeSettings !== undefined &&
        args.claudeSettings !== current.claudeSettings
          ? now
          : current.claudeSettingsUpdatedAt,
      // Trimmed, because what is stored is what the CLI is served and what
      // reaches `gh repo clone` — storing the untrimmed string while validating
      // the trimmed one is how a value nobody checked gets shipped.
      repoSlug: args.repoSlug?.trim() ?? current.repoSlug,
      minCliVersion: args.minCliVersion ?? current.minCliVersion,
      latestCliVersion: args.latestCliVersion ?? current.latestCliVersion,
      secretsUpdatedAt: args.markSecretsRotated
        ? now
        : current.secretsUpdatedAt,
      ngrokAuthToken: nextNgrokToken,
      // Zero when there is no token, because that is the value the CLI reads as
      // "nobody has set one" — a cleared token that kept its timestamp would
      // have every developer's riabuild reporting a token that is not there.
      ngrokAuthTokenUpdatedAt:
        nextNgrokToken === ""
          ? 0
          : nextNgrokToken === current.ngrokAuthToken
            ? current.ngrokAuthTokenUpdatedAt
            : now,
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

/**
 * Whether a value is settings *structure* rather than a settings answer.
 *
 * Arrays are answers, not structure. `permissions.deny` is the case that
 * matters: an org that trimmed the deny list has chosen it, and descending into
 * it to restore entries riabuild once shipped would overwrite that choice one
 * element at a time.
 */
function isSettingsObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Copies across every default the stored settings do not have, and nothing
 * else. Records the dotted path of each one in `added`.
 *
 * Absence is the whole test. A key the org set to `false` was answered, and an
 * answer riabuild disagrees with is still an answer.
 */
function fillMissing(
  stored: Record<string, unknown>,
  defaults: Record<string, unknown>,
  prefix: string,
  added: string[],
): void {
  for (const [key, fallback] of Object.entries(defaults)) {
    const path = prefix === "" ? key : `${prefix}.${key}`;
    const current = stored[key];

    if (current === undefined) {
      stored[key] = fallback;
      added.push(path);
    } else if (isSettingsObject(current) && isSettingsObject(fallback)) {
      // `permissions` exists on every org that ever saved, and the keys added
      // to it since do not. Descending is the only way they land.
      fillMissing(current, fallback, path, added);
    }
  }
}

/**
 * Gives an org the team settings that were added after it last pressed save.
 *
 * The general form of `backfillStatusLine`, and the reason that one existed:
 * `loadConfig` serves `DEFAULT_CLAUDE_SETTINGS` only to a deployment with
 * **no** `orgConfig` row, so on any org where a lead has saved once — or where
 * a CLI release published a version, which inserts the same row — a new default
 * reaches nobody. Editing the constant ships it to fresh deployments and to
 * nowhere else. Run this after adding a key to it:
 *
 *     npx convex run org:backfillClaudeDefaults --prod
 *
 * The keys that stranded a real developer were `theme`,
 * `permissions.defaultMode` and `skipDangerousModePermissionPrompt`, all added
 * together and none of them ever backfilled. Their laptop cached settings with
 * no permission mode in them, so every account launcher started Claude Code in
 * the default mode with nothing anywhere reporting a problem.
 *
 * Conservative by design, and only ever additive: a key the org already has is
 * left exactly as it is, whatever its value. Overwriting a lead's deliberate
 * choice because a migration ran a second time is worse than the migration
 * doing nothing — which is also what makes it safe to run twice, and safe to
 * run on an org that has never been touched.
 *
 * One pairing is worth knowing about. `permissions.defaultMode:
 * "bypassPermissions"` is silently downgraded unless
 * `skipDangerousModePermissionPrompt` is set too. Both arrive together here, so
 * an org missing both is repaired — but an org that has explicitly set
 * `skipDangerousModePermissionPrompt: false` keeps that answer and will see the
 * downgrade. That is the correct outcome: it declined the disclaimer.
 *
 * Internal on purpose, like `setLatestCliVersion`: reachable from CI and the
 * Convex dashboard, and from no browser client.
 */
export const backfillClaudeDefaults = internalMutation({
  args: {},
  returns: v.object({
    updated: v.boolean(),
    added: v.array(v.string()),
    reason: v.string(),
  }),
  handler: async (ctx) => {
    const row = await ctx.db.query("orgConfig").first();
    if (row === null) {
      return {
        updated: false,
        added: [],
        reason:
          "No stored config — the served defaults already carry every key.",
      };
    }

    let settings: Record<string, unknown>;
    try {
      const parsed: unknown = JSON.parse(row.claudeSettings);
      if (!isSettingsObject(parsed)) throw new Error("not an object");
      settings = parsed;
    } catch {
      // `org.update` rejects invalid JSON, so this means the row predates that
      // check or was written by hand. Guessing at a repair here would replace
      // settings nobody can see with settings nobody chose.
      return {
        updated: false,
        added: [],
        reason:
          "Stored settings are not a JSON object. Fix them in the dashboard first.",
      };
    }

    const added: string[] = [];
    fillMissing(
      settings,
      JSON.parse(DEFAULT_CLAUDE_SETTINGS) as Record<string, unknown>,
      "",
      added,
    );

    if (added.length === 0) {
      return {
        updated: false,
        added: [],
        reason: "Every default is already answered.",
      };
    }

    await ctx.db.patch("orgConfig", row._id, {
      claudeSettings: JSON.stringify(settings, null, 2),
      // Moving this is the point: the CLI decides whether to re-fetch by
      // comparing it, so a settings change that leaves it alone never lands.
      claudeSettingsUpdatedAt: Date.now(),
    });

    // No actorId: this is a migration, not a person.
    await writeAudit(ctx, {
      action: "org.config_updated",
      meta: {
        fields: "claudeSettings",
        via: "backfillClaudeDefaults",
        added: added.join(","),
      },
    });

    return { updated: true, added, reason: `Added ${added.join(", ")}.` };
  },
});

/** The glob that covers every dotenv file riabuild writes, now and later. */
const DOTENV_DENY_GLOB = "Read(./.env.*)";

/** Exact-path deny entries that mean "this org wants dotenv reads denied". */
const DOTENV_DENY_ENTRIES = ["Read(./.env)", "Read(./.env.local)"];

/**
 * Teaches an existing org the dotenv filenames riabuild writes today.
 *
 *     npx convex run org:denyEveryDotenvFile --prod
 *
 * `backfillClaudeDefaults` cannot do this and is not broken. It only fills keys
 * that are *absent*, and `permissions.deny` is present on every stored row — so
 * editing the array inside `DEFAULT_CLAUDE_SETTINGS` reaches fresh deployments
 * and nowhere else. That gap became a real one when riabuild stopped writing a
 * single `.env.local` and started writing `.env.dev` and `.env.staging`:
 * `Read(./.env)` is an exact path, so neither new file was covered, and the
 * secrets riabuild had just brokered were readable by every Claude Code account.
 *
 * Separate from the backfill, and separately named, because it does something
 * the backfill deliberately does not: it reaches *into* an array the org
 * already answered. Two things keep that honest.
 *
 * It is additive — the glob is appended and nothing is removed, including the
 * now-redundant `Read(./.env.local)`. And it only fires on an org whose deny
 * list still carries a dotenv entry. An org that removed them all is left
 * exactly as it is: an emptied deny list is a decision, and this migration
 * exists to teach an org the new *filenames*, not to re-argue whether dotenv
 * files should be denied at all. That is the same line `backfillClaudeDefaults`
 * draws, and the test beside it pins both sides.
 *
 * Safe to run twice, and safe on an org that has never been touched.
 */
export const denyEveryDotenvFile = internalMutation({
  args: {},
  returns: v.object({ updated: v.boolean(), reason: v.string() }),
  handler: async (ctx) => {
    const row = await ctx.db.query("orgConfig").first();
    if (row === null) {
      return {
        updated: false,
        reason: "No stored config — the served defaults already carry the glob.",
      };
    }

    let settings: Record<string, unknown>;
    try {
      const parsed: unknown = JSON.parse(row.claudeSettings);
      if (!isSettingsObject(parsed)) throw new Error("not an object");
      settings = parsed;
    } catch {
      return {
        updated: false,
        reason:
          "Stored settings are not a JSON object. Fix them in the dashboard first.",
      };
    }

    const permissions = settings.permissions;
    if (!isSettingsObject(permissions) || !Array.isArray(permissions.deny)) {
      // Nothing to extend. `backfillClaudeDefaults` is what supplies a missing
      // key, and it will bring the current default glob with it.
      return {
        updated: false,
        reason: "No deny list to extend — run backfillClaudeDefaults instead.",
      };
    }

    const deny = permissions.deny as unknown[];
    if (deny.includes(DOTENV_DENY_GLOB)) {
      return { updated: false, reason: "Already denies every dotenv file." };
    }
    if (!DOTENV_DENY_ENTRIES.some((entry) => deny.includes(entry))) {
      return {
        updated: false,
        reason: "This org denies no dotenv reads; leaving its choice alone.",
      };
    }

    permissions.deny = [...deny, DOTENV_DENY_GLOB];
    await ctx.db.patch("orgConfig", row._id, {
      claudeSettings: JSON.stringify(settings, null, 2),
      // Moving this is the point: the CLI decides whether to re-fetch by
      // comparing it, so a settings change that leaves it alone never lands.
      claudeSettingsUpdatedAt: Date.now(),
    });

    // No actorId: this is a migration, not a person.
    await writeAudit(ctx, {
      action: "org.config_updated",
      meta: {
        fields: "claudeSettings",
        via: "denyEveryDotenvFile",
        added: DOTENV_DENY_GLOB,
      },
    });

    return { updated: true, reason: `Added ${DOTENV_DENY_GLOB}.` };
  },
});

import { describe, expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import {
  bearer,
  ClaudeSettings,
  issueSession,
  json,
  OrgConfigBody,
  parseSettings,
  seedMember,
  setup,
  TestConvex,
} from "./testing.fixtures";

/**
 * The org config row: who may read it, who may write it, what a lead is
 * allowed to put in the Claude settings blob, and the migrations that reach an
 * org that already has a row.
 *
 * Split out of the old `api.test.ts`.
 */

describe("org config and claude settings", () => {
  test("a signed-out browser is refused the org config outright", async () => {
    // The Convex deployment URL ships in the browser bundle, so a query the
    // dashboard merely declines to call is still a query anyone can make. This
    // one used to answer: Claude settings, the repo slug, the version floors
    // and the ngrok token's last four characters, to whoever asked.
    const t = setup();
    await expect(t.query(api.org.get, {})).rejects.toThrow(/not signed in/i);
  });

  test("a signed-in stranger with no member row is refused too", async () => {
    // Authentication is not the gate. `users` gets a row from the OAuth flow
    // before anything has decided the person belongs here.
    const t = setup();
    const userId = await t.run(
      async (ctx) =>
        await ctx.db.insert("users", { name: "Mal", email: "mal@example.com" }),
    );
    await expect(
      t.withIdentity({ subject: userId }).query(api.org.get, {}),
    ).rejects.toThrow(/not signed in/i);
  });

  test("a member is served the config", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { role: "candidate" });
    const config = await t
      .withIdentity({ subject: userId })
      .query(api.org.get, {});
    expect(config.repoSlug).toBe("Clubria/ai-builders-hub");
  });

  test("a fresh deployment serves defaults rather than an error", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    expect(response.status).toBe(200);
    expect((await json<OrgConfigBody>(response)).repoSlug).toBe(
      "Clubria/ai-builders-hub",
    );
  });

  test("config names the environments the CLI must have on disk", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    // `check()` runs on every `riabuild --check` and must not broker a token to
    // learn which files it is looking for — brokering hits Infisical and writes
    // an audit row. So the list is served here too.
    expect((await json<OrgConfigBody>(response)).secretEnvironments).toEqual([
      "dev",
      "staging",
    ]);
  });

  test("a candidate's config names dev alone", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "candidate" });
    const { token } = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    // Otherwise `check()` would demand a `.env.staging` that `apply()` is never
    // going to be allowed to write — a task that can never go green.
    expect((await json<OrgConfigBody>(response)).secretEnvironments).toEqual([
      "dev",
    ]);
  });

  test("the retired checkout path is still sent, so older CLIs can parse this", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/config", {
      headers: bearer(token),
    });
    // Current CLIs choose the checkout location themselves — it differs per
    // platform, which one stored string cannot express. But a build released
    // before that change cannot deserialize a response missing this field, and
    // /api/v1 is add-only, so the endpoint keeps emitting a frozen value.
    expect((await json<OrgConfigBody>(response)).defaultProjectPath).toBe(
      "~/code/ai-builders-hub",
    );
  });

  test("a lead editing config drops the retired path from the stored row", async () => {
    const t = setup();
    const { userId } = await seedMember(t, { role: "lead" });
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: "{}",
        claudeSettingsUpdatedAt: 0,
        repoSlug: "Clubria/ai-builders-hub",
        defaultProjectPath: "~/code/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    await t
      .withIdentity({ subject: `${userId}|session` })
      .mutation(api.org.update, { repoSlug: "Clubria/ai-builders-hub" });

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    // The field is optional in the schema precisely so this write can drop it;
    // no backfill needed.
    expect(row?.defaultProjectPath).toBeUndefined();
  });

  test("claude settings come back parsed, with their timestamp", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings: JSON.stringify({ env: { CLUBRIA: "1" } }),
        claudeSettingsUpdatedAt: 1234,
        repoSlug: "Clubria/ai-builders-hub",
        defaultProjectPath: "~/code",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });

    const response = await t.fetch("/api/v1/org/claude-settings", {
      headers: bearer(token),
    });
    const body = await json<{ settings: ClaudeSettings; updatedAt: number }>(
      response,
    );
    expect(body.settings).toEqual({ env: { CLUBRIA: "1" } });
    expect(body.updatedAt).toBe(1234);
  });

  test("the default settings ask for bypass mode and pre-accept its disclaimer", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);
    const response = await t.fetch("/api/v1/org/claude-settings", {
      headers: bearer(token),
    });
    const { settings } = await json<{ settings: ClaudeSettings }>(response);

    expect(settings.theme).toBe("auto");
    expect(settings.permissions.defaultMode).toBe("bypassPermissions");
    // These two are one setting wearing two names. Claude Code downgrades
    // bypassPermissions to default unless the disclaimer has been accepted, so
    // shipping the mode alone produces a developer who thinks permissions are
    // off and gets prompted anyway.
    expect(settings.skipDangerousModePermissionPrompt).toBe(true);
  });

  test("the default settings name no status line", async () => {
    const t = setup();
    const { rowId } = await seedMember(t);
    const { token } = await issueSession(t, rowId);

    const response = await t.fetch("/api/v1/org/claude-settings", {
      headers: bearer(token),
    });

    // A status line is a command Claude Code runs on every render, so it is
    // not a thing this server sends: the CLI writes the one its own
    // `claude_statusline` task installed on that machine. A key here would be
    // dropped on every laptop.
    expect(
      (await json<{ settings: ClaudeSettings }>(response)).settings.statusLine,
    ).toBeUndefined();
  });

  /** Exactly what an org that saved before the permission keys existed holds. */
  const preBypassSettings = JSON.stringify({
    permissions: {
      deny: ["Read(./.env)", "Read(./.env.*)", "Bash(git push --force:*)"],
    },
    env: { CLUBRIA_ORG: "1" },
  });

  async function seedOrgConfig(t: TestConvex, claudeSettings: string) {
    await t.run(async (ctx) => {
      await ctx.db.insert("orgConfig", {
        claudeSettings,
        claudeSettingsUpdatedAt: 1234,
        repoSlug: "Clubria/ai-builders-hub",
        minCliVersion: "0.1.0",
        latestCliVersion: "0.1.0",
        secretsUpdatedAt: 0,
      });
    });
  }

  const storedSettings = async (t: TestConvex) => {
    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    return { row: row!, settings: parseSettings(row!.claudeSettings) };
  };

  test("the defaults backfill repairs an org that saved before bypass mode existed", async () => {
    // The regression a developer actually hit: `claude-1` started in the
    // default permission mode because the org row predated the keys, and
    // editing DEFAULT_CLAUDE_SETTINGS reached fresh deployments only.
    const t = setup();
    await seedOrgConfig(t, preBypassSettings);

    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(true);

    const { row, settings } = await storedSettings(t);
    expect(settings.permissions.defaultMode).toBe("bypassPermissions");
    // Without this the mode above is silently downgraded, so a backfill that
    // added one and not the other would look repaired and behave otherwise.
    expect(settings.skipDangerousModePermissionPrompt).toBe(true);
    expect(settings.theme).toBe("auto");
    // And nothing puts a status line back: it left the defaults when riabuild
    // started writing its own on each machine.
    expect(settings.statusLine).toBeUndefined();
    expect(result.added).toContain("permissions.defaultMode");

    // The CLI re-fetches by comparing this. A backfill that left it at 1234
    // would change the database and nobody's laptop.
    expect(row.claudeSettingsUpdatedAt).toBeGreaterThan(1234);
  });

  test("the defaults backfill leaves every answer the org already gave", async () => {
    const t = setup();
    const chosen = {
      theme: "dark",
      // An org that chose the *opposite* split from the one riabuild ships:
      // a cheaper session model and a more expensive subagent default. Both
      // are answers, and a backfill that "corrected" either would be
      // overwriting a lead rather than filling a gap.
      model: "sonnet",
      permissions: { defaultMode: "acceptEdits", deny: [] },
      skipDangerousModePermissionPrompt: false,
      env: {
        CLUBRIA_ORG: "1",
        CLAUDE_CODE_SUBAGENT_MODEL: "opus",
        EXTRA: "kept",
      },
      statusLine: { type: "command", command: "my-own-statusline" },
    };
    await seedOrgConfig(t, JSON.stringify(chosen));

    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(false);

    const { row, settings } = await storedSettings(t);
    expect(settings).toEqual(chosen);
    expect(row.claudeSettingsUpdatedAt).toBe(1234);
  });

  test("the defaults backfill teaches an org the opus/sonnet split", async () => {
    // The two halves are spelled differently by Claude Code — the session's
    // model is a settings key, its subagents' default is an environment
    // variable — so they land through two different arms of `fillMissing`:
    // `model` is absent at the top level, and `env` is present on every stored
    // row, which means the variable only arrives by descending into it.
    const t = setup();
    await seedOrgConfig(t, preBypassSettings);

    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(true);

    const { settings } = await storedSettings(t);
    expect(settings.model).toBe("opus");
    expect(settings.env.CLAUDE_CODE_SUBAGENT_MODEL).toBe("sonnet");
    // The org's own variable is still there: descending fills gaps and
    // replaces nothing.
    expect(settings.env.CLUBRIA_ORG).toBe("1");
    expect(result.added).toContain("model");
    expect(result.added).toContain("env.CLAUDE_CODE_SUBAGENT_MODEL");
  });

  test("the defaults backfill never restores deny rules an org removed", async () => {
    // An emptied deny list is a decision. Descending into an array to put
    // riabuild's entries back would undo it one element at a time.
    const t = setup();
    await seedOrgConfig(t, JSON.stringify({ permissions: { deny: [] } }));

    await t.mutation(internal.org.backfillClaudeDefaults, {});

    const { settings } = await storedSettings(t);
    expect(settings.permissions.deny).toEqual([]);
    // The sibling key it never answered still arrives.
    expect(settings.permissions.defaultMode).toBe("bypassPermissions");
  });

  test("an org still denying dotenv reads is taught the new filenames", async () => {
    // riabuild used to write one `.env.local` and now writes `.env.dev` and
    // `.env.staging`. `Read(./.env)` is an exact path, so neither new file is
    // covered — and `backfillClaudeDefaults` cannot help, because it only adds
    // keys that are *absent* and `permissions.deny` is present on every stored
    // row. Without this migration the secrets riabuild just wrote would be
    // readable by every Claude Code account on every existing deployment.
    const t = setup();
    await seedOrgConfig(
      t,
      JSON.stringify({
        permissions: {
          deny: [
            "Read(./.env.local)",
            "Read(./.env)",
            "Bash(git push --force:*)",
          ],
        },
      }),
    );

    const result = await t.mutation(internal.org.denyEveryDotenvFile, {});
    expect(result.updated).toBe(true);

    const { row, settings } = await storedSettings(t);
    expect(settings.permissions.deny).toContain("Read(./.env.*)");
    // Additive only: nothing the org already had is removed, including the
    // now-redundant exact entry for the file riabuild no longer writes.
    expect(settings.permissions.deny).toContain("Read(./.env.local)");
    expect(settings.permissions.deny).toContain("Bash(git push --force:*)");
    // The CLI re-fetches by comparing this; leaving it would change the
    // database and nobody's laptop.
    expect(row.claudeSettingsUpdatedAt).toBeGreaterThan(1234);
  });

  test("an org that removed its dotenv denials is left alone", async () => {
    // The same rule the defaults backfill follows: an emptied deny list is a
    // decision, and putting an entry back one element at a time undoes it.
    // This migration teaches orgs the new *filenames*; it does not re-argue
    // whether dotenv files should be denied at all.
    const t = setup();
    const chosen = { permissions: { deny: ["Bash(git push --force:*)"] } };
    await seedOrgConfig(t, JSON.stringify(chosen));

    const result = await t.mutation(internal.org.denyEveryDotenvFile, {});
    expect(result.updated).toBe(false);

    const { row, settings } = await storedSettings(t);
    expect(settings).toEqual(chosen);
    expect(row.claudeSettingsUpdatedAt).toBe(1234);
  });

  test("running the dotenv migration twice is a no-op the second time", async () => {
    const t = setup();
    await seedOrgConfig(
      t,
      JSON.stringify({ permissions: { deny: ["Read(./.env)"] } }),
    );

    await t.mutation(internal.org.denyEveryDotenvFile, {});
    const { row: first } = await storedSettings(t);
    const second = await t.mutation(internal.org.denyEveryDotenvFile, {});

    expect(second.updated).toBe(false);
    const { row } = await storedSettings(t);
    expect(row.claudeSettingsUpdatedAt).toBe(first.claudeSettingsUpdatedAt);
  });

  test("a deployment with no stored config needs no dotenv migration", async () => {
    // It is served DEFAULT_CLAUDE_SETTINGS, which already carries the glob.
    const t = setup();
    const result = await t.mutation(internal.org.denyEveryDotenvFile, {});
    expect(result.updated).toBe(false);
  });

  test("running the defaults backfill twice is a no-op the second time", async () => {
    const t = setup();
    await seedOrgConfig(t, preBypassSettings);

    expect(
      (await t.mutation(internal.org.backfillClaudeDefaults, {})).updated,
    ).toBe(true);
    expect(
      (await t.mutation(internal.org.backfillClaudeDefaults, {})).updated,
    ).toBe(false);

    const entries = await t.run(async (ctx) =>
      ctx.db
        .query("auditLog")
        .collect()
        .then((rows) =>
          rows.filter((row) => row.meta.via === "backfillClaudeDefaults"),
        ),
    );
    expect(entries).toHaveLength(1);
  });

  test("the defaults backfill refuses to guess at settings it cannot parse", async () => {
    const t = setup();
    await seedOrgConfig(t, "{ not json");

    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(false);
    expect(result.reason).toMatch(/dashboard/i);

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    expect(row!.claudeSettings).toBe("{ not json");
  });

  test("the defaults backfill leaves a deployment with no stored row alone", async () => {
    const t = setup();
    const result = await t.mutation(internal.org.backfillClaudeDefaults, {});
    expect(result.updated).toBe(false);

    const row = await t.run(
      async (ctx) => await ctx.db.query("orgConfig").first(),
    );
    // Inserting one here would freeze today's defaults into the database and
    // recreate the very trap this migration exists to undo.
    expect(row).toBeNull();
  });
});

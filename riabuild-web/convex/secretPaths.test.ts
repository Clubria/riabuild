import { describe, expect, test } from "vitest";
import { api, internal } from "./_generated/api";
import {
  auditRows,
  Role,
  seedMember,
  setup,
  TestConvex,
} from "./testing.fixtures";

/**
 * Which Infisical folders each repository takes its secrets from: who may
 * change the table, what a lead is allowed to type into it, and the one rule
 * the whole feature rests on — **no row means no environment files**, never a
 * fallback to the deployment's own path.
 *
 * `secretBrokering.test.ts` covers what a CLI is then told about the result.
 */

/** A lead signs in as `grace`, everyone else as `ada`. */
async function asRole(t: TestConvex, role: Role) {
  const { userId } = await seedMember(t, {
    login: role === "lead" ? "grace" : "ada",
    role,
  });
  return t.withIdentity({ subject: `${userId}|session` });
}

const HUB = { repoSlug: "Clubria/ai-builders-hub", secretPaths: ["/"] };

describe("who may change the table", () => {
  test("a lead maps a repository and it comes back in the list", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, HUB);

    const list = await lead.query(api.secretPaths.list, {});
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({
      repoSlug: "Clubria/ai-builders-hub",
      secretPaths: ["/"],
    });
  });

  test("a developer cannot map a repository", async () => {
    const t = setup();
    const developer = await asRole(t, "developer");

    await expect(developer.mutation(api.secretPaths.set, HUB)).rejects.toThrow(
      /lead/i,
    );
  });

  test("a developer cannot read the dashboard's table", async () => {
    // Not a secret — their own CLI is told the one row its run is about. This
    // is the dashboard section being lead-only, nothing more.
    const t = setup();
    const developer = await asRole(t, "developer");

    await expect(developer.query(api.secretPaths.list, {})).rejects.toThrow(
      /lead/i,
    );
  });

  test("a signed-out visitor cannot map a repository", async () => {
    const t = setup();
    await expect(t.mutation(api.secretPaths.set, HUB)).rejects.toThrow(
      /signed in/i,
    );
  });
});

describe("the repository a lead types", () => {
  test("a name that is not owner/name is refused", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    for (const repoSlug of [
      "ai-builders-hub",
      "Clubria/apps/payments",
      "Clubria/",
      "/payments",
    ]) {
      await expect(
        lead.mutation(api.secretPaths.set, { ...HUB, repoSlug }),
      ).rejects.toThrow(/owner\/name/i);
    }
  });

  test("a half that git would read as an option is refused", async () => {
    // Not cosmetic: this value reaches `gh repo clone` argv on a laptop, where
    // a leading dash is an option rather than a repository. Every character
    // here is one the charset rule allows, so without the dash branch it
    // stores cleanly and is read as a flag on somebody else's machine.
    const t = setup();
    const lead = await asRole(t, "lead");

    await expect(
      lead.mutation(api.secretPaths.set, {
        ...HUB,
        repoSlug: "Clubria/-upload-pack=sh",
      }),
    ).rejects.toThrow(/dash/i);
  });

  test("a half that would climb out of the checkout is refused", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await expect(
      lead.mutation(api.secretPaths.set, { ...HUB, repoSlug: "../etc" }),
    ).rejects.toThrow(/not a repository name/i);
  });
});

describe("the folders a lead types", () => {
  test("a relative folder is refused, because the CLI's cwd would decide it", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await expect(
      lead.mutation(api.secretPaths.set, {
        ...HUB,
        secretPaths: ["apps/payments"],
      }),
    ).rejects.toThrow(/starts at the root/i);
  });

  test("the trailing slash the Infisical UI shows is refused rather than trimmed", async () => {
    // Refused rather than fixed up: `/apps/payments` and `/apps/payments/`
    // would otherwise be two spellings that hash to different cache keys and
    // read as a folder move on every save.
    const t = setup();
    const lead = await asRole(t, "lead");

    await expect(
      lead.mutation(api.secretPaths.set, {
        ...HUB,
        secretPaths: ["/apps/payments/"],
      }),
    ).rejects.toThrow(/trailing slash/i);
  });

  test("a folder riabuild would have to resolve is refused", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    for (const path of ["/apps/../secrets", "/apps//payments", "/apps/."]) {
      await expect(
        lead.mutation(api.secretPaths.set, { ...HUB, secretPaths: [path] }),
      ).rejects.toThrow();
    }
  });

  test("the root is a folder, and the honest spelling of the small case", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, { ...HUB, secretPaths: ["/"] });
    const list = await lead.query(api.secretPaths.list, {});
    expect(list[0].secretPaths).toEqual(["/"]);
  });

  test("naming no folder at all is refused, because removing the row says that", async () => {
    // Two spellings of "this repository has no environment variables" would be
    // two things every reader has to know about, and one of them writes a row
    // that looks configured.
    const t = setup();
    const lead = await asRole(t, "lead");

    await expect(
      lead.mutation(api.secretPaths.set, { ...HUB, secretPaths: [] }),
    ).rejects.toThrow(/at least one/i);
  });

  test("a repeated folder keeps its last mention, because the last one wins", async () => {
    // Order is dotenv's contract: a key two folders hold takes the value of
    // the folder exported later. Dropping the *earlier* mention cannot change
    // the finished file; dropping the later one silently would.
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, {
      ...HUB,
      secretPaths: ["/apps/payments", "/", "/apps/payments"],
    });

    const list = await lead.query(api.secretPaths.list, {});
    expect(list[0].secretPaths).toEqual(["/", "/apps/payments"]);
  });
});

describe("mapping, moving and unmapping", () => {
  test("the repository is the key, so a second save moves it rather than adding a row", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, HUB);
    await lead.mutation(api.secretPaths.set, {
      ...HUB,
      secretPaths: ["/apps/hub"],
    });

    const list = await lead.query(api.secretPaths.list, {});
    expect(list).toHaveLength(1);
    expect(list[0].secretPaths).toEqual(["/apps/hub"]);
  });

  test("re-saving the same folders does not restage the whole team's files", async () => {
    // `updatedAt` is what makes every developer's `.env.<name>` stale. A lead
    // who opens the row and presses save has moved no folder, and every laptop
    // on the team refetching because of it is a cost with nothing behind it.
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, HUB);
    const before = (await lead.query(api.secretPaths.list, {}))[0].updatedAt;

    await lead.mutation(api.secretPaths.set, HUB);
    const after = (await lead.query(api.secretPaths.list, {}))[0].updatedAt;

    expect(after).toBe(before);
    expect(await auditRows(t)).toHaveLength(1);
  });

  test("reordering the same folders is a change, because the order decides the value", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, {
      ...HUB,
      secretPaths: ["/", "/apps/hub"],
    });
    await lead.mutation(api.secretPaths.set, {
      ...HUB,
      secretPaths: ["/apps/hub", "/"],
    });

    const list = await lead.query(api.secretPaths.list, {});
    expect(list[0].secretPaths).toEqual(["/apps/hub", "/"]);
    expect((await auditRows(t)).map((row) => row.action)).toEqual([
      "repo_secret_path.added",
      "repo_secret_path.updated",
    ]);
  });

  test("unmapping leaves the repository with no row, which is the whole point", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, HUB);
    const [row] = await lead.query(api.secretPaths.list, {});
    await lead.mutation(api.secretPaths.remove, { id: row._id });

    expect(await lead.query(api.secretPaths.list, {})).toEqual([]);
    expect(
      await t.query(internal.secretPaths.forRepo, {
        repoSlug: HUB.repoSlug,
      }),
    ).toBeNull();
  });

  test("a developer cannot unmap a repository", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");
    await lead.mutation(api.secretPaths.set, HUB);
    const [row] = await lead.query(api.secretPaths.list, {});

    const { userId } = await seedMember(t, { login: "ada", role: "developer" });
    const developer = t.withIdentity({ subject: `${userId}|session` });

    await expect(
      developer.mutation(api.secretPaths.remove, { id: row._id }),
    ).rejects.toThrow(/lead/i);
  });

  test("every change to where secrets come from is audited", async () => {
    // Pointing a repository at a folder decides whose secrets land in which
    // checkout, which is an access change however inert the path itself is.
    const t = setup();
    const lead = await asRole(t, "lead");

    await lead.mutation(api.secretPaths.set, HUB);
    await lead.mutation(api.secretPaths.set, {
      ...HUB,
      secretPaths: ["/apps/hub"],
    });
    const [row] = await lead.query(api.secretPaths.list, {});
    await lead.mutation(api.secretPaths.remove, { id: row._id });

    expect(await auditRows(t)).toEqual([
      {
        action: "repo_secret_path.added",
        meta: { repo: HUB.repoSlug, path: "/" },
      },
      {
        action: "repo_secret_path.updated",
        meta: { repo: HUB.repoSlug, from: "/", path: "/apps/hub" },
      },
      {
        action: "repo_secret_path.removed",
        meta: { repo: HUB.repoSlug, path: "/apps/hub" },
      },
    ]);
  });
});

describe("what one repository's CLI is told", () => {
  test("a repository nobody mapped comes back null, never the deployment's path", async () => {
    // The sentence the whole feature rests on. `null` here is what the two
    // endpoints turn into "this repository has no environment files", and a
    // fallback to `INFISICAL_SECRET_PATH` would fill an unmapped repository
    // from another repository's folders with nothing said on the terminal.
    const t = setup();
    const lead = await asRole(t, "lead");
    await lead.mutation(api.secretPaths.set, HUB);

    expect(
      await t.query(internal.secretPaths.forRepo, {
        repoSlug: "Clubria/marketing",
      }),
    ).toBeNull();
  });

  test("a mapped repository carries its folders in order and when they last moved", async () => {
    const t = setup();
    const lead = await asRole(t, "lead");
    await lead.mutation(api.secretPaths.set, {
      ...HUB,
      secretPaths: ["/", "/apps/hub"],
    });

    const scope = await t.query(internal.secretPaths.forRepo, {
      repoSlug: HUB.repoSlug,
    });
    expect(scope?.secretPaths).toEqual(["/", "/apps/hub"]);
    expect(scope?.updatedAt).toBeGreaterThan(0);
  });
});

describe("the discovery cache", () => {
  test("an answer is kept and read back under the same question", async () => {
    const t = setup();
    await t.mutation(internal.secretPaths.cacheEnvironments, {
      key: "developer /",
      environments: ["dev", "staging"],
    });

    expect(
      await t.query(internal.secretPaths.cachedEnvironments, {
        key: "developer /",
      }),
    ).toEqual(["dev", "staging"]);
  });

  test("a question nobody asked yet is a miss rather than an empty answer", async () => {
    // The distinction that matters: "no environments hold these folders" is a
    // real answer, and a cache that returned `[]` for "never asked" would make
    // the first run of every repository look like that.
    const t = setup();
    expect(
      await t.query(internal.secretPaths.cachedEnvironments, {
        key: "developer /apps/hub",
      }),
    ).toBeNull();
  });

  test("the key is the question, so a lead's edit invalidates it without a purge", async () => {
    // Keyed by role and folder list rather than by repository. Moving a
    // repository to another folder asks a different question, which has its own
    // row — nothing has to remember to delete the old one.
    const t = setup();
    await t.mutation(internal.secretPaths.cacheEnvironments, {
      key: "developer /",
      environments: ["dev"],
    });

    expect(
      await t.query(internal.secretPaths.cachedEnvironments, {
        key: "developer /apps/hub",
      }),
    ).toBeNull();
    expect(
      await t.query(internal.secretPaths.cachedEnvironments, {
        key: "candidate /",
      }),
    ).toBeNull();
  });

  test("an answer past its life is a miss, not a stale list", async () => {
    const t = setup();
    await t.mutation(internal.secretPaths.cacheEnvironments, {
      key: "developer /",
      environments: ["dev"],
    });
    await t.run(async (ctx) => {
      const row = await ctx.db.query("infisicalEnvCache").first();
      if (row === null) throw new Error("the cache row was not written");
      await ctx.db.patch("infisicalEnvCache", row._id, {
        fetchedAt: Date.now() - 60 * 60 * 1000,
      });
    });

    expect(
      await t.query(internal.secretPaths.cachedEnvironments, {
        key: "developer /",
      }),
    ).toBeNull();
  });

  test("a second answer to the same question replaces the first", async () => {
    const t = setup();
    await t.mutation(internal.secretPaths.cacheEnvironments, {
      key: "developer /",
      environments: ["dev"],
    });
    await t.mutation(internal.secretPaths.cacheEnvironments, {
      key: "developer /",
      environments: ["dev", "prod"],
    });

    expect(
      await t.query(internal.secretPaths.cachedEnvironments, {
        key: "developer /",
      }),
    ).toEqual(["dev", "prod"]);
    const rows = await t.run(
      async (ctx) => await ctx.db.query("infisicalEnvCache").collect(),
    );
    expect(rows).toHaveLength(1);
  });
});

describe("the migration for a deployment that predates the table", () => {
  test("an org with no rows gets the one that reproduces today's behaviour", async () => {
    // "No row means no secrets" applied to a deployment that has never had a
    // row means every developer loses their `.env.dev` on the day this ships.
    const t = setup();
    const lead = await asRole(t, "lead");
    await lead.mutation(api.org.update, {
      repoSlug: "Clubria/ai-builders-hub",
    });

    await t.mutation(internal.secretPaths.seedFromDeploymentPath, {});

    const list = await lead.query(api.secretPaths.list, {});
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({
      repoSlug: "Clubria/ai-builders-hub",
      secretPaths: ["/"],
    });
  });

  test("an org that has mapped anything is left alone, and re-running does nothing", async () => {
    // The emptiness test is the whole safety property: an org that has mapped
    // something has made a decision, and a migration that adds a row beside it
    // is re-arguing one nobody asked it to.
    const t = setup();
    const lead = await asRole(t, "lead");
    await lead.mutation(api.org.update, {
      repoSlug: "Clubria/ai-builders-hub",
    });
    await lead.mutation(api.secretPaths.set, {
      repoSlug: "Clubria/payments",
      secretPaths: ["/apps/payments"],
    });

    const said = await t.mutation(
      internal.secretPaths.seedFromDeploymentPath,
      {},
    );

    expect(said).toMatch(/already mapped/i);
    const list = await lead.query(api.secretPaths.list, {});
    expect(list).toHaveLength(1);
    expect(list[0].repoSlug).toBe("Clubria/payments");
  });

  test("a deployment with no org configuration yet says so rather than throwing", async () => {
    const t = setup();
    const said = await t.mutation(
      internal.secretPaths.seedFromDeploymentPath,
      {},
    );
    expect(said).toMatch(/nothing to seed/i);
    expect(
      await t.run(async (ctx) => await ctx.db.query("repoSecretPaths").first()),
    ).toBeNull();
  });
});

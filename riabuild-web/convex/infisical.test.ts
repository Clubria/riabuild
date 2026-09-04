import { afterEach, describe, expect, test, vi } from "vitest";
import {
  discoverEnvironments,
  environmentsForRole,
  identityForRole,
  readEnvironmentSlugs,
  secretPaths,
  splitFolder,
} from "./infisical";
import { stubFetch } from "./testing.fixtures";

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("which machine identity a role is brokered through", () => {
  function stubLeadIdentity() {
    vi.stubEnv("INFISICAL_LEAD_CLIENT_ID", "lead-id");
    vi.stubEnv("INFISICAL_LEAD_CLIENT_SECRET", "lead-secret");
  }

  test("a lead gets the identity Infisical grants everything", () => {
    stubLeadIdentity();
    const identity = identityForRole("lead");
    expect(identity.name).toBe("mi-lead");
    expect(identity.clientIdVar).toBe("INFISICAL_LEAD_CLIENT_ID");
    expect(identity.clientSecretVar).toBe("INFISICAL_LEAD_CLIENT_SECRET");
  });

  test("a developer and a candidate are unaffected by it", () => {
    // Widening a lead must not widen anybody else: the credential is the only
    // thing that differs, and the permissions behind each are Infisical's.
    stubLeadIdentity();
    expect(identityForRole("developer").name).toBe("mi-developer");
    expect(identityForRole("candidate").name).toBe("mi-candidate");
  });

  test("a deployment without mi-lead keeps its leads on mi-developer", () => {
    // Where every lead was brokered before `mi-lead` existed. Failing their
    // runs until two Convex environment variables are set would cost more than
    // it buys, and the fallback only ever narrows.
    expect(identityForRole("lead").name).toBe("mi-developer");
  });

  test("half a lead identity is not one", () => {
    // A client id typed and a secret still to come is a deployment mid-setup.
    // Authenticating with an incomplete pair fails against Infisical with a
    // 401 nobody can read; the narrower identity that works is the answer.
    vi.stubEnv("INFISICAL_LEAD_CLIENT_ID", "lead-id");
    expect(identityForRole("lead").name).toBe("mi-developer");
  });
});

describe("which environments a role may pull", () => {
  test("a developer and a lead get dev and staging", () => {
    expect(environmentsForRole("developer")).toEqual(["dev", "staging"]);
    expect(environmentsForRole("lead")).toEqual(["dev", "staging"]);
  });

  test("a candidate gets the base environment alone", () => {
    // The same split `identityForRole` makes: a candidate is brokered through
    // the narrower machine identity, so asking for staging on their behalf
    // would only produce an Infisical denial the developer cannot act on.
    expect(environmentsForRole("candidate")).toEqual(["dev"]);
    expect(identityForRole("candidate").name).toBe("mi-candidate");
  });

  test("both environment names come from the deployment", () => {
    vi.stubEnv("INFISICAL_ENVIRONMENT", "development");
    vi.stubEnv("INFISICAL_STAGING_ENVIRONMENT", "stage");
    expect(environmentsForRole("lead")).toEqual(["development", "stage"]);
    expect(environmentsForRole("candidate")).toEqual(["development"]);
  });

  test("a deployment that points both names at one environment pulls it once", () => {
    // Otherwise the CLI would export the same environment twice and write the
    // second copy over the first under a different filename.
    vi.stubEnv("INFISICAL_STAGING_ENVIRONMENT", "dev");
    expect(environmentsForRole("lead")).toEqual(["dev"]);
  });

  test("a deployment can switch staging off entirely", () => {
    // An org with no staging environment in Infisical: asking for one would
    // fail every developer's run rather than degrade.
    vi.stubEnv("INFISICAL_STAGING_ENVIRONMENT", "");
    expect(environmentsForRole("lead")).toEqual(["dev"]);
  });
});

describe("which folders a credential is minted to export", () => {
  test("one folder is the ordinary case and stays one", () => {
    vi.stubEnv("INFISICAL_SECRET_PATH", "/apps");
    expect(secretPaths()).toEqual(["/apps"]);
  });

  test("a deployment whose environment is several folders names them all", () => {
    // AI Builders since 2026-08-29: the VITE_* in one folder, the admin key
    // and the developer-only secrets in another, and a `.env.dev` carrying
    // either half alone does not start the app.
    vi.stubEnv(
      "INFISICAL_SECRET_PATH",
      "/tenant/aibuilders/frontend, /tenant/aibuilders/convex",
    );
    expect(secretPaths()).toEqual([
      "/tenant/aibuilders/frontend",
      "/tenant/aibuilders/convex",
    ]);
  });

  test("a deployment that names nothing gets the root it always got", () => {
    // Both spellings of "nothing": unset, and set to separators alone.
    expect(secretPaths()).toEqual(["/"]);
    vi.stubEnv("INFISICAL_SECRET_PATH", " , ");
    expect(secretPaths()).toEqual(["/"]);
  });
});

/* -------------------------------------------------------------------------- */
/* Discovering which environments a repository's folders are in               */
/* -------------------------------------------------------------------------- */

/**
 * A project holding `environments`, whose folders are described by `folders` —
 * a map from `<environment> <parent path>` to the folder names inside it.
 *
 * Written as a stub rather than against a live Infisical because these tests
 * are about the *readings* riabuild takes from an answer, and the two that
 * matter most are the ones a live project would not produce on demand: a folder
 * one environment has and another does not, and a 403 on the listing.
 */
function stubProject(project: {
  environments: string[];
  folders: Record<string, string[]>;
  folderStatus?: number;
  workspaceStatus?: number;
}) {
  const asked: string[] = [];
  stubFetch(async (input: RequestInfo | URL) => {
    const url = input instanceof Request ? input.url : input.toString();
    if (url.includes("/auth/universal-auth/login")) {
      return Response.json({ accessToken: "tok", expiresIn: 300 });
    }
    if (url.includes("/api/v1/workspace/")) {
      if (project.workspaceStatus !== undefined) {
        return new Response(null, { status: project.workspaceStatus });
      }
      return Response.json({
        workspace: {
          environments: project.environments.map((slug) => ({ slug })),
        },
      });
    }
    if (url.includes("/api/v1/folders")) {
      if (project.folderStatus !== undefined) {
        return new Response(null, { status: project.folderStatus });
      }
      const query = new URL(url).searchParams;
      const key = `${query.get("environment")} ${query.get("path")}`;
      asked.push(key);
      return Response.json({
        folders: (project.folders[key] ?? []).map((name) => ({ name })),
      });
    }
    throw new Error(`unexpected fetch to ${url}`);
  });
  return asked;
}

function stubDeveloperIdentity() {
  vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_ID", "dev-id");
  vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_SECRET", "dev-secret");
  vi.stubEnv("INFISICAL_PROJECT_ID", "proj");
}

describe("reading a project's environments out of Infisical's answer", () => {
  test("both response shapes are accepted", () => {
    // Infisical has served the workspace under a `workspace` key and at the top
    // level, and a project fetch that parses on one deployment and not on
    // another would be a fleet-wide failure nobody could act on.
    const environments = [{ slug: "dev" }, { slug: "prod" }];
    expect(readEnvironmentSlugs({ workspace: { environments } })).toEqual([
      "dev",
      "prod",
    ]);
    expect(readEnvironmentSlugs({ environments })).toEqual(["dev", "prod"]);
  });

  test("anything that is not a slug is dropped rather than passed on", () => {
    // These names become `--env=` arguments and the tail of `.env.<name>` on
    // somebody's laptop.
    expect(
      readEnvironmentSlugs({
        environments: [
          { slug: "dev" },
          { slug: "" },
          { slug: 7 },
          null,
          "prod",
        ],
      }),
    ).toEqual(["dev"]);
  });

  test("an answer with no environments in it is empty rather than a throw", () => {
    for (const body of [
      null,
      "",
      {},
      { workspace: null },
      { environments: 3 },
    ]) {
      expect(readEnvironmentSlugs(body)).toEqual([]);
    }
  });
});

describe("splitting a folder into the listing that would find it", () => {
  test("a nested folder is looked for inside its parent", () => {
    expect(splitFolder("/apps/payments")).toEqual({
      parent: "/apps",
      leaf: "payments",
    });
  });

  test("a top-level folder is looked for inside the root", () => {
    // `""` rather than `"/"` is the bug this exists to avoid: Infisical reads
    // an empty path as the project root of no environment at all.
    expect(splitFolder("/payments")).toEqual({ parent: "/", leaf: "payments" });
  });
});

describe("which environments a repository's folders are actually in", () => {
  test("the root is every environment's, and costs no listing at all", async () => {
    stubDeveloperIdentity();
    const asked = stubProject({
      environments: ["dev", "staging"],
      folders: {},
    });

    expect(await discoverEnvironments("developer", ["/"])).toEqual({
      status: "ok",
      environments: ["dev", "staging"],
    });
    expect(asked).toEqual([]);
  });

  test("an environment without the folder is left out", async () => {
    // The whole reason `environmentsForRole` had to go: a deployment's two
    // environment variables described a project they are not part of, so a team
    // whose `prod` existed could not receive `.env.prod` and a team with no
    // staging failed every run against one Infisical does not have.
    stubDeveloperIdentity();
    stubProject({
      environments: ["dev", "staging", "prod"],
      folders: { "dev /apps": ["payments"], "prod /apps": ["payments"] },
    });

    expect(await discoverEnvironments("developer", ["/apps/payments"])).toEqual(
      {
        status: "ok",
        environments: ["dev", "prod"],
      },
    );
  });

  test("every folder, not any folder", async () => {
    // `env_local` exports the list as a fold, so a missing folder is a 404 that
    // fails the whole pull. Half a `.env.dev` that starts nothing is worse than
    // no `.env.dev` and a line saying which environment was skipped.
    stubDeveloperIdentity();
    stubProject({
      environments: ["dev", "staging"],
      folders: {
        "dev /apps": ["payments", "shared"],
        "staging /apps": ["payments"],
      },
    });

    expect(
      await discoverEnvironments("developer", [
        "/apps/payments",
        "/apps/shared",
      ]),
    ).toEqual({ status: "ok", environments: ["dev"] });
  });

  test("the base environment is named first, and the rest keep Infisical's order", async () => {
    // `.env.dev` stays the file the CLI's own notes lead with.
    stubDeveloperIdentity();
    stubProject({
      environments: ["prod", "staging", "dev"],
      folders: {},
    });

    expect(await discoverEnvironments("developer", ["/"])).toEqual({
      status: "ok",
      environments: ["dev", "prod", "staging"],
    });
  });

  test("a candidate is asked about the base environment alone", async () => {
    // The same narrowing `identityForRole` already makes: naming `prod` on a
    // candidate's behalf buys nothing but a denial their developer cannot act
    // on.
    vi.stubEnv("INFISICAL_CANDIDATE_CLIENT_ID", "cand-id");
    vi.stubEnv("INFISICAL_CANDIDATE_CLIENT_SECRET", "cand-secret");
    vi.stubEnv("INFISICAL_PROJECT_ID", "proj");
    stubProject({ environments: ["dev", "staging", "prod"], folders: {} });

    expect(await discoverEnvironments("candidate", ["/"])).toEqual({
      status: "ok",
      environments: ["dev"],
    });
  });

  test("the base environment is the deployment's, not the word dev", async () => {
    stubDeveloperIdentity();
    vi.stubEnv("INFISICAL_ENVIRONMENT", "development");
    stubProject({ environments: ["staging", "development"], folders: {} });

    expect(await discoverEnvironments("developer", ["/"])).toEqual({
      status: "ok",
      environments: ["development", "staging"],
    });
  });

  test("a listing this credential is refused drops that environment, not the run", async () => {
    // Which is how a narrower identity is kept out of an environment without
    // this file holding a second copy of Infisical's RBAC.
    stubDeveloperIdentity();
    stubProject({
      environments: ["dev", "prod"],
      folders: {},
      folderStatus: 403,
    });

    expect(await discoverEnvironments("developer", ["/apps/payments"])).toEqual(
      {
        status: "ok",
        environments: [],
      },
    );
  });

  test("folders no environment holds is an answer, and it is not an error", async () => {
    // It has to reach the CLI as `ok` with an empty list, because the sentence
    // a developer needs is "no environment has the folders this repository is
    // mapped to" — not "riabuild could not find out", which is a different
    // fact with a different fix.
    stubDeveloperIdentity();
    stubProject({ environments: ["dev"], folders: { "dev /apps": ["other"] } });

    expect(await discoverEnvironments("developer", ["/apps/payments"])).toEqual(
      {
        status: "ok",
        environments: [],
      },
    );
  });

  test("a project riabuild cannot list is an upstream error, never no environments", async () => {
    // The distinction the CLI turns into `Unavailable` rather than `Unmapped`:
    // "we could not tell" must never render as "you have no secrets".
    stubDeveloperIdentity();
    stubProject({ environments: [], folders: {}, workspaceStatus: 500 });

    const result = await discoverEnvironments("developer", ["/"]);
    expect(result.status).toBe("upstream_error");
  });

  test("a deployment with no machine identity says what to set", async () => {
    vi.stubEnv("INFISICAL_PROJECT_ID", "proj");
    const result = await discoverEnvironments("developer", ["/"]);
    expect(result.status).toBe("not_configured");
    if (result.status === "ok") throw new Error("unreachable");
    expect(result.detail).toContain("INFISICAL_DEVELOPER_CLIENT_ID");
  });
});

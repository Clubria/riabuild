import { beforeEach, describe, expect, test, vi } from "vitest";
import { api } from "./_generated/api";
import {
  ApiError,
  auditRows,
  bearer,
  currentVersion,
  issueSession,
  json,
  seedMember,
  setup,
  stubFetch,
  TestConvex,
} from "./testing.fixtures";

/**
 * `GET /api/v1/secrets/scope`, and the `repo` field `POST /api/v1/secrets/token`
 * gained beside it.
 *
 * The two rules under test are the ones the whole feature rests on, and both are
 * about what riabuild says rather than what it fetches: **an unmapped repository
 * is a 200 saying so, never a 404**, and a CLI that names no repository gets
 * exactly what it always got.
 */

type ScopeReply = {
  repo: string;
  configured: boolean;
  secretPaths: string[];
  environments: string[];
  updatedAt: number;
  secretsUpdatedAt: number;
};

type TokenReply = {
  token: string;
  environments: string[];
  secretPaths: string[];
  configured?: boolean;
};

const HUB = "Clubria/ai-builders-hub";

describe("the repository-scoped answers", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_ID", "client-id");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_SECRET", "client-secret");
    vi.stubEnv("INFISICAL_PROJECT_ID", "proj_1");
  });

  /**
   * GitHub says yes, Infisical logs the identity in, and the project holds
   * `environments` — each of which contains every folder named in `folders`.
   *
   * `logins` counts the universal-auth calls, because "no credential is minted
   * for an unmapped repository" is an assertion about a request that must not
   * happen.
   */
  function stubUpstreams(project: {
    environments: string[];
    folders?: Record<string, string[]>;
  }) {
    const logins: string[] = [];
    stubFetch(async (input) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        return new Response(null, { status: 204 });
      }
      if (url.includes("universal-auth/login")) {
        logins.push(url);
        return Response.json({ accessToken: "inf_token", expiresIn: 300 });
      }
      if (url.includes("/api/v1/workspace/")) {
        return Response.json({
          workspace: {
            environments: project.environments.map((slug) => ({ slug })),
          },
        });
      }
      if (url.includes("/api/v1/folders")) {
        const query = new URL(url).searchParams;
        const key = `${query.get("environment")} ${query.get("path")}`;
        return Response.json({
          folders: (project.folders?.[key] ?? []).map((name) => ({ name })),
        });
      }
      throw new Error(`unexpected fetch to ${url}`);
    });
    return logins;
  }

  /** A lead maps `repo` to `paths`, through the mutation a lead would use. */
  async function mapRepo(t: TestConvex, repo: string, paths: string[]) {
    const { userId } = await seedMember(t, { login: "grace", role: "lead" });
    await t
      .withIdentity({ subject: `${userId}|session` })
      .mutation(api.secretPaths.set, { repoSlug: repo, secretPaths: paths });
  }

  test("a repository nobody mapped is a 200 saying so, not a 404", async () => {
    // The distinction a status code cannot carry: an older riabuild-web has no
    // such route and answers 404, and the CLI reads that as "this deployment
    // has no mapping table" and falls back to the org-wide list. If an
    // unmapped repository answered 404 too, a lead's decision would be
    // indistinguishable from a deployment that never heard of it.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ environments: ["dev"] });

    const response = await t.fetch(
      `/api/v1/secrets/scope?repo=${encodeURIComponent("Clubria/marketing")}`,
      { headers: bearer(token) },
    );

    expect(response.status).toBe(200);
    const body = await json<ScopeReply>(response);
    expect(body.configured).toBe(false);
    expect(body.secretPaths).toEqual([]);
    expect(body.environments).toEqual([]);
  });

  test("a mapped repository carries its folders and the environments that hold them", async () => {
    const t = setup();
    await mapRepo(t, HUB, ["/apps/hub"]);
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({
      environments: ["dev", "staging", "prod"],
      folders: { "dev /apps": ["hub"], "prod /apps": ["hub"] },
    });

    const response = await t.fetch(
      `/api/v1/secrets/scope?repo=${encodeURIComponent(HUB)}`,
      { headers: bearer(token) },
    );

    expect(response.status).toBe(200);
    const body = await json<ScopeReply>(response);
    expect(body).toMatchObject({
      repo: HUB,
      configured: true,
      secretPaths: ["/apps/hub"],
      environments: ["dev", "prod"],
    });
    expect(body.updatedAt).toBeGreaterThan(0);
  });

  test("asking about no repository at all is refused rather than guessed at", async () => {
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ environments: ["dev"] });

    const response = await t.fetch("/api/v1/secrets/scope", {
      headers: bearer(token),
    });

    expect(response.status).toBe(400);
    const body = await json<ApiError>(response);
    expect(body.error.code).toBe("bad_request");
  });

  test("a name the CLI could not read is a 400, in the developer's terms", async () => {
    // The lead's own wording is written for somebody editing the dashboard;
    // the person reading this is a developer whose run just stopped.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ environments: ["dev"] });

    const response = await t.fetch("/api/v1/secrets/scope?repo=not-a-slug", {
      headers: bearer(token),
    });

    expect(response.status).toBe(400);
    const body = await json<ApiError>(response);
    expect(body.error.action).toMatch(/team lead/i);
  });

  test("someone who has left the GitHub org is refused, mapping or not", async () => {
    // The non-negotiable one: this route ships the shape of the team's
    // Infisical project, so a Convex row cannot outvote GitHub here either.
    const t = setup();
    await mapRepo(t, HUB, ["/"]);
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubFetch(async (input) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        return new Response(null, { status: 404 });
      }
      throw new Error(`unexpected fetch to ${url}`);
    });

    const response = await t.fetch(
      `/api/v1/secrets/scope?repo=${encodeURIComponent(HUB)}`,
      { headers: bearer(token) },
    );

    expect(response.status).toBe(403);
  });

  test("a request with no session is refused", async () => {
    const t = setup();
    const response = await t.fetch(
      `/api/v1/secrets/scope?repo=${encodeURIComponent(HUB)}`,
      { headers: currentVersion },
    );
    expect(response.status).toBe(401);
  });

  test("the answer is not recomputed for the same question twice", async () => {
    // One run asks `/scope` and then `/token`, and both resolve the same
    // repository. Without the cache that is two workspace fetches and two
    // folder listings per environment, on every run of every laptop.
    const t = setup();
    await mapRepo(t, HUB, ["/"]);
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    const logins = stubUpstreams({ environments: ["dev", "staging"] });

    await t.fetch(`/api/v1/secrets/scope?repo=${encodeURIComponent(HUB)}`, {
      headers: bearer(token),
    });
    const first = logins.length;
    await t.fetch(`/api/v1/secrets/scope?repo=${encodeURIComponent(HUB)}`, {
      headers: bearer(token),
    });

    expect(logins.length).toBe(first);
  });
});

describe("the token request, once it names a repository", () => {
  beforeEach(() => {
    vi.stubEnv("GITHUB_ORG_TOKEN", "ghp_test");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_ID", "client-id");
    vi.stubEnv("INFISICAL_DEVELOPER_CLIENT_SECRET", "client-secret");
    vi.stubEnv("INFISICAL_PROJECT_ID", "proj_1");
    vi.stubEnv("INFISICAL_SECRET_PATH", "/deployment-wide");
  });

  function stubUpstreams(project: {
    environments: string[];
    folders?: Record<string, string[]>;
  }) {
    const logins: string[] = [];
    stubFetch(async (input) => {
      const url = input instanceof Request ? input.url : input.toString();
      if (url.includes("api.github.com")) {
        return new Response(null, { status: 204 });
      }
      if (url.includes("universal-auth/login")) {
        logins.push(url);
        return Response.json({ accessToken: "inf_token", expiresIn: 300 });
      }
      if (url.includes("/api/v1/workspace/")) {
        return Response.json({
          workspace: {
            environments: project.environments.map((slug) => ({ slug })),
          },
        });
      }
      if (url.includes("/api/v1/folders")) {
        const query = new URL(url).searchParams;
        const key = `${query.get("environment")} ${query.get("path")}`;
        return Response.json({
          folders: (project.folders?.[key] ?? []).map((name) => ({ name })),
        });
      }
      throw new Error(`unexpected fetch to ${url}`);
    });
    return logins;
  }

  async function mapRepo(t: TestConvex, repo: string, paths: string[]) {
    const { userId } = await seedMember(t, { login: "grace", role: "lead" });
    await t
      .withIdentity({ subject: `${userId}|session` })
      .mutation(api.secretPaths.set, { repoSlug: repo, secretPaths: paths });
  }

  test("a CLI that names no repository gets what it always got", async () => {
    // The add-only rule, applied to a field. Every riabuild in the field posts
    // `{}` and must keep receiving the deployment-wide answer.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ environments: ["dev", "staging"] });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
    });

    expect(response.status).toBe(200);
    const body = await json<TokenReply>(response);
    expect(body.token).toBe("inf_token");
    expect(body.environments).toEqual(["dev", "staging"]);
    expect(body.secretPaths).toEqual(["/deployment-wide"]);
  });

  test("a mapped repository is brokered for its own folders", async () => {
    const t = setup();
    await mapRepo(t, HUB, ["/apps/hub"]);
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({
      environments: ["dev", "staging"],
      folders: { "dev /apps": ["hub"] },
    });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
      body: JSON.stringify({ repo: HUB }),
    });

    expect(response.status).toBe(200);
    const body = await json<TokenReply>(response);
    expect(body.configured).toBe(true);
    expect(body.secretPaths).toEqual(["/apps/hub"]);
    expect(body.environments).toEqual(["dev"]);
  });

  test("an unmapped repository mints nothing and audits nothing", async () => {
    // The CLI asks `/scope` first, so reaching here unmapped is the race — a
    // lead removing the mapping between the two calls. Nothing was read, so
    // there is nothing to audit and no credential worth minting.
    const t = setup();
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    const logins = stubUpstreams({ environments: ["dev"] });

    const response = await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
      body: JSON.stringify({ repo: "Clubria/marketing" }),
    });

    expect(response.status).toBe(200);
    const body = await json<TokenReply>(response);
    expect(body.configured).toBe(false);
    expect(body.token).toBe("");
    expect(logins).toEqual([]);
    expect(
      (await auditRows(t)).filter((row) => row.action.startsWith("secrets.")),
    ).toEqual([]);
  });

  test("the repository is recorded on the audit row for a mapped one", async () => {
    const t = setup();
    await mapRepo(t, HUB, ["/"]);
    const { rowId } = await seedMember(t, { role: "developer" });
    const { token } = await issueSession(t, rowId);
    stubUpstreams({ environments: ["dev"] });

    await t.fetch("/api/v1/secrets/token", {
      method: "POST",
      headers: bearer(token),
      body: JSON.stringify({ repo: HUB }),
    });

    const brokered = (await auditRows(t)).filter((row) =>
      row.action.startsWith("secrets."),
    );
    expect(brokered).toHaveLength(1);
    expect(brokered[0].meta).toMatchObject({ repo: HUB });
  });
});

/**
 * Infisical universal-auth brokering.
 *
 * Service tokens and API keys were deprecated in April 2024 with a July 2024
 * migration deadline; machine identities with universal auth are the supported
 * non-interactive path. Do not reintroduce service tokens here.
 *
 * riabuild-web never sees a secret value. It exchanges a per-role machine
 * identity for a short-lived access token and hands that to the CLI, which pipes
 * it into `infisical export`. Path scoping is Infisical's RBAC, not our code.
 */

import { fetchUpstream } from "./lib/http";

export type Role = "candidate" | "developer" | "lead";

export type BrokerResult =
  | {
      status: "ok";
      token: string;
      expiresAt: number;
      identity: string;
      projectId: string;
      /**
       * The base environment, kept for CLIs released before `environments`
       * existed. It is always `environments[0]`.
       */
      environment: string;
      environments: string[];
      /**
       * The primary folder alone, kept for CLIs released before `secretPaths`
       * existed. It is always the LAST of `secretPaths` — see `secretPaths()`.
       */
      secretPath: string;
      secretPaths: string[];
      siteUrl: string;
    }
  | { status: "not_configured"; detail: string }
  | { status: "upstream_error"; detail: string };

/**
 * One machine identity per role — widest for a lead, narrowest for a candidate.
 * The difference is enforced inside Infisical; riabuild only chooses which
 * credential to authenticate as.
 *
 * `mi-lead` is the identity Infisical grants **everything** in the project:
 * writing secrets as well as reading them, creating and deleting the folders
 * they live in, certificate management, and every other subject the project
 * has. A developer keeps the identity that reads the team's paths, and a
 * candidate the subset of those.
 *
 * Which is why this function names three credentials and no permissions. The
 * permission set belongs to the identity in Infisical, where it is administered
 * and audited; a list of subjects held here would be riabuild-web deciding what
 * a laptop may do to the team's secrets, which is the boundary in
 * `../CLAUDE.md` seen from the authorization side. Widening a lead is a change
 * an Infisical admin makes to `mi-lead`, not a deploy of this file.
 *
 * **A deployment that has not created `mi-lead` yet keeps its leads on
 * `mi-developer`.** That is where every lead was brokered before this existed,
 * and failing their runs the moment this deploys — before anyone can set two
 * Convex environment variables — costs more than it buys. The fallback only
 * ever narrows: a deployment that has not opted in is never silently widened,
 * it carries on exactly as it did.
 */
export function identityForRole(role: Role): {
  name: string;
  clientIdVar: string;
  clientSecretVar: string;
} {
  if (role === "candidate") {
    return {
      name: "mi-candidate",
      clientIdVar: "INFISICAL_CANDIDATE_CLIENT_ID",
      clientSecretVar: "INFISICAL_CANDIDATE_CLIENT_SECRET",
    };
  }
  if (
    role === "lead" &&
    process.env.INFISICAL_LEAD_CLIENT_ID &&
    process.env.INFISICAL_LEAD_CLIENT_SECRET
  ) {
    return {
      name: "mi-lead",
      clientIdVar: "INFISICAL_LEAD_CLIENT_ID",
      clientSecretVar: "INFISICAL_LEAD_CLIENT_SECRET",
    };
  }
  return {
    name: "mi-developer",
    clientIdVar: "INFISICAL_DEVELOPER_CLIENT_ID",
    clientSecretVar: "INFISICAL_DEVELOPER_CLIENT_SECRET",
  };
}

/**
 * The Infisical environments this role may pull, in the order the CLI writes
 * them. The CLI turns each name into `.env.<name>` in the checkout.
 *
 * This is the same split `identityForRole` makes, and for the same reason: a
 * candidate is brokered through the narrower machine identity, so naming
 * staging on their behalf would buy nothing but an Infisical denial their
 * developer cannot act on. Infisical's RBAC remains the gate — this list only
 * decides what riabuild *asks* for.
 *
 * The list is data, not a path. It carries environment names; deciding what a
 * file is called from one is the CLI's job, so that a value chosen on the
 * server can never name a location on a laptop.
 */
export function environmentsForRole(role: Role): string[] {
  const base = process.env.INFISICAL_ENVIRONMENT ?? "dev";
  if (role === "candidate") return [base];

  // An org with no staging environment sets this empty rather than having
  // every developer's run fail against an environment Infisical does not have.
  const staging = process.env.INFISICAL_STAGING_ENVIRONMENT ?? "staging";
  if (!staging || staging === base) return [base];
  return [base, staging];
}

/**
 * The folders a credential is minted to export, in the order the CLI exports
 * them.
 *
 * `INFISICAL_SECRET_PATH` carries one folder or a comma-separated list of them,
 * because a team's secrets for one environment are not always in one folder. AI
 * Builders' are in two: since 2026-08-29 they live at
 * `/tenant/aibuilders/frontend` (the `VITE_*` the image bakes in) and
 * `/tenant/aibuilders/convex` (the admin key and the developer-only secrets),
 * and a `.env.dev` carrying either half alone does not start the app.
 *
 * **Order is the contract, and it is dotenv's own: later wins.** A key both
 * folders hold takes the value of the folder named later, which is why the
 * credential folder goes last — and why `secretPath`, which is what a bare
 * `infisical` command through the shim defaults to and all that a CLI released
 * before this field can read, is the last entry rather than the first. That
 * leaves an old CLI with the credential folder and without the frontend one,
 * which is the right half to keep: the `VITE_*` are client-public and
 * derivable, the admin key is neither.
 *
 * This is data and not a path on a laptop: the CLI still decides what file an
 * export lands in, the same way it does for an environment name.
 */
export function secretPaths(): string[] {
  const paths = (process.env.INFISICAL_SECRET_PATH ?? "/")
    .split(",")
    .map((path) => path.trim())
    .filter((path) => path.length > 0);
  // A variable holding nothing but separators is a misconfiguration, and the
  // root is what this brokered before the variable existed at all.
  return paths.length > 0 ? paths : ["/"];
}

export function siteUrl(): string {
  return (
    process.env.INFISICAL_SITE_URL?.replace(/\/+$/, "") ??
    "https://app.infisical.com"
  );
}

/**
 * The scope a credential is minted for, when the caller knows it.
 *
 * Present when the CLI named a repository, which is how the folders and the
 * environments stop being one answer for the whole deployment. Absent for every
 * CLI released before that, which still gets `secretPaths()` and
 * `environmentsForRole` — the compatibility rule in
 * `.agents/skills/riabuild-api/SKILL.md` applied to a field rather than to an
 * endpoint.
 */
export type Scope = { secretPaths: string[]; environments: string[] };

type LoginResult =
  | {
      status: "ok";
      token: string;
      expiresAt: number;
      identity: string;
      projectId: string;
    }
  | { status: "not_configured"; detail: string }
  | { status: "upstream_error"; detail: string };

/**
 * Universal-auth login as the role's machine identity.
 *
 * Shared by brokering and by folder discovery on purpose: discovery must see
 * exactly what the credential it describes will see, or riabuild would promise
 * a laptop a `.env.<name>` that `infisical export` then cannot fill.
 */
async function loginAs(role: Role): Promise<LoginResult> {
  const identity = identityForRole(role);
  const clientId = process.env[identity.clientIdVar];
  const clientSecret = process.env[identity.clientSecretVar];
  const projectId = process.env.INFISICAL_PROJECT_ID;

  if (!clientId || !clientSecret || !projectId) {
    return {
      status: "not_configured",
      detail:
        `set ${identity.clientIdVar}, ${identity.clientSecretVar} and ` +
        `INFISICAL_PROJECT_ID on the riabuild deployment`,
    };
  }

  let response: Response;
  try {
    response = await fetchUpstream(
      `${siteUrl()}/api/v1/auth/universal-auth/login`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ clientId, clientSecret }),
      },
    );
  } catch (error) {
    return {
      status: "upstream_error",
      detail: `could not reach Infisical: ${String(error)}`,
    };
  }

  if (!response.ok) {
    // Infisical's own errors name identities and paths a developer has no
    // context for, so only the status survives into the log.
    return {
      status: "upstream_error",
      detail: `Infisical returned ${response.status} for ${identity.name}`,
    };
  }

  const body = (await response.json()) as unknown;
  const accessToken = (body as { accessToken?: unknown }).accessToken;
  const expiresIn = (body as { expiresIn?: unknown }).expiresIn;
  if (typeof accessToken !== "string") {
    return {
      status: "upstream_error",
      detail: "Infisical did not return an access token",
    };
  }

  const ttlSeconds = typeof expiresIn === "number" ? expiresIn : 300;
  return {
    status: "ok",
    token: accessToken,
    expiresAt: Date.now() + ttlSeconds * 1000,
    identity: identity.name,
    projectId,
  };
}

export async function brokerToken(
  role: Role,
  scope?: Scope,
): Promise<BrokerResult> {
  const login = await loginAs(role);
  if (login.status !== "ok") return login;

  const environments = scope ? scope.environments : environmentsForRole(role);
  const paths = scope ? scope.secretPaths : secretPaths();

  return {
    status: "ok",
    token: login.token,
    expiresAt: login.expiresAt,
    identity: login.identity,
    projectId: login.projectId,
    environment: environments[0],
    environments,
    secretPath: paths[paths.length - 1],
    secretPaths: paths,
    siteUrl: siteUrl(),
  };
}

/* -------------------------------------------------------------------------- */
/* Which environments a repository's folders have                             */
/* -------------------------------------------------------------------------- */

export type DiscoveryResult =
  | { status: "ok"; environments: string[] }
  | { status: "not_configured"; detail: string }
  | { status: "upstream_error"; detail: string };

/**
 * The project's environment slugs, in the order Infisical lists them.
 *
 * Two response shapes are accepted because Infisical has served both — the
 * workspace under a `workspace` key, and at the top level — and a project fetch
 * that parses on one deployment and not on another would be a fleet-wide
 * failure nobody could act on. Anything that is not a non-empty string slug is
 * dropped rather than passed on: these names become `--env=` arguments and the
 * tail of `.env.<name>` on somebody's laptop, which is why the CLI checks them
 * again with `is_safe_environment_name`.
 */
export function readEnvironmentSlugs(body: unknown): string[] {
  const holder =
    typeof body === "object" && body !== null && "workspace" in body
      ? // `"workspace" in body` has already narrowed it, so no cast is needed
        // to reach the key — TypeScript's `in` narrowing does that much on its
        // own, and eslint refuses the assertion that says otherwise.
        body.workspace
      : body;
  if (typeof holder !== "object" || holder === null) return [];
  const raw = (holder as { environments?: unknown }).environments;
  if (!Array.isArray(raw)) return [];
  const slugs: string[] = [];
  for (const entry of raw) {
    if (typeof entry !== "object" || entry === null) continue;
    const slug = (entry as { slug?: unknown }).slug;
    if (typeof slug === "string" && slug !== "") slugs.push(slug);
  }
  return slugs;
}

/** `/apps/payments` → `{ parent: "/apps", leaf: "payments" }`. */
export function splitFolder(path: string): { parent: string; leaf: string } {
  const cut = path.lastIndexOf("/");
  return {
    parent: cut === 0 ? "/" : path.slice(0, cut),
    leaf: path.slice(cut + 1),
  };
}

/**
 * Whether `path` exists as a folder in `environment`.
 *
 * Asked by listing the *parent* and looking for the leaf, because Infisical's
 * folder listing describes what is inside a path rather than whether the path
 * is there — a listing of a folder that does not exist and a listing of an
 * empty one are the same empty array, and telling those apart is the whole job
 * of this function.
 *
 * The root is true without a request: every environment has one, and a team
 * whose secrets sit at `/` is the ordinary small case.
 *
 * A request that fails for any reason answers **false**, which is the
 * conservative direction: riabuild would rather leave out a file the developer
 * can ask a lead about than promise one `infisical export` cannot fill. That
 * includes the 403 a narrower identity gets, which is how a candidate is kept
 * out of an environment without this file holding a second copy of Infisical's
 * RBAC.
 */
async function folderExists(
  token: string,
  projectId: string,
  environment: string,
  path: string,
): Promise<boolean> {
  if (path === "/") return true;
  const { parent, leaf } = splitFolder(path);
  const url =
    `${siteUrl()}/api/v1/folders?workspaceId=${encodeURIComponent(projectId)}` +
    `&environment=${encodeURIComponent(environment)}` +
    `&path=${encodeURIComponent(parent)}`;

  let response: Response;
  try {
    response = await fetchUpstream(url, {
      headers: { authorization: `Bearer ${token}` },
    });
  } catch {
    return false;
  }
  if (!response.ok) return false;

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return false;
  }
  const folders = (body as { folders?: unknown }).folders;
  if (!Array.isArray(folders)) return false;
  return folders.some(
    (folder) =>
      typeof folder === "object" &&
      folder !== null &&
      (folder as { name?: unknown }).name === leaf,
  );
}

/**
 * The environments this role's credential will actually find every one of
 * `paths` in.
 *
 * This is what replaces `environmentsForRole` for a repository-scoped request,
 * and the difference is where the list is maintained. `dev` and `staging` were
 * two deployment environment variables describing a project they are not part
 * of: a team whose `prod` folder existed could not receive `.env.prod`, and a
 * team with no staging had to blank a variable to stop every run failing
 * against an environment Infisical does not have. Both are one bug — a list
 * kept in a second place from the thing it describes.
 *
 * **Every folder, not any folder.** A repository naming two folders counts an
 * environment only when that environment has both, because `env_local` exports
 * them as a fold and a missing one is a 404 that fails the whole pull. Half a
 * `.env.dev` that starts nothing is worse than no `.env.dev` and a line saying
 * which environment was skipped.
 *
 * **One role distinction survives.** A candidate gets the base environment
 * alone. That is not a second copy of Infisical's RBAC; it is the same
 * narrowing `identityForRole` already makes, kept because naming `prod` on a
 * candidate's behalf buys nothing but a denial their developer cannot act on.
 * Everyone else gets the folders' environments whole, which is the half that
 * was wrong.
 *
 * The base environment is ordered first when it is there, so `.env.dev` stays
 * the file the CLI's own notes lead with.
 */
export async function discoverEnvironments(
  role: Role,
  paths: string[],
): Promise<DiscoveryResult> {
  const login = await loginAs(role);
  if (login.status !== "ok") return login;

  let response: Response;
  try {
    response = await fetchUpstream(
      `${siteUrl()}/api/v1/workspace/${encodeURIComponent(login.projectId)}`,
      { headers: { authorization: `Bearer ${login.token}` } },
    );
  } catch (error) {
    return {
      status: "upstream_error",
      detail: `could not reach Infisical: ${String(error)}`,
    };
  }
  if (!response.ok) {
    return {
      status: "upstream_error",
      detail: `Infisical returned ${response.status} listing the project's environments`,
    };
  }

  let slugs: string[];
  try {
    slugs = readEnvironmentSlugs(await response.json());
  } catch {
    return {
      status: "upstream_error",
      detail: "Infisical did not describe the project's environments",
    };
  }

  const base = process.env.INFISICAL_ENVIRONMENT ?? "dev";
  const asked = role === "candidate" ? slugs.filter((s) => s === base) : slugs;

  const present: string[] = [];
  for (const slug of asked) {
    const all = await Promise.all(
      paths.map((path) =>
        folderExists(login.token, login.projectId, slug, path),
      ),
    );
    if (all.every(Boolean)) present.push(slug);
  }

  // A stable sort, so everything but the base keeps Infisical's own order.
  present.sort((a, b) => Number(b === base) - Number(a === base));
  return { status: "ok", environments: present };
}

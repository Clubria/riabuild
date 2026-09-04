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

export async function brokerToken(role: Role): Promise<BrokerResult> {
  const identity = identityForRole(role);
  const clientId = process.env[identity.clientIdVar];
  const clientSecret = process.env[identity.clientSecretVar];
  const projectId = process.env.INFISICAL_PROJECT_ID;
  const environments = environmentsForRole(role);
  const paths = secretPaths();

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
    environment: environments[0],
    environments,
    secretPath: paths[paths.length - 1],
    secretPaths: paths,
    siteUrl: siteUrl(),
  };
}

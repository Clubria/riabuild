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

export type Role = "candidate" | "developer" | "lead";

export type BrokerResult =
  | {
      status: "ok";
      token: string;
      expiresAt: number;
      identity: string;
      projectId: string;
      environment: string;
      secretPath: string;
      siteUrl: string;
    }
  | { status: "not_configured"; detail: string }
  | { status: "upstream_error"; detail: string };

/**
 * `candidate` gets a narrower identity than `developer`/`lead`. The difference
 * is enforced inside Infisical — riabuild only chooses which credential to use.
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
  return {
    name: "mi-developer",
    clientIdVar: "INFISICAL_DEVELOPER_CLIENT_ID",
    clientSecretVar: "INFISICAL_DEVELOPER_CLIENT_SECRET",
  };
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
  const environment = process.env.INFISICAL_ENVIRONMENT ?? "dev";
  const secretPath = process.env.INFISICAL_SECRET_PATH ?? "/";

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
    response = await fetch(`${siteUrl()}/api/v1/auth/universal-auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ clientId, clientSecret }),
    });
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
    environment,
    secretPath,
    siteUrl: siteUrl(),
  };
}

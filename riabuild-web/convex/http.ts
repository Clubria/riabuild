import { httpRouter } from "convex/server";
import { httpAction, ActionCtx } from "./_generated/server";
import { internal } from "./_generated/api";
import { auth } from "./auth";
import { Id } from "./_generated/dataModel";
import { pkceChallenge, randomToken, sha256Hex } from "./lib/crypto";
import { meetsMinimum } from "./lib/version";
import { ApiFailure, apiError, fail, jsonResponse } from "./lib/responses";
import { checkOrgMembership, orgLogin } from "./github";
import { brokerToken } from "./infisical";
import type { OrgConfig } from "./org";

const http = httpRouter();
auth.addHttpRoutes(http);

type MemberView = {
  _id: Id<"members">;
  githubLogin: string;
  githubId: string;
  firstName: string;
  lastName: string;
  email: string;
  role: "candidate" | "developer" | "lead";
  status: "active" | "suspended";
  joinedAt: number;
};

/** Wraps a handler so `fail(...)` unwinds to the prepared error response. */
function endpoint(
  handler: (ctx: ActionCtx, req: Request) => Promise<Response>,
): (ctx: ActionCtx, req: Request) => Promise<Response> {
  return async (ctx, req) => {
    try {
      return await handler(ctx, req);
    } catch (error) {
      if (error instanceof ApiFailure) return error.response;
      console.error("unhandled /api/v1 error", error);
      return apiError(
        500,
        "upstream_error",
        "riabuild hit an unexpected server error.",
        "Try again; if it keeps happening, tell your team lead.",
      );
    }
  };
}

function bearerToken(req: Request): string {
  const header = req.headers.get("authorization") ?? "";
  const match = /^Bearer\s+(.+)$/i.exec(header.trim());
  if (match === null) {
    fail(
      401,
      "unauthenticated",
      "riabuild is not signed in on this machine.",
      "Run `riabuild login`.",
    );
  }
  return match[1].trim();
}

async function authenticate(
  ctx: ActionCtx,
  req: Request,
): Promise<{ member: MemberView; sessionId: Id<"cliSessions"> }> {
  const tokenHash = await sha256Hex(bearerToken(req));
  const result = await ctx.runMutation(internal.sessions.authenticate, {
    tokenHash,
    now: Date.now(),
  });

  if (result.status === "ok") {
    return { member: result.member, sessionId: result.sessionId };
  }
  if (result.status === "expired") {
    fail(
      401,
      "session_expired",
      "Your riabuild session has expired.",
      "Run `riabuild login` to sign in again.",
    );
  }
  if (result.status === "revoked") {
    fail(
      401,
      "session_revoked",
      "This machine's riabuild session was revoked.",
      "Run `riabuild login` to sign in again.",
    );
  }
  if (result.status === "suspended") {
    // 403, not 401: re-authenticating would succeed and change nothing, so the
    // CLI must stop and explain rather than loop through the browser.
    fail(
      403,
      "suspended",
      "Your riabuild account is suspended.",
      "Ask your team lead to reactivate it.",
    );
  }
  fail(
    401,
    "unauthenticated",
    "riabuild is not signed in on this machine.",
    "Run `riabuild login`.",
  );
}

async function loadConfig(ctx: ActionCtx): Promise<OrgConfig> {
  return await ctx.runQuery(internal.org.forApi, {});
}

/**
 * `/org/config` and `/cli/token` deliberately never enforce this: the first is
 * how a CLI learns it must upgrade, and the second is how it signs in to be told
 * so. Enforcing everywhere would leave an old CLI with no path forward.
 */
function enforceMinVersion(req: Request, config: OrgConfig): void {
  const version = req.headers.get("x-riabuild-cli-version");
  if (version === null) return;
  if (meetsMinimum(version, config.minCliVersion)) return;
  fail(
    409,
    "cli_too_old",
    `This riabuild is ${version}; the team requires ${config.minCliVersion} or newer.`,
    "Run `brew upgrade clubria/tap/riabuild`.",
  );
}

async function requireOrgMembership(login: string): Promise<void> {
  const result = await checkOrgMembership(login);
  if (result.status === "member") return;
  if (result.status === "not_member") {
    fail(
      403,
      "not_org_member",
      `Your GitHub account @${login} is not in the ${orgLogin()} organisation.`,
      "Ask your team lead to re-invite you on GitHub.",
    );
  }
  console.error("org membership check unavailable:", result.detail);
  fail(
    503,
    "org_check_unavailable",
    "riabuild could not check your GitHub organisation membership.",
    "Try again in a minute; if it persists, tell your team lead.",
  );
}

function memberPayload(member: MemberView) {
  return {
    githubLogin: member.githubLogin,
    githubId: member.githubId,
    firstName: member.firstName,
    lastName: member.lastName,
    email: member.email,
    role: member.role,
    status: member.status,
    joinedAt: member.joinedAt,
  };
}

/* -------------------------------------------------------------------------- */
/* POST /api/v1/cli/token — exchange a one-time code for a session token       */
/* -------------------------------------------------------------------------- */

http.route({
  path: "/api/v1/cli/token",
  method: "POST",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const body: unknown = await req.json().catch(() => null);
      const code = (body as { code?: unknown } | null)?.code;
      const verifier = (body as { verifier?: unknown } | null)?.verifier;
      if (typeof code !== "string" || typeof verifier !== "string") {
        fail(
          400,
          "bad_request",
          "riabuild sent a malformed sign-in request.",
          "Run `riabuild login` again.",
        );
      }

      const token = randomToken(32);
      const result = await ctx.runMutation(internal.cliAuth.redeem, {
        codeHash: await sha256Hex(code),
        computedChallenge: await pkceChallenge(verifier),
        tokenHash: await sha256Hex(token),
        now: Date.now(),
      });

      if (result.status !== "ok") {
        if (result.status === "suspended") {
          fail(
            403,
            "suspended",
            "Your riabuild account is suspended.",
            "Ask your team lead to reactivate it.",
          );
        }
        fail(
          401,
          "unauthenticated",
          "That sign-in link is no longer valid.",
          "Run `riabuild login` again.",
        );
      }

      return jsonResponse({
        token,
        expiresAt: result.expiresAt,
        member: memberPayload(result.member),
      });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* GET /api/v1/me — profile, role, status                                      */
/* -------------------------------------------------------------------------- */

http.route({
  path: "/api/v1/me",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      const { member } = await authenticate(ctx, req);
      return jsonResponse({ member: memberPayload(member) });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* GET /api/v1/org/config — repo slug and version floors                       */
/* -------------------------------------------------------------------------- */

http.route({
  path: "/api/v1/org/config",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      await authenticate(ctx, req);
      const config = await loadConfig(ctx);
      return jsonResponse({
        repoSlug: config.repoSlug,
        defaultProjectPath: config.defaultProjectPath,
        minCliVersion: config.minCliVersion,
        latestCliVersion: config.latestCliVersion,
        secretsUpdatedAt: config.secretsUpdatedAt,
      });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* GET /api/v1/org/claude-settings — org Claude Code settings JSON             */
/* -------------------------------------------------------------------------- */

http.route({
  path: "/api/v1/org/claude-settings",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      await authenticate(ctx, req);

      let settings: unknown;
      try {
        settings = JSON.parse(config.claudeSettings);
      } catch {
        console.error("orgConfig.claudeSettings is not valid JSON");
        fail(
          500,
          "not_configured",
          "The team's Claude Code settings are not valid JSON.",
          "Ask your team lead to fix them in the riabuild dashboard.",
        );
      }
      return jsonResponse({
        settings,
        updatedAt: config.claudeSettingsUpdatedAt,
      });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* POST /api/v1/secrets/token — short-lived Infisical access token             */
/* -------------------------------------------------------------------------- */

http.route({
  path: "/api/v1/secrets/token",
  method: "POST",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      const { member } = await authenticate(ctx, req);

      // The non-negotiable one: a Convex row cannot outvote GitHub.
      await requireOrgMembership(member.githubLogin);

      const broker = await brokerToken(member.role);
      if (broker.status === "not_configured") {
        console.error("infisical not configured:", broker.detail);
        fail(
          503,
          "not_configured",
          "riabuild is not connected to the team's secret store yet.",
          "Tell your team lead — the riabuild deployment needs its Infisical credentials.",
        );
      }
      if (broker.status === "upstream_error") {
        console.error("infisical broker error:", broker.detail);
        fail(
          503,
          "upstream_error",
          "riabuild could not get secrets from Infisical right now.",
          "Try again in a minute; if it persists, tell your team lead.",
        );
      }

      await ctx.runMutation(internal.audit.record, {
        memberId: member._id,
        action: "secrets.token_brokered",
        meta: {
          identity: broker.identity,
          role: member.role,
          environment: broker.environment,
        },
      });

      return jsonResponse({
        token: broker.token,
        expiresAt: broker.expiresAt,
        projectId: broker.projectId,
        environment: broker.environment,
        secretPath: broker.secretPath,
        siteUrl: broker.siteUrl,
        secretsUpdatedAt: config.secretsUpdatedAt,
      });
    }),
  ),
});

export default http;

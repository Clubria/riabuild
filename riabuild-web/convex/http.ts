import { httpRouter } from "convex/server";
import { httpAction, ActionCtx } from "./_generated/server";
import { internal } from "./_generated/api";
import { auth } from "./auth";
import { Id } from "./_generated/dataModel";
import {
  formatUserCode,
  randomToken,
  randomUserCode,
  sha256Hex,
} from "./lib/crypto";
import { DEVICE_CODE_TTL_MS, POLL_INTERVAL_SECONDS } from "./cliAuth";
import { meetsMinimum } from "./lib/version";
import { ApiFailure, apiError, fail, jsonResponse } from "./lib/responses";
import { checkOrgMembership, orgLogin } from "./github";
import { brokerToken, environmentsForRole } from "./infisical";
import { RETIRED_DEFAULT_PROJECT_PATH, type OrgConfig } from "./org";

const http = httpRouter();
auth.addHttpRoutes(http);

type MemberView = {
  _id: Id<"members">;
  memberId: string;
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
 * The dashboard a developer is sent to, which is not this origin — `/api/v1` is
 * served from the Convex deployment while the pages are on Cloudflare.
 *
 * `SITE_URL` rather than a new variable of our own: the deployment already sets
 * it for auth redirects, and it already means "where the dashboard lives". A
 * second variable holding the same answer is a second variable that can
 * disagree with the first, and the failure would be a verification link
 * pointing somewhere nobody is signed in.
 */
function dashboardUrl(): string {
  const configured = process.env.SITE_URL ?? "https://riabuild.clubria.com";
  return configured.replace(/\/+$/, "");
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
    memberId: member.memberId,
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
/* POST /api/v1/cli/device — start a device authorisation                      */
/* -------------------------------------------------------------------------- */

/**
 * Unauthenticated: this is how a machine *becomes* authenticated.
 *
 * It is also the one place the version floor reaches a machine that has never
 * signed in. `/api/v1/org/config` carries `minCliVersion` but requires a
 * session, so before this endpoint existed an unsigned machine on an old build
 * had no way to be told it had to upgrade.
 */
http.route({
  path: "/api/v1/cli/device",
  method: "POST",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);

      const body: unknown = await req.json().catch(() => null);
      const rawLabel = (body as { deviceLabel?: unknown } | null)?.deviceLabel;
      if (rawLabel !== undefined && typeof rawLabel !== "string") {
        fail(
          400,
          "bad_request",
          "riabuild sent a malformed sign-in request.",
          "Run `riabuild login` again.",
        );
      }

      const deviceLabel = (rawLabel ?? "").slice(0, 80) || "unknown device";
      const cliVersion =
        (req.headers.get("x-riabuild-cli-version") ?? "").slice(0, 32) ||
        "unknown";

      const deviceCode = randomToken(32);
      const expiresAt = Date.now() + DEVICE_CODE_TTL_MS;

      // Retried rather than assumed unique: a collision would wire one
      // developer's approval screen to another developer's terminal, and it
      // would do it silently.
      let userCode = "";
      for (let attempt = 0; attempt < 5; attempt++) {
        const candidate = randomUserCode();
        const result = await ctx.runMutation(internal.cliAuth.startDevice, {
          deviceCodeHash: await sha256Hex(deviceCode),
          userCode: candidate,
          deviceLabel,
          cliVersion,
          expiresAt,
          now: Date.now(),
        });
        if (result.status === "ok") {
          userCode = candidate;
          break;
        }
      }
      if (userCode === "") {
        console.error("could not mint a free user code in five attempts");
        fail(
          500,
          "upstream_error",
          "riabuild could not start a sign-in just now.",
          "Try `riabuild login` again in a moment.",
        );
      }

      const verificationUri = `${dashboardUrl()}/cli`;
      return jsonResponse({
        deviceCode,
        userCode: formatUserCode(userCode),
        verificationUri,
        verificationUriComplete: `${verificationUri}?code=${formatUserCode(userCode)}`,
        // Relative seconds, unlike `expiresAt` elsewhere in this API: riabuild's
        // first run happens on freshly provisioned machines where NTP may not
        // have settled, and a skewed clock would make the CLI abandon a live
        // code or keep polling a dead one. A duration is immune to that.
        expiresIn: Math.round(DEVICE_CODE_TTL_MS / 1000),
        interval: POLL_INTERVAL_SECONDS,
      });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* POST /api/v1/cli/token — poll a device code, eventually for a session       */
/* -------------------------------------------------------------------------- */

/**
 * Polling states come back as 200 with a discriminated body rather than RFC
 * 8628's `400 authorization_pending`. "Not yet" is the expected answer in a
 * loop that runs dozens of times per login, and the CLI turns every non-2xx
 * into an error that unwinds — encoding the normal path that way would mean
 * reconstructing the happy path from an error code on every tick.
 */
http.route({
  path: "/api/v1/cli/token",
  method: "POST",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const body: unknown = await req.json().catch(() => null);
      const deviceCode = (body as { deviceCode?: unknown } | null)?.deviceCode;
      if (typeof deviceCode !== "string") {
        fail(
          400,
          "bad_request",
          "riabuild sent a malformed sign-in request.",
          "Run `riabuild login` again.",
        );
      }

      const token = randomToken(32);
      const result = await ctx.runMutation(internal.cliAuth.redeem, {
        deviceCodeHash: await sha256Hex(deviceCode),
        tokenHash: await sha256Hex(token),
        now: Date.now(),
      });

      if (result.status === "pending") {
        return jsonResponse({
          status: "pending",
          interval: POLL_INTERVAL_SECONDS,
        });
      }
      if (result.status === "denied") {
        return jsonResponse({ status: "denied" });
      }
      if (result.status === "suspended") {
        fail(
          403,
          "suspended",
          "Your riabuild account is suspended.",
          "Ask your team lead to reactivate it.",
        );
      }
      if (result.status !== "ok") {
        fail(
          401,
          "unauthenticated",
          "That sign-in request is no longer valid.",
          "Run `riabuild login` again.",
        );
      }

      return jsonResponse({
        status: "ok",
        token,
        // Additive field: `redeem` already computed this for the audit log,
        // it was just never handed back before. `riabuild remote forget`
        // needs it to name the exact `cliSessions` row a server's own
        // session lives in when it calls `DELETE /api/v1/cli/sessions/<id>`
        // — see convex/sessions.ts's `revokeById`.
        sessionId: result.sessionId,
        expiresAt: result.expiresAt,
        member: memberPayload(result.member),
      });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* POST /api/v1/cli/sessions — a signed-in laptop signs a server in            */
/* -------------------------------------------------------------------------- */

/**
 * Delegation: the one way a riabuild session is created without a human
 * approving a device code.
 *
 * `riabuild remote` needs a session for the server it is provisioning, and it
 * runs on a laptop that signed in minutes ago. It used to get one by driving
 * the whole device-code flow a second time — printing a second code, opening a
 * second browser tab, waiting for a second approval — which asked the
 * developer to prove, again, the thing the bearer token on this very request
 * already proves. The server still cannot sign itself in; nothing here gives
 * it a way to. Its laptop asks on its behalf.
 *
 * Every gate the browser flow had is still here, and two are stricter:
 *
 * - the caller must hold a live session for an active member (`authenticate`);
 * - it must still be in the GitHub org, re-checked against GitHub on this
 *   request — the browser flow only ever checked at sign-in, which may have
 *   been months ago;
 * - and the caller's own session must be a `device` one. A delegated session
 *   cannot delegate. See `sessions.delegate`.
 */
http.route({
  path: "/api/v1/cli/sessions",
  method: "POST",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      const { member, sessionId } = await authenticate(ctx, req);
      // Non-negotiable, as on /secrets/token: this hands out a live 90-day
      // credential, so a Convex row cannot outvote GitHub.
      await requireOrgMembership(member.githubLogin);

      const body: unknown = await req.json().catch(() => null);
      const rawLabel = (body as { deviceLabel?: unknown } | null)?.deviceLabel;
      if (rawLabel !== undefined && typeof rawLabel !== "string") {
        fail(
          400,
          "bad_request",
          "riabuild sent a malformed request for a server session.",
          "Run `riabuild remote` again.",
        );
      }

      const deviceLabel = (rawLabel ?? "").slice(0, 80) || "unknown device";
      const cliVersion =
        (req.headers.get("x-riabuild-cli-version") ?? "").slice(0, 32) ||
        "unknown";

      const token = randomToken(32);
      const result = await ctx.runMutation(internal.sessions.delegate, {
        parentSessionId: sessionId,
        tokenHash: await sha256Hex(token),
        deviceLabel,
        cliVersion,
      });

      if (result.status !== "ok") {
        // 403 rather than 401: this session is valid and will stay valid, so
        // re-authenticating would succeed and change nothing. The CLI has to
        // stop and say where to run the command instead.
        fail(
          403,
          "delegation_not_permitted",
          "This machine's riabuild session was itself signed in by another machine, so it cannot sign a third one in.",
          "Run `riabuild remote` from your own laptop.",
        );
      }

      return jsonResponse({
        token,
        // The handle `riabuild remote forget` revokes this by, through
        // `DELETE /api/v1/cli/sessions/<id>`.
        sessionId: result.sessionId,
        expiresAt: result.expiresAt,
        member: memberPayload(member),
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
      const { member } = await authenticate(ctx, req);
      const config = await loadConfig(ctx);
      return jsonResponse({
        repoSlug: config.repoSlug,
        // Frozen, not read from config: this field is retired and only still
        // here because a CLI released before the change cannot parse a response
        // without it. Current CLIs ignore it and choose the path themselves.
        defaultProjectPath: RETIRED_DEFAULT_PROJECT_PATH,
        minCliVersion: config.minCliVersion,
        latestCliVersion: config.latestCliVersion,
        secretsUpdatedAt: config.secretsUpdatedAt,
        // The same list `/secrets/token` returns. It is here as well because
        // the CLI's `check()` has to know which `.env.<name>` files ought to
        // exist on every run, and brokering a token to find out would hit
        // Infisical and write an audit row for a question nobody asked.
        secretEnvironments: environmentsForRole(member.role),
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
/* GET /api/v1/remotes/shared — the addresses of the team's servers            */
/* -------------------------------------------------------------------------- */

http.route({
  path: "/api/v1/remotes/shared",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      const { member } = await authenticate(ctx, req);
      // Re-verified here as everywhere: identity lives in GitHub, and someone
      // removed from the org must stop being handed the team's machines
      // without anyone remembering to update their Convex row.
      await requireOrgMembership(member.githubLogin);

      // A candidate gets an empty list and a 200, never a 403. `riabuild
      // remote` is also how they reach the server they set up themselves, and
      // refusing the whole request would take that away in order to enforce a
      // rule about servers they were never going to see.
      if (member.role === "candidate") {
        return jsonResponse({ servers: [] });
      }
      const servers = await ctx.runQuery(internal.sharedServers.forApi, {});
      return jsonResponse({ servers });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* GET /api/v1/issued-keys — the SSH keys issued to this developer             */
/* -------------------------------------------------------------------------- */

/**
 * The only response in riabuild that carries a durable credential.
 *
 * Everything else brokered here expires on its own — an Infisical token in
 * minutes, a session on revocation. A private SSH key does neither, which is
 * why this handler is the one place the org check is doing the whole job and
 * why the fetch itself is logged, inside `serveForApi`, next to the read.
 *
 * The private half travels in the same response as the metadata rather than
 * behind a second, separately authorised call. A second round trip would be
 * theatre: same session, same bearer token, same connection — and the CLI needs
 * every key it is entitled to anyway, because it probes them one at a time to
 * find which one the chosen server accepts.
 */
http.route({
  path: "/api/v1/issued-keys",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      const { member } = await authenticate(ctx, req);
      // `members.role` is never the sole gate, and on this endpoint that is not
      // a formality: a stale Convex row would otherwise keep handing a departed
      // developer a key that opens a machine indefinitely.
      await requireOrgMembership(member.githubLogin);

      // A candidate gets an empty list and a 200, for the reason
      // /api/v1/remotes/shared does. Returned before `serveForApi` rather than
      // through it, deliberately: nothing is served, so nothing was taken a
      // copy of, and an audit row here would read as though a candidate had
      // been handed keys.
      if (member.role === "candidate") {
        return jsonResponse({ keys: [] });
      }
      const keys = await ctx.runMutation(internal.issuedKeys.serveForApi, {
        memberId: member._id,
      });
      return jsonResponse({ keys });
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
          // Which environments one credential opened is the part worth being
          // able to answer later; `environment` alone cannot say "and staging".
          environments: broker.environments.join(","),
        },
      });

      return jsonResponse({
        token: broker.token,
        expiresAt: broker.expiresAt,
        projectId: broker.projectId,
        // The base environment alone, for CLIs released before `environments`.
        environment: broker.environment,
        environments: broker.environments,
        secretPath: broker.secretPath,
        siteUrl: broker.siteUrl,
        secretsUpdatedAt: config.secretsUpdatedAt,
      });
    }),
  ),
});

/* -------------------------------------------------------------------------- */
/* DELETE /api/v1/cli/sessions/<id> — revoke a session                        */
/* -------------------------------------------------------------------------- */

http.route({
  pathPrefix: "/api/v1/cli/sessions/",
  method: "DELETE",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const { member } = await authenticate(ctx, req);
      // The non-negotiable one, same as /secrets/token: a Convex row cannot
      // outvote GitHub. Revocation changes access, so it re-verifies too.
      await requireOrgMembership(member.githubLogin);

      const id = new URL(req.url).pathname.split("/").pop() ?? "";
      const result = await ctx.runMutation(internal.sessions.revokeById, {
        sessionId: id,
        actorId: member._id,
        isLead: member.role === "lead",
      });

      // "not_found" and "forbidden" collapse into the identical response: a
      // session id that belongs to somebody else must be indistinguishable
      // from one that never existed, or this endpoint becomes a way to probe
      // for live session ids one guess at a time.
      if (result === "not_found" || result === "forbidden") {
        fail(
          404,
          "session_unknown",
          "That session no longer exists.",
          "Run `riabuild remote list` to see what is left.",
        );
      }
      return jsonResponse({ revoked: true });
    }),
  ),
});

export default http;

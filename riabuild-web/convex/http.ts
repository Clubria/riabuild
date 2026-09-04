import { httpRouter } from "convex/server";
import { httpAction, ActionCtx } from "./_generated/server";
import { internal } from "./_generated/api";
import { auth } from "./auth";
import {
  formatUserCode,
  randomToken,
  randomUserCode,
  sha256Hex,
} from "./lib/crypto";
import { DEVICE_CODE_TTL_MS, POLL_INTERVAL_SECONDS } from "./cliAuth";
import { ApiFailure, apiError, fail, jsonResponse } from "./lib/responses";
import {
  guard,
  requireOrgMembership,
  versionGate,
  type MemberView,
} from "./lib/guard";
import { brokerToken, environmentsForRole } from "./infisical";
import { MAX_SAMPLES_PER_REQUEST } from "./usage";
import { RETIRED_DEFAULT_PROJECT_PATH } from "./org";

const http = httpRouter();
auth.addHttpRoutes(http);

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
      // No session yet, so the version floor is all there is to enforce.
      await versionGate(ctx, req);

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
 * The version floor is deliberately not enforced here: this is how a CLI below
 * the floor signs in so `/api/v1/org/config` can tell it to upgrade. Org
 * membership *is* re-verified, below — a stale floor is a compatibility
 * problem, a stale org row is an access one.
 *
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

      // Identity is GitHub, authorization is Convex — and this route was the
      // hole in that sentence. GitHub OAuth still succeeds for someone who was
      // removed from the org, so they could sign in, approve their own device
      // code and walk away holding a live 90-day session, refused only later
      // and only by whichever endpoint remembered to ask. `/api/v1/cli/sessions`
      // mints the identical credential and has always re-verified here.
      //
      // The floor stays exempt on this route (see the comment above it), but
      // membership is not a version question.
      try {
        await requireOrgMembership(result.member.githubLogin);
      } catch (error) {
        // `redeem` already burned the device code and inserted the row, so a
        // refusal here leaves a session nobody will ever hold the token for —
        // it is generated in this handler and discarded with the request. Left
        // alone it would still show up in the dashboard as live for ninety
        // days, so it is revoked on the way out.
        await ctx.runMutation(internal.sessions.revokeById, {
          sessionId: result.sessionId,
          actorId: result.member._id,
          isLead: false,
        });
        throw error;
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
      // Non-negotiable, as on /secrets/token: this hands out a live 90-day
      // credential, so a Convex row cannot outvote GitHub.
      const { member, sessionId } = await guard(ctx, req, {
        version: true,
        org: true,
      });

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

      // `delegate` re-reads the parent row rather than trusting what
      // `authenticate` saw a GitHub round trip ago, so these two are reachable
      // even though the request authenticated: the session was revoked or
      // expired in between. They are 401s, not 403s — signing in again is the
      // fix and it will work.
      if (result.status === "revoked") {
        fail(
          401,
          "session_revoked",
          "This machine's riabuild session was revoked while it was signing the server in.",
          "Run `riabuild login` to sign in again, then `riabuild remote`.",
        );
      }
      if (result.status === "expired") {
        fail(
          401,
          "session_expired",
          "This machine's riabuild session expired while it was signing the server in.",
          "Run `riabuild login` to sign in again, then `riabuild remote`.",
        );
      }
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
      // No org re-check: this returns a member's own profile and brokers
      // nothing, so it costs a GitHub round trip on every `riabuild --check`
      // and buys no access decision.
      const { member } = await guard(ctx, req, { version: true, org: false });
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
      // `version: false` is the documented exemption, not an oversight: this
      // is the route a CLI below the floor reads to learn it is below the
      // floor, and enforcing here would leave it no path forward.
      const { member, config } = await guard(ctx, req, {
        version: false,
        org: false,
      });
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
        // When a lead last set the team's ngrok authtoken, or 0 if none is set.
        // Metadata, never the token: it rides here so the CLI can say "your
        // lead has not set one" without calling /org/ngrok-token, which hands
        // out a live credential and writes an audit row.
        ngrokAuthTokenUpdatedAt: config.ngrokAuthTokenUpdatedAt,
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
      const { config } = await guard(ctx, req, { version: true, org: false });

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
/* GET /api/v1/org/ngrok-token — the team's ngrok authtoken                    */
/* -------------------------------------------------------------------------- */

/**
 * The second response that carries a durable credential, after `/issued-keys`.
 *
 * It is its own route rather than a field on `/org/config` because of what that
 * difference buys: `/org/config` is fetched on every `riabuild` run, and this
 * is fetched when somebody actually runs `ngrok`. The audit row therefore says
 * a developer used the team's tunnel credential, which — since ngrok sees one
 * account for the whole org — is the only attribution that exists.
 *
 * The token does not expire on its own, so the org check is doing the whole job
 * here, exactly as it is for an issued key.
 */
http.route({
  path: "/api/v1/org/ngrok-token",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      // `members.role` is never the sole gate: a developer who left the org
      // yesterday must lose the team's tunnel today, without anyone
      // remembering to edit a Convex row.
      const { member, config } = await guard(ctx, req, {
        version: true,
        org: true,
      });

      if (config.ngrokAuthToken === "") {
        // Nothing is broken and the CLI says so in the developer's terms — the
        // person who can fix this is not the person reading the message.
        fail(
          404,
          "not_configured",
          "Your team has not set an ngrok authtoken yet.",
          "Ask your team lead to add one in the riabuild dashboard, under org settings.",
        );
      }

      await ctx.runMutation(internal.audit.record, {
        memberId: member._id,
        action: "org.ngrok_token_fetched",
        meta: { role: member.role },
      });

      return jsonResponse({
        token: config.ngrokAuthToken,
        updatedAt: config.ngrokAuthTokenUpdatedAt,
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
      // Re-verified here as everywhere: identity lives in GitHub, and someone
      // removed from the org must stop being handed the team's machines
      // without anyone remembering to update their Convex row.
      const { member } = await guard(ctx, req, { version: true, org: true });

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
      // `members.role` is never the sole gate, and on this endpoint that is not
      // a formality: a stale Convex row would otherwise keep handing a departed
      // developer a key that opens a machine indefinitely.
      const { member } = await guard(ctx, req, { version: true, org: true });

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
      // The non-negotiable one: a Convex row cannot outvote GitHub.
      const { member, config } = await guard(ctx, req, {
        version: true,
        org: true,
      });

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
        // The primary folder alone, for CLIs released before `secretPaths`.
        secretPath: broker.secretPath,
        secretPaths: broker.secretPaths,
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
      // The non-negotiable one, same as /secrets/token: a Convex row cannot
      // outvote GitHub. Revocation changes access, so it re-verifies too.
      //
      // `version: false` is the third opt-out, and unlike the other two it is
      // being written down here for the first time — this route simply never
      // had the check. Keeping it out is the deliberate answer rather than the
      // inherited one: `riabuild remote forget` is how a leaked 90-day
      // credential gets pulled, and refusing to revoke a session because the
      // laptop asking is a version behind would block the one command that
      // must always work.
      const { member } = await guard(ctx, req, { version: false, org: true });

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

/* -------------------------------------------------------------------------- */
/* POST /api/v1/usage — session totals from the status line                    */
/* -------------------------------------------------------------------------- */

/**
 * The one write endpoint a laptop makes on its own schedule, and the only one
 * whose caller nobody is watching: `riabuild internal usage-flush` runs
 * detached beside an interactive Claude Code session and treats every failure
 * as "keep the spool, try again in a minute". So this handler is written for a
 * reader who will never see its message — the status code is the whole
 * conversation.
 *
 * `org: true` like every other route that carries member data. Usage is not a
 * secret being brokered, but it is a member's data being written under their
 * name, and a developer who left the org yesterday should stop filing rows
 * today without anybody remembering to edit a Convex row.
 *
 * Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md`.
 */
http.route({
  path: "/api/v1/usage",
  method: "POST",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const { member } = await guard(ctx, req, { version: true, org: true });

      const body: unknown = await req.json().catch(() => null);
      const samples = parseSamples(body);

      const result = await ctx.runMutation(internal.usage.record, {
        // From the session, never from the body. There is no `memberId` on the
        // wire at all: the flush has already proved who it is with a bearer
        // token, and a member named in the body would be a client-supplied
        // claim standing in front of one the request had proved.
        memberId: member._id,
        // Stamped here, from the server's clock. A laptop whose clock is wrong
        // — or a caller that would like its rows to outlive the reaper — does
        // not get to choose which window a sample lands in.
        observedAt: Math.floor(Date.now() / 1000),
        samples,
      });

      // No `auditLog` row, deliberately, and not one per sample either. See
      // `usage.record`: that table records changes to access, and a flush every
      // sixty seconds per active developer would bury every one of them.
      return jsonResponse({ accepted: result.accepted });
    }),
  ),
});

/** The one malformed-body failure this route has. */
function badUsageBody(): never {
  fail(
    400,
    "bad_request",
    "riabuild sent a usage report this server could not read.",
    "Upgrade riabuild with `brew upgrade clubria/tap/riabuild`; nothing else is affected.",
  );
}

/** Long enough for anything real; short enough that a field cannot be a payload. */
const MAX_USAGE_FIELD_LENGTH = 200;

function usageString(raw: unknown, required: boolean): string | undefined {
  if (raw === undefined || raw === null) {
    if (required) badUsageBody();
    return undefined;
  }
  if (typeof raw !== "string") badUsageBody();
  const value = raw.trim();
  if (value === "") {
    if (required) badUsageBody();
    return undefined;
  }
  return value.slice(0, MAX_USAGE_FIELD_LENGTH);
}

/**
 * A number, or nothing.
 *
 * `NaN` and `Infinity` are refused rather than stored: both pass
 * `typeof === "number"`, both come back out of `JSON.stringify` as `null`, and
 * one of either poisons every sum in the lead's rollup for the whole team. A
 * negative is refused for the same reason — every field here is a count, a
 * duration or a percentage, and none of them runs backwards.
 */
function usageNumber(raw: unknown): number | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (typeof raw !== "number" || !Number.isFinite(raw) || raw < 0) {
    badUsageBody();
  }
  return raw;
}

/**
 * `{ samples: [...] }`, narrowed.
 *
 * Unknown keys inside a sample are **ignored rather than refused**, which is
 * the compatibility rule read from the other side: a newer CLI that starts
 * reporting a field this deployment has not learned about yet must keep
 * working, and riabuild upgrades on every developer's own schedule. What is
 * refused is a body of the wrong *shape* — that one is a bug in one of the two
 * halves, and accepting it quietly would file rows nobody can read.
 */
function parseSamples(body: unknown) {
  if (typeof body !== "object" || body === null) badUsageBody();
  const raw = (body as { samples?: unknown }).samples;
  if (!Array.isArray(raw)) badUsageBody();

  // A bound on one transaction rather than a rate limit: the flush compacts its
  // spool to one line per session before sending, so a laptop that has been
  // offline for a week sends its session count and not its message count.
  // Refused rather than truncated — the CLI clears what the server said it
  // accepted, so a silently dropped tail is a total that is quietly wrong for
  // ever.
  if (raw.length > MAX_SAMPLES_PER_REQUEST) {
    fail(
      400,
      "bad_request",
      `riabuild sent ${raw.length} usage samples at once; this server takes ${MAX_SAMPLES_PER_REQUEST}.`,
      "Nothing is lost — riabuild sends them in smaller batches on its next try.",
    );
  }

  return raw.map((entry) => {
    if (typeof entry !== "object" || entry === null) badUsageBody();
    const sample = entry as Record<string, unknown>;
    const harness = usageString(sample.harness, true);
    const accountId = usageString(sample.accountId, true);
    const sessionId = usageString(sample.sessionId, true);
    if (
      harness === undefined ||
      accountId === undefined ||
      sessionId === undefined
    ) {
      badUsageBody();
    }
    return {
      harness,
      accountId,
      sessionId,
      model: usageString(sample.model, false),
      costUsd: usageNumber(sample.costUsd),
      durationMs: usageNumber(sample.durationMs),
      apiDurationMs: usageNumber(sample.apiDurationMs),
      linesAdded: usageNumber(sample.linesAdded),
      linesRemoved: usageNumber(sample.linesRemoved),
      fiveHourPct: usageNumber(sample.fiveHourPct),
      fiveHourResetsAt: usageNumber(sample.fiveHourResetsAt),
      sevenDayPct: usageNumber(sample.sevenDayPct),
      sevenDayResetsAt: usageNumber(sample.sevenDayResetsAt),
    };
  });
}

export default http;

import { ActionCtx } from "../_generated/server";
import { internal } from "../_generated/api";
import { Id } from "../_generated/dataModel";
import { sha256Hex } from "./crypto";
import { meetsMinimum } from "./version";
import { fail } from "./responses";
import { checkOrgMembership, orgLogin } from "../github";
import type { OrgConfig } from "../org";

/**
 * The one prologue every authenticated `/api/v1` route runs.
 *
 * It used to be four calls copy-pasted into each handler, in four different
 * orders with four different subsets — which meant the checklist in
 * `.claude/skills/riabuild-api/SKILL.md` was enforced by whichever handler the
 * next endpoint was pasted from. Two routes had already drifted: `/me` and
 * `/org/config` disagreed about ordering, and the DELETE route silently
 * carried no version check at all.
 *
 * Both flags are **required**, with no defaults. A new endpoint cannot compile
 * without stating whether it enforces the version floor and whether it
 * re-verifies GitHub org membership, so an omission is a decision somebody
 * wrote down rather than a line that never got pasted. That matters most for
 * `org`: "identity is GitHub, authorization is Convex" fails silently, and it
 * fails in the direction of a departed developer keeping access.
 */
export type MemberView = {
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

export type GuardOptions = {
  /**
   * Enforce `orgConfig.minCliVersion`. `false` is an opt-out that has to be
   * argued for at the call site — see `/org/config`, which is how a CLI below
   * the floor learns it is below the floor.
   */
  version: boolean;
  /**
   * Re-verify Clubria GitHub org membership against GitHub itself. `true` for
   * anything that hands out access; `members.role` is never the sole gate.
   */
  org: boolean;
};

export type Guarded = {
  config: OrgConfig;
  member: MemberView;
  sessionId: Id<"cliSessions">;
};

export async function loadConfig(ctx: ActionCtx): Promise<OrgConfig> {
  return await ctx.runQuery(internal.org.forApi, {});
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

export async function authenticate(
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

/**
 * An absent `x-riabuild-cli-version` is treated as version `0`, not as an
 * exemption.
 *
 * Returning early on a missing header made the floor opt-in from the client
 * side: anything that simply did not send it — a hand-rolled `curl`, or a CLI
 * whose `ApiClient::request` dropped the header through a bug — sailed past
 * `minCliVersion` on every route, and the one mechanism riabuild has for
 * forcing an upgrade quietly stopped applying to exactly the clients least
 * likely to be current.
 *
 * A floor of `0` (or `0.0.0`) still lets an unversioned caller through, which
 * is the correct reading of a team that has set no floor.
 */
export function enforceMinVersion(req: Request, config: OrgConfig): void {
  const header = req.headers.get("x-riabuild-cli-version");
  if (meetsMinimum(header ?? "0", config.minCliVersion)) return;
  fail(
    409,
    "cli_too_old",
    header === null
      ? `This riabuild did not say which version it is; the team requires ${config.minCliVersion} or newer.`
      : `This riabuild is ${header}; the team requires ${config.minCliVersion} or newer.`,
    "Run `brew upgrade clubria/tap/riabuild`.",
  );
}

export async function requireOrgMembership(login: string): Promise<void> {
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

/**
 * The version floor on its own, for `POST /api/v1/cli/device` — the one
 * enforcing route that runs before any session exists, and the only place the
 * floor reaches a machine that has never signed in.
 */
export async function versionGate(
  ctx: ActionCtx,
  req: Request,
): Promise<OrgConfig> {
  const config = await loadConfig(ctx);
  enforceMinVersion(req, config);
  return config;
}

/** Config, version floor, session, org membership — in that order, once. */
export async function guard(
  ctx: ActionCtx,
  req: Request,
  options: GuardOptions,
): Promise<Guarded> {
  const config = await loadConfig(ctx);
  if (options.version) enforceMinVersion(req, config);
  const { member, sessionId } = await authenticate(ctx, req);
  if (options.org) await requireOrgMembership(member.githubLogin);
  return { config, member, sessionId };
}

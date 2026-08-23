import { v } from "convex/values";
import { action, internalQuery } from "./_generated/server";
import { internal } from "./_generated/api";
import { getAuthUserId } from "@convex-dev/auth/server";
import { fetchUpstream, UPSTREAM_TIMEOUT_MS } from "./lib/http";

/**
 * GitHub org membership is the trust boundary. Convex only decides *how much*
 * access a member gets; GitHub decides whether they get any at all.
 *
 * Membership is checked with a server-held org token rather than the developer's
 * own OAuth token, because it has to be checkable at secret-brokering time —
 * months after sign-in, from an HTTP action that has no user session in hand.
 */

export const orgLogin = (): string =>
  process.env.RIABUILD_GITHUB_ORG ?? "Clubria";

export type MembershipResult =
  | { status: "member" }
  | { status: "not_member" }
  /** We could not tell. Fail closed, but say so honestly — see below. */
  | { status: "unavailable"; detail: string };

/**
 * `unavailable` exists so a missing token or a GitHub outage never renders as
 * "you are not a member of the org". Telling a developer they were removed from
 * the org when the truth is our credential expired sends them to the wrong
 * person for help.
 */
export async function checkOrgMembership(
  login: string,
  /**
   * Overridden only by the test that proves a hung GitHub becomes
   * `unavailable` rather than a request nobody ever answers. Every caller
   * takes the default.
   */
  timeoutMs: number = UPSTREAM_TIMEOUT_MS,
): Promise<MembershipResult> {
  const token = process.env.GITHUB_ORG_TOKEN;
  if (!token) {
    return {
      status: "unavailable",
      detail: "GITHUB_ORG_TOKEN is not set on the riabuild deployment",
    };
  }

  const org = orgLogin();
  let response: Response;
  try {
    response = await fetchUpstream(
      `https://api.github.com/orgs/${encodeURIComponent(org)}/members/${encodeURIComponent(login)}`,
      {
        headers: {
          authorization: `Bearer ${token}`,
          accept: "application/vnd.github+json",
          "user-agent": "riabuild-web",
          "x-github-api-version": "2022-11-28",
        },
        redirect: "manual",
      },
      timeoutMs,
    );
  } catch (error) {
    return {
      status: "unavailable",
      detail: `could not reach api.github.com: ${String(error)}`,
    };
  }

  // 204 member, 404 not a member, 302 the *requester* is not an org member.
  if (response.status === 204) return { status: "member" };
  if (response.status === 404) return { status: "not_member" };
  if (response.status === 302) {
    return {
      status: "unavailable",
      detail: `GITHUB_ORG_TOKEN does not belong to a member of ${org}`,
    };
  }
  return {
    status: "unavailable",
    detail: `GitHub returned ${response.status} checking membership of ${org}`,
  };
}

/**
 * Everyone in the org, so a lead can invite one of them rather than type a name.
 *
 * The typo is the reason this exists. A hand-typed login that is wrong produces
 * an invited row nobody will ever adopt: it sits in the member list looking like
 * a provisioned developer, holding an SSH key grant, while the person it was
 * meant for signs in beside it as a fresh candidate with nothing. A list the org
 * itself produced cannot make that row.
 *
 * Bounded at five pages. This is a company, not a directory, and an unbounded
 * loop against a paginated API is a way for one slow call to hold a Convex
 * action open until it is killed.
 */
const MEMBER_PAGES = 5;
const PER_PAGE = 100;

export type OrgCandidate = { login: string; githubId: string };

/** What a deployment with no real org to ask about offers instead. */
const DEV_CANDIDATES: OrgCandidate[] = [
  { login: "devuser", githubId: "dev-1" },
  { login: "dana", githubId: "dev-2" },
  { login: "sam", githubId: "dev-3" },
  { login: "priya", githubId: "dev-4" },
  { login: "rowan", githubId: "dev-5" },
];

export const listOrgMembers = action({
  args: {},
  returns: v.array(v.object({ login: v.string(), githubId: v.string() })),
  handler: async (ctx): Promise<OrgCandidate[]> => {
    const isLead: boolean = await ctx.runQuery(
      internal.github.viewerIsLead,
      {},
    );
    if (!isLead) throw new Error("Only team leads can do that.");

    // Same deployment-level gate as the dev sign-in provider, for the same
    // reason: without it the invite form is unreachable locally and in
    // Playwright, so the one flow this feature adds would be the one nobody
    // could ever look at. Production sets neither variable.
    if (process.env.RIABUILD_DEV_AUTH === "1") return DEV_CANDIDATES;

    const token = process.env.GITHUB_ORG_TOKEN;
    if (!token) {
      throw new Error(
        "GITHUB_ORG_TOKEN is not set on the riabuild deployment, so the org's members cannot be listed.",
      );
    }

    const org = orgLogin();
    const found: OrgCandidate[] = [];
    for (let page = 1; page <= MEMBER_PAGES; page += 1) {
      const response = await fetchUpstream(
        `https://api.github.com/orgs/${encodeURIComponent(org)}/members?per_page=${PER_PAGE}&page=${page}`,
        {
          headers: {
            authorization: `Bearer ${token}`,
            accept: "application/vnd.github+json",
            "user-agent": "riabuild-web",
            "x-github-api-version": "2022-11-28",
          },
        },
      );
      if (!response.ok) {
        throw new Error(
          `GitHub returned ${response.status} listing the members of ${org}.`,
        );
      }
      const body = (await response.json()) as unknown;
      if (!Array.isArray(body)) {
        throw new Error(`GitHub returned something unexpected for ${org}.`);
      }
      for (const entry of body) {
        const login = (entry as { login?: unknown }).login;
        const id = (entry as { id?: unknown }).id;
        if (
          typeof login === "string" &&
          (typeof id === "number" || typeof id === "string")
        ) {
          found.push({ login, githubId: String(id) });
        }
      }
      if (body.length < PER_PAGE) break;
    }

    return found.sort((a, b) =>
      a.login.toLowerCase().localeCompare(b.login.toLowerCase()),
    );
  },
});

export const viewerIsLead = internalQuery({
  args: {},
  returns: v.boolean(),
  handler: async (ctx) => {
    const userId = await getAuthUserId(ctx);
    if (userId === null) return false;
    const member = await ctx.db
      .query("members")
      .withIndex("by_userId", (q) => q.eq("userId", userId))
      .unique();
    return member?.role === "lead" && member.status === "active";
  },
});

export const viewerGithubLogin = internalQuery({
  args: {},
  returns: v.union(v.string(), v.null()),
  handler: async (ctx) => {
    const userId = await getAuthUserId(ctx);
    if (userId === null) return null;
    const member = await ctx.db
      .query("members")
      .withIndex("by_userId", (q) => q.eq("userId", userId))
      .unique();
    return member?.githubLogin ?? null;
  },
});

/**
 * The dashboard gate. Called on load rather than cached on the member row, so
 * losing org membership takes effect on the next page view instead of at the
 * next sign-in.
 */
export const viewerOrgMembership = action({
  args: {},
  returns: v.object({
    org: v.string(),
    status: v.union(
      v.literal("member"),
      v.literal("not_member"),
      v.literal("unavailable"),
      v.literal("signed_out"),
    ),
    detail: v.optional(v.string()),
  }),
  handler: async (ctx) => {
    const login: string | null = await ctx.runQuery(
      internal.github.viewerGithubLogin,
      {},
    );
    if (login === null)
      return { org: orgLogin(), status: "signed_out" as const };

    // A dev deployment has no real GitHub org to ask about, and without this
    // every local page renders the "check unavailable" banner — the happy path
    // would be the one state nobody could ever look at. Same deployment-level
    // gate as the dev sign-in provider; production never sets it.
    if (process.env.RIABUILD_DEV_AUTH === "1") {
      return { org: orgLogin(), status: "member" as const };
    }

    const result = await checkOrgMembership(login);
    return {
      org: orgLogin(),
      status: result.status,
      detail: result.status === "unavailable" ? result.detail : undefined,
    };
  },
});

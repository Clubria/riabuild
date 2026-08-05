import { v } from "convex/values";
import { action, internalQuery } from "./_generated/server";
import { internal } from "./_generated/api";
import { getAuthUserId } from "@convex-dev/auth/server";

/**
 * GitHub org membership is the trust boundary. Convex only decides *how much*
 * access a member gets; GitHub decides whether they get any at all.
 *
 * Membership is checked with a server-held org token rather than the developer's
 * own OAuth token, because it has to be checkable at secret-brokering time —
 * months after sign-in, from an HTTP action that has no user session in hand.
 */

export const orgLogin = (): string => process.env.RIABUILD_GITHUB_ORG ?? "Clubria";

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
    response = await fetch(
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
    if (login === null) return { org: orgLogin(), status: "signed_out" as const };

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

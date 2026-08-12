import { v } from "convex/values";
import { action } from "./_generated/server";
import { internal } from "./_generated/api";

/**
 * The release pipeline's entry point into riabuild-web.
 *
 * Separate from `org.ts` on purpose, following `github.ts`: the module that
 * *calls* internal functions imports the generated api, and the module that
 * *defines* them does not. Having org.ts do both makes its own types
 * self-referential, and TypeScript quietly degrades the whole api to `unknown`
 * — which surfaces as a type error somewhere else entirely.
 */

/** Where riabuild releases are published. */
export const RELEASE_REPO = "Clubria/riabuild";

/**
 * Announces a newly published CLI build so developers are offered it.
 *
 * Cutting a release and telling anyone about it used to be separate acts, and
 * only the first was automated. A release nobody is offered is invisible: the
 * CLI learns what to upgrade to from `/api/v1/org/config`, never from GitHub,
 * so a forgotten field left every machine on the old build with nothing
 * anywhere reporting a problem.
 *
 * Public, but not trusting. It accepts a version only after confirming GitHub
 * really has a published release tagged `v<version>`, and
 * `org.setLatestCliVersion` refuses to move backwards. Between them, the only
 * value anyone can put here is the newest genuinely published riabuild —
 * exactly the state this field is meant to be in — so exposing it costs
 * nothing.
 *
 * A shared secret would have been the obvious design and is the wrong one: a
 * Convex deploy key cannot write environment variables, so the credential
 * could only be installed by hand, and an automation that needs a manual step
 * before it runs is the problem this exists to remove.
 */
export const publishCliVersion = action({
  args: { version: v.string() },
  returns: v.object({
    updated: v.boolean(),
    latestCliVersion: v.string(),
  }),
  // The return type is annotated rather than inferred. Inference would run
  // through `internal`, which includes this module, and TypeScript reports the
  // cycle as "implicitly has type 'any'" on the export itself.
  handler: async (
    ctx,
    args,
  ): Promise<{ updated: boolean; latestCliVersion: string }> => {
    const version = args.version.trim();
    if (!/^\d+(\.\d+)*$/.test(version)) {
      throw new Error(
        `version must be dotted-numeric like 2026.08.04 — got "${version}".`,
      );
    }

    const url = `https://api.github.com/repos/${RELEASE_REPO}/releases/tags/v${version}`;

    // Authenticated for the rate limit, not for access: the repository is
    // public and this endpoint needs no scope at all. Unauthenticated calls are
    // limited to sixty an hour **per source IP**, and that IP is Convex's,
    // shared with every other deployment on the same infrastructure — a budget
    // riabuild neither controls nor can see. When it ran out during the
    // 2026.08.12.1 release, every attempt returned 403 for over an hour: the
    // binaries, the formula and both Linux repositories were published, and
    // nobody was offered any of it, because the CLI learns what to upgrade to
    // from `/api/v1/org/config` and nothing had written the version there.
    //
    // The same token org membership is checked with, which is already on every
    // deployment that works at all — `checkOrgMembership` returns `unavailable`
    // without it and secret brokering returns 503. Sending it moves this
    // request onto a five-thousand-an-hour budget of our own.
    const token = process.env.GITHUB_ORG_TOKEN;
    const headers: Record<string, string> = {
      Accept: "application/vnd.github+json",
      "User-Agent": "riabuild",
    };
    if (token) headers.authorization = `Bearer ${token}`;

    let response: Response;
    try {
      response = await fetch(url, { headers });
    } catch (error) {
      // Fail closed. An unreachable GitHub is not evidence a release exists.
      throw new Error(`could not reach api.github.com: ${String(error)}`);
    }

    if (response.status === 404) {
      throw new Error(
        `No published release is tagged v${version} in ${RELEASE_REPO}. ` +
          `Cut the release before announcing it.`,
      );
    }
    if (!response.ok) {
      // 403 is GitHub's answer both to "you have spent your budget" and to
      // "this token may not look at that", and the two want opposite responses:
      // wait, or go and repair a credential. A message naming only the status
      // sends whoever reads it to the wrong one — it already did, for an hour.
      if (response.status === 403 || response.status === 429) {
        const remaining = response.headers.get("x-ratelimit-remaining");
        const body = await response.text().catch(() => "");
        if (remaining === "0" || /rate limit/i.test(body)) {
          const reset = Number(response.headers.get("x-ratelimit-reset"));
          const when = Number.isFinite(reset) && reset > 0
            ? ` It resets at ${new Date(reset * 1000).toISOString()}.`
            : "";
          const unauthenticated = token
            ? ""
            : " GITHUB_ORG_TOKEN is not set on this deployment, so the check" +
              " shared the sixty-an-hour budget for its IP address with every" +
              " other deployment on it.";
          throw new Error(
            `api.github.com rate limit reached checking the v${version} release.` +
              `${when}${unauthenticated}`,
          );
        }
      }
      throw new Error(
        `api.github.com returned ${response.status} for the v${version} release.`,
      );
    }

    const release = (await response.json()) as { draft?: boolean };
    if (release.draft === true) {
      throw new Error(
        `The v${version} release is still a draft, so nobody could download it.`,
      );
    }

    return await ctx.runMutation(internal.org.setLatestCliVersion, { version });
  },
});

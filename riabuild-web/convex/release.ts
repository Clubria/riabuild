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
    let response: Response;
    try {
      response = await fetch(url, {
        headers: {
          Accept: "application/vnd.github+json",
          "User-Agent": "riabuild",
          "X-GitHub-Api-Version": "2022-11-28",
          // The org token `github.ts` already checks membership with, so this
          // costs no new secret and no manual step — the objection that ruled
          // out a shared secret above.
          //
          // It is not about permission: `RELEASE_REPO` is public and this read
          // works signed out. It is about the *rate limit*. Unauthenticated
          // api.github.com allows 60 requests an hour **per IP**, and a Convex
          // deployment's egress addresses are shared with everyone else on
          // it — so this call can be refused for traffic riabuild never made.
          // On 2026-08-12 it was: v2026.08.12.1 announced itself into three
          // 403s ten seconds apart, `latestCliVersion` stayed on the release
          // before it, and every laptop and every server riabuild set up went
          // on running a build whose bugs were already fixed. A token raises
          // the limit to 5000 an hour and is counted against riabuild alone.
          //
          // Absent, this sends no header and still works — signed out is the
          // old behaviour, not a broken one, and a deployment without the
          // token should not lose the ability to announce a release over it.
          ...(process.env.GITHUB_ORG_TOKEN
            ? { Authorization: `Bearer ${process.env.GITHUB_ORG_TOKEN}` }
            : {}),
        },
      });
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
      // Named, because the remedy is nothing like the one for a refusal: a
      // rate limit is waited out or authenticated past, and the failure that
      // started this said only "returned 403", which reads as a permission
      // problem nobody had.
      const rateLimited = response.headers.get("x-ratelimit-remaining") === "0";
      throw new Error(
        `api.github.com returned ${response.status} for the v${version} release.` +
          (rateLimited
            ? ` The rate limit for ${
                process.env.GITHUB_ORG_TOKEN
                  ? "GITHUB_ORG_TOKEN"
                  : "unauthenticated requests from this deployment"
              } is exhausted; it resets hourly.`
            : ""),
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

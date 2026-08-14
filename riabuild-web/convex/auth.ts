import GitHub from "@auth/core/providers/github";
import { convexAuth } from "@convex-dev/auth/server";
import { Anonymous } from "@convex-dev/auth/providers/Anonymous";
import { MutationCtx } from "./_generated/server";
import { Id } from "./_generated/dataModel";
import { claimOrCreateMember } from "./members";

/**
 * GitHub only. There is no password provider and there never should be — the
 * whole authorization model assumes the identity is a GitHub account whose org
 * membership can be re-checked later.
 *
 * `read:org` is required: without it the token cannot answer membership
 * questions and every secret-brokering request fails closed.
 */
const GitHubProvider = GitHub({
  authorization: {
    params: { scope: "read:user user:email read:org" },
  },
  async profile(githubProfile, tokens) {
    const login = String(githubProfile.login);
    const email = await resolveEmail(githubProfile.email, tokens.access_token);
    return {
      id: String(githubProfile.id),
      name: githubProfile.name ?? login,
      email,
      image: githubProfile.avatar_url,
      // Consumed by `createOrUpdateUser` below; never written to `users`.
      githubLogin: login,
      githubId: String(githubProfile.id),
      // The OAuth access token is deliberately not persisted. Membership is
      // re-checked with a server-held org token instead.
    } as unknown as { id: string };
  },
});

/**
 * GitHub omits `email` from the profile when the developer keeps it private.
 * The profile screen is prefilled from the verified email list instead, which is
 * what `user:email` scope is for.
 */
async function resolveEmail(
  profileEmail: string | null | undefined,
  accessToken: string | undefined,
): Promise<string | undefined> {
  if (profileEmail) return profileEmail;
  if (!accessToken) return undefined;
  try {
    const response = await fetch("https://api.github.com/user/emails", {
      headers: {
        authorization: `Bearer ${accessToken}`,
        accept: "application/vnd.github+json",
        "user-agent": "riabuild-web",
      },
    });
    if (!response.ok) return undefined;
    const emails = (await response.json()) as unknown;
    if (!Array.isArray(emails)) return undefined;
    const verified = emails.filter(
      (entry): entry is { email: string; primary: boolean; verified: boolean } =>
        typeof entry === "object" &&
        entry !== null &&
        typeof (entry as { email?: unknown }).email === "string" &&
        (entry as { verified?: unknown }).verified === true,
    );
    return (verified.find((entry) => entry.primary) ?? verified[0])?.email;
  } catch {
    return undefined;
  }
}

/**
 * A door that only exists on a deployment that has opted in.
 *
 * Playwright cannot complete a GitHub OAuth round trip, so without this there is
 * no way to walk the signed-in pages against a real backend at all. It is safe
 * because it is not conditionally *enabled* — it is conditionally *registered*.
 * On a deployment without `RIABUILD_DEV_AUTH=1` the provider is not in the
 * providers array, so `signIn("dev")` has nothing to dispatch to and fails the
 * same way a misspelt provider name would.
 *
 * The role still comes from `RIABUILD_BOOTSTRAP_LEADS`, exactly as it does for a
 * real GitHub sign-in. This adds a way to authenticate; it adds no way to
 * authorize, and it is `members.role` that gates everything that matters.
 */
const DevProvider = Anonymous({
  id: "dev",
  profile(params) {
    const raw = params.login;
    const login = (typeof raw === "string" && raw !== "" ? raw : "devuser").slice(
      0,
      39,
    );
    return {
      name: login,
      email: `${login}@example.invalid`,
      githubLogin: login,
      githubId: `dev-${login}`,
      isAnonymous: true,
    } as unknown as { name: string; isAnonymous: true };
  },
});

function providers() {
  if (process.env.RIABUILD_DEV_AUTH === "1") {
    console.warn(
      "riabuild: RIABUILD_DEV_AUTH=1 — the dev sign-in provider is registered. Never set this in production.",
    );
    return [GitHubProvider, DevProvider];
  }
  return [GitHubProvider];
}

function bootstrapLeads(): string[] {
  return (process.env.RIABUILD_BOOTSTRAP_LEADS ?? "")
    .split(/[\s,]+/)
    .map((login) => login.trim().toLowerCase())
    .filter((login) => login.length > 0);
}

function readString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

export const { auth, signIn, signOut, store, isAuthenticated } = convexAuth({
  providers: providers(),
  callbacks: {
    /**
     * We own user creation rather than using `afterUserCreatedOrUpdated`
     * because the GitHub login has to reach the `members` row without being
     * spread into `users`, whose schema comes from `authTables`.
     */
    async createOrUpdateUser(ctx: MutationCtx, args) {
      const profile = args.profile;
      const githubLogin = readString(profile.githubLogin) ?? "";
      const githubId = readString(profile.githubId) ?? "";
      const name = readString(profile.name) ?? githubLogin;
      const image = readString(profile.image);
      const email = readString(profile.email) ?? "";

      const userFields = { name, email: email || undefined, image };

      let userId: Id<"users">;
      if (args.existingUserId !== null) {
        userId = args.existingUserId;
        await ctx.db.patch("users", userId, userFields);
      } else {
        userId = await ctx.db.insert("users", userFields);
      }

      await claimOrCreateMember(ctx, {
        userId,
        githubLogin,
        githubId,
        name,
        email,
        isBootstrapLead: bootstrapLeads().includes(githubLogin.toLowerCase()),
      });
      return userId;
    },
  },
});

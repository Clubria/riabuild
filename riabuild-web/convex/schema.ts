import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";
import { authTables } from "@convex-dev/auth/server";

export const roleValidator = v.union(
  v.literal("candidate"),
  v.literal("developer"),
  v.literal("lead"),
);

export const statusValidator = v.union(
  v.literal("active"),
  v.literal("suspended"),
);

/**
 * Identity lives in GitHub; only authorization lives here.
 *
 * Every token-shaped value in this schema is stored as a SHA-256 hex digest.
 * A dump of this database must not hand out live sessions.
 */
export default defineSchema({
  ...authTables,

  members: defineTable({
    /**
     * Absent means **invited and not yet arrived**: a lead picked this person
     * out of the GitHub org and recorded their role, and possibly issued them a
     * key, before they ever signed in. `auth.ts:upsertMember` adopts the row on
     * their first sign-in rather than inserting a second one.
     *
     * This is what keeps an invited row inert. `viewerMember` looks up
     * `by_userId` with a real user id, which `undefined` never matches, so
     * `requireLead` and everything downstream of it are unreachable; and every
     * `/api/v1` route needs a `cliSessions` row, which needs a sign-in. An
     * invited `lead` is therefore a decision recorded in advance, not access
     * granted in advance.
     *
     * Design: `docs/superpowers/specs/2026-08-14-inviting-members-design.md`.
     */
    userId: v.optional(v.id("users")),
    githubLogin: v.string(),
    /**
     * GitHub's numeric id. Carried by an invited row from the moment it is
     * created — the org listing API returns it beside the login — because it is
     * what adoption matches on: a developer can rename their GitHub account
     * between the invitation and their first sign-in, and this cannot change.
     */
    githubId: v.string(),
    /**
     * Immutable, ours, and independent of GitHub. Names a developer's
     * directory on a shared server, so it must outlive a GitHub rename.
     * Required — `members.backfillMemberIds` fills existing rows before this
     * field is required in production. See `docs/deploying.md` §7 for the
     * deploy order this depends on.
     *
     * Not the same thing as `cliSessions.memberId` below: that one is a
     * document reference (`v.id("members")`); this one is a UUID string
     * stored on the row itself. Same name, unrelated types — do not unify.
     */
    memberId: v.string(),
    firstName: v.string(),
    lastName: v.string(),
    email: v.string(),
    role: roleValidator,
    status: statusValidator,
  })
    .index("by_userId", ["userId"])
    .index("by_githubLogin", ["githubLogin"])
    .index("by_githubId", ["githubId"]),

  /** Live CLI sessions. `tokenHash` is the lookup key — the raw token is never stored. */
  cliSessions: defineTable({
    /** A document reference — not the UUID `members.memberId` above. Same name, different type. */
    memberId: v.id("members"),
    tokenHash: v.string(),
    deviceLabel: v.string(),
    cliVersion: v.string(),
    lastUsedAt: v.number(),
    expiresAt: v.number(),
    revokedAt: v.optional(v.number()),
    /**
     * How this session came into existence: approved by a human in a browser
     * (`device`), or minted by another session through
     * `POST /api/v1/cli/sessions` (`delegated`) — which is how a laptop signs
     * a server in without sending the developer to riabuild.clubria.com/cli a
     * second time.
     *
     * Optional, and **absent means `device`**. Every row written before
     * delegation existed was a browser approval, so the absent case is not an
     * unknown to be treated carefully — it is the answer. Making it required
     * would mean backfilling every live session to record something already
     * true of all of them.
     *
     * It is read for exactly one decision: only a `device` session may
     * delegate. A server's token is readable by every co-tenant sharing that
     * Unix account, and a token that can mint tokens turns one leaked
     * credential into an unlimited supply of them — including ones minted
     * after `riabuild remote forget` revoked the original, which is the
     * guarantee the whole on-disk-token amendment rests on.
     */
    origin: v.optional(v.union(v.literal("device"), v.literal("delegated"))),
    /**
     * The session that minted this one. Never used to authorise anything —
     * `origin` alone decides that — but it is what makes a delegation
     * readable after the fact: which laptop signed this server in.
     */
    delegatedFrom: v.optional(v.id("cliSessions")),
  })
    .index("by_tokenHash", ["tokenHash"])
    .index("by_memberId", ["memberId"])
    /**
     * For the sweep that reaps expired sessions, the same way
     * `cliDeviceCodes.by_expiresAt` serves abandoned logins. Without it the
     * reaper is a full table scan of every session ever minted, which grows
     * without bound while the rows worth deleting are a prefix of this index.
     */
    .index("by_expiresAt", ["expiresAt"]),

  /**
   * Pending device-authorisation requests: one row per `riabuild login`, minted
   * by POST /api/v1/cli/device and redeemed once by POST /api/v1/cli/token.
   * Separate from `cliSessions` on purpose — an abandoned login must never look
   * like a live session.
   *
   * A row is created *before* anyone is known, which is the inversion that
   * matters when reading this table: `memberId`, `approvedAt` and `deniedAt`
   * stay empty until a human acts on the request, and most rows never fill them
   * in. `by_expiresAt` exists for the hourly sweep in `crons.ts` that keeps
   * abandoned logins from accumulating forever.
   */
  cliDeviceCodes: defineTable({
    /** SHA-256 of the secret the CLI polls with. The raw value is never stored. */
    deviceCodeHash: v.string(),
    /**
     * The short code the developer reads off their terminal, normalised to
     * uppercase without its dash. Plaintext on purpose: it identifies a request
     * but cannot be exchanged for anything, so hashing it would only stop the
     * dashboard from looking it up.
     */
    userCode: v.string(),
    deviceLabel: v.string(),
    cliVersion: v.string(),
    expiresAt: v.number(),
    memberId: v.optional(v.id("members")),
    approvedAt: v.optional(v.number()),
    deniedAt: v.optional(v.number()),
    consumedAt: v.optional(v.number()),
  })
    .index("by_deviceCodeHash", ["deviceCodeHash"])
    .index("by_userCode", ["userCode"])
    .index("by_expiresAt", ["expiresAt"]),

  /** Single row. Edited by leads in the dashboard, read by every CLI launch. */
  orgConfig: defineTable({
    /**
     * Org Claude Code settings, stored and served as verbatim JSON text.
     *
     * A `v.string()` is all a table validator can say about a blob whose
     * structure lives inside the string, and it is worth being explicit that
     * this is **not** the gate. The CLI is: `tasks::org_settings::vetting`
     * refuses any key that names a program — `hooks`, `apiKeyHelper`,
     * `awsCredentialExport`, `mcpServers` and the rest — and accepts
     * `statusLine.command` only when it is the exact command the
     * `claude_statusline` task installs. Whatever this column holds, that is
     * what reaches `claude --settings`.
     *
     * Keeping the real check there rather than here is deliberate. This row is
     * data the CLI treats as untrusted: a hand-edited document, a compromised
     * deployment, or anything between the two and a laptop would all get past a
     * validator that only runs on the way in. See "the server ships data, never
     * logic" in the root `CLAUDE.md`.
     */
    claudeSettings: v.string(),
    claudeSettingsUpdatedAt: v.number(),
    repoSlug: v.string(),
    /**
     * Retired — the CLI now picks the checkout location per platform. Optional
     * rather than deleted so the row written before this change still validates;
     * the next `replace` drops it. See RETIRED_DEFAULT_PROJECT_PATH in org.ts.
     */
    defaultProjectPath: v.optional(v.string()),
    minCliVersion: v.string(),
    latestCliVersion: v.string(),
    /** Bumped when secrets rotate; the CLI treats an older .env.<environment> as stale. */
    secretsUpdatedAt: v.number(),
    /**
     * The one ngrok authtoken the whole team tunnels with, set by a lead.
     *
     * Stored in plaintext, like an issued SSH key and for the same reason: the
     * CLI needs the value itself, so encryption with a key held in this same
     * deployment would move the problem rather than solve it. It is bounded the
     * same way instead — no route returns it to a browser, every fetch is
     * audited, and it never lands on a developer's filesystem. See
     * `docs/superpowers/specs/2026-08-18-ngrok-design.md`.
     *
     * Optional because the row written before this field existed must still
     * validate, and because a team that has not set one is an ordinary state
     * rather than a broken deployment.
     */
    ngrokAuthToken: v.optional(v.string()),
    /** When a lead last set it. Zero, or absent, means no token is set. */
    ngrokAuthTokenUpdatedAt: v.optional(v.number()),
  }),

  /**
   * The addresses of the team's servers, typed once by a lead and read by
   * every developer's CLI through `GET /api/v1/remotes/shared`.
   *
   * Deliberately holds no secret, and is the one table here that could not
   * hold one usefully: a shared server's SSH key pair, its saved password and
   * the riabuild session minted for it all belong to the single laptop that
   * made them, and a session minted for one laptop is not shareable. What is
   * shared is an address, which is inert.
   *
   * `name` is stored bare. The CLI shows it as `shared-<name>` so it cannot
   * collide with a server a developer added themselves, and that prefix is
   * never written down at either end — it exists between the two lists, which
   * is where the collision it prevents happens.
   *
   * Design: `docs/superpowers/specs/2026-08-12-shared-servers-design.md`.
   */
  sharedServers: defineTable({
    name: v.string(),
    host: v.string(),
    port: v.number(),
    user: v.string(),
    /**
     * What this server is for, in one line a lead types — "the 4×A100 box",
     * "staging, do not run migrations here". Every developer reads it under the
     * server's name in `riabuild remote`'s picker, which is the whole reason it
     * exists: a list of hostnames is not a list a new developer can choose from.
     *
     * Optional, because every row that existed before this field has none and
     * inventing one for them would put a lead's words in riabuild's mouth. It
     * is also the first field on this table that is *prose*, so it is the first
     * that reaches a terminal as something other than an address — the CLI puts
     * it through `riabuild_ui::one_line` before printing, and that is where the
     * rule lives rather than here.
     */
    description: v.optional(v.string()),
    createdBy: v.id("members"),
    createdAt: v.number(),
    updatedAt: v.number(),
  }).index("by_name", ["name"]),

  /**
   * Which Infisical folder a repository's secrets come from — one row per
   * repository, typed by a lead.
   *
   * Holds no secret and cannot hold one: a path names where secrets live, and
   * every value at that path is fetched by the CLI with a credential brokered
   * for the one command. What a dump of this table gives away is the shape of
   * the team's Infisical project, which its own members can already see.
   *
   * **A repository with no row gets no environment files at all.** That is the
   * meaning of "unset" and it is a decision rather than an omission — a
   * repository that is supposed to have no environment variables had no way to
   * say so, and `env_local` failed its run on every one of them. So absence is
   * the answer here, and nothing falls back to `INFISICAL_SECRET_PATH`; that
   * variable is read only by `secretPaths.seedFromDeploymentPath` and by the
   * legacy no-`repo` path in `infisical.brokerToken`, both of which exist for
   * CLIs and deployments released before this table.
   *
   * `repoSlug` is stored as a lead typed it, checked for *shape* only —
   * `owner/name`, the rules `api::Repo::parse` applies. It is deliberately not
   * checked against GitHub: which repositories exist is a question the CLI asks
   * through the developer's own `gh`, so that riabuild holds no permission
   * logic that could be wrong about it (see "the server ships data, never
   * logic" in `../../AGENTS.md`). A row naming a repository nobody has is
   * inert, because no run is ever about it.
   *
   * `updatedAt` is not bookkeeping. `.env.dev` filled from `/apps/hub` is wrong
   * the moment this row says `/apps/payments`, and nothing on the laptop can
   * see that; `env_local::check()` compares this against the file's mtime the
   * same way it already compares `orgConfig.secretsUpdatedAt`. It is per row
   * rather than org-wide precisely so that editing one repository's path does
   * not restage every other repository's files.
   *
   * Which environments those folders have is **not** stored. It is read from
   * Infisical on demand, because it is a fact about the team's project rather
   * than a thing anybody types here, and a copy kept in this row would be wrong
   * from the first folder somebody adds.
   *
   * Design: `docs/superpowers/specs/2026-09-04-per-repository-secret-paths-design.md`.
   */
  repoSecretPaths: defineTable({
    /** `owner/name`, exactly as the CLI's `Repo::slug()` spells it. */
    repoSlug: v.string(),
    /**
     * The absolute Infisical folders this repository's secrets come from, in
     * the order they are exported and therefore merged: **later wins**, exactly
     * as a dotenv loader reads the finished file.
     *
     * A list rather than one string for the reason `secretPaths()` already
     * gives about `INFISICAL_SECRET_PATH`: one environment's secrets are not
     * always in one folder, and a `.env.dev` carrying either half alone does
     * not start the app. Never empty — a repository with nothing to pull has no
     * row at all, which is a different statement and the one that means "write
     * no env files".
     */
    secretPaths: v.array(v.string()),
    updatedBy: v.id("members"),
    createdAt: v.number(),
    updatedAt: v.number(),
  }).index("by_repo", ["repoSlug"]),

  /**
   * Which environments a set of folders was last found in — a cache, and
   * nothing here is authoritative about anything.
   *
   * `env_local::check()` asks for a repository's scope on **every** run,
   * including every `riabuild --check`, and answering from Infisical costs a
   * universal-auth login, a project fetch and one folder listing per
   * environment per folder. That is a fine price to pay when the answer has
   * changed and an absurd one to pay when a team of ten provisions the same
   * morning.
   *
   * The row is keyed by the question, not by the repository — the role and the
   * exact ordered folder list — so a lead editing a path invalidates it by
   * asking a different question rather than by remembering to clear anything,
   * and two repositories that happen to name the same folders share one entry.
   * Stale rows are simply ignored past their age and overwritten in place;
   * there is no reaper, because the number of distinct questions a team asks is
   * bounded by its repositories.
   */
  infisicalEnvCache: defineTable({
    key: v.string(),
    environments: v.array(v.string()),
    fetchedAt: v.number(),
  }).index("by_key", ["key"]),

  /**
   * SSH keys the org issues: a private key a lead pastes once, and the members
   * it is issued to.
   *
   * This is the one table here that holds a long-lived secret in plaintext, and
   * `../../CLAUDE.md` names it as a deliberate third exception to "secrets are
   * brokered, never stored" rather than leaving the invariant and this row to
   * contradict each other quietly. Say the cost plainly: a dump of this
   * database hands out working SSH access to whatever these keys open. It is
   * here because the alternative is not a brokered key — it is that key
   * arriving over Slack and living in someone's `~/.ssh` forever.
   *
   * What bounds it is everywhere else. No route returns `privateKey` to a
   * browser. Every fetch is audited by label, so "who took a copy" has an
   * answer. The CLI holds it only in an `ssh-agent` riabuild owns and never on
   * a filesystem. And it *bootstraps* rather than replaces: it authenticates
   * one `ssh-copy-id`, after which the developer's own per-laptop key carries
   * the run and `remote forget` still has exactly one line to remove.
   *
   * `publicKey`, `fingerprint` and `keyType` are derived from `privateKey` by
   * `lib/opensshKey.ts` and never accepted from a client — an OpenSSH container
   * carries its own public half, so this costs one digest and no key
   * mathematics. They exist so a lead can identify a row without the row ever
   * handing the secret back, which is what makes "no reveal control" a usable
   * rule rather than an obstruction.
   *
   * `issuedTo` is an array on the row rather than a join table. Convex cannot
   * index array-contains, so "keys issued to me" is a bounded scan — the same
   * shape and the same 200-row bound `sharedServers` uses, for the same reason:
   * this list is tens of rows, typed by hand.
   *
   * Design: `docs/superpowers/specs/2026-08-13-issued-ssh-keys-design.md`.
   */
  issuedKeys: defineTable({
    label: v.string(),
    privateKey: v.string(),
    publicKey: v.string(),
    fingerprint: v.string(),
    keyType: v.string(),
    issuedTo: v.array(v.id("members")),
    createdBy: v.id("members"),
    createdAt: v.number(),
    updatedAt: v.number(),
  }).index("by_label", ["label"]),

  /**
   * One row per Claude Code session, per developer, per Claude account — the
   * cumulative totals the status line already prints, sent on by
   * `riabuild internal usage-flush` and upserted here.
   *
   * Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md`.
   *
   * Two things about the key are decisions rather than details. The member
   * comes from the authenticated session and never from the request body, so
   * there is no way to file a sample against somebody else. And the account is
   * `accountId` — riabuild's own config-directory uuid — rather than the Claude
   * login's email address: these are personal Pro and Max subscriptions, so
   * keying on the email would make this table a durable map of which private
   * Anthropic accounts each developer owns, acquired as a side effect of
   * picking a primary key.
   *
   * Every metric is optional because every one of them is genuinely absent
   * somewhere: an API-key or Console login gets no `rate_limits` block at all,
   * a session that has not called the API yet has no `cost`, and a harness that
   * is not Claude Code will arrive with a different subset again. Absent means
   * "never measured" and is not the same as zero, which is why nothing here
   * defaults to `0` on the way in.
   */
  usageSessions: defineTable({
    memberId: v.id("members"),
    /**
     * The uuid riabuild names a Claude config directory with — not an email,
     * and not the account *number* a developer sees in `riabuild claude`, which
     * renumbers when one is deleted.
     */
    accountId: v.string(),
    sessionId: v.string(),
    /** "claude" today. Grok publishes a status line of the same shape; Codex will arrive over a different producer. */
    harness: v.string(),
    model: v.optional(v.string()),
    /**
     * Unix **seconds**, stamped by the server when the sample arrived. Not the
     * laptop's clock: a machine with a wrong one would otherwise decide which
     * rows a lead's window contains and which rows the reaper deletes.
     */
    observedAt: v.number(),
    /**
     * What the session would have cost against the public API price sheet.
     * Nobody spent it — these are subscriptions — so it is labelled
     * "list-price equivalent" everywhere it is shown, and never "spend".
     */
    costUsd: v.optional(v.number()),
    /**
     * There is deliberately **no token count here**, and its absence is a
     * finding rather than an omission. The status line's
     * `context_window.total_input_tokens` and `total_output_tokens` are what is
     * *currently in the context window*, taken from the most recent API
     * response: zero before the first one, and smaller again after every
     * `/compact`. Merged by maximum — which is what every other number here
     * does — they would report the largest context this session ever held,
     * under a heading saying "tokens". `current_usage` describes one API call
     * and is no better. The payload carries no cumulative billed-token field at
     * all, so `costUsd` is the volume proxy, and a column nobody can populate
     * honestly is worse than no column.
     */
    durationMs: v.optional(v.number()),
    apiDurationMs: v.optional(v.number()),
    linesAdded: v.optional(v.number()),
    linesRemoved: v.optional(v.number()),
    /**
     * The rate-limit windows, which exist in no other surface: the Admin APIs
     * cannot see a personal subscription and OpenTelemetry does not emit these.
     * On a plan where nobody pays per token, consumed window is the only
     * measure of the thing that actually runs out.
     *
     * These are the fields that take the *newest* sample rather than the
     * largest, because a percentage legitimately falls when its window resets.
     */
    fiveHourPct: v.optional(v.number()),
    fiveHourResetsAt: v.optional(v.number()),
    sevenDayPct: v.optional(v.number()),
    sevenDayResetsAt: v.optional(v.number()),
  })
    /** The upsert key. One row per session, however many samples describe it. */
    .index("by_member_account_session", ["memberId", "accountId", "sessionId"])
    /** One member's window, newest first — what the lead rollup reads. */
    .index("by_member_observed", ["memberId", "observedAt"])
    /**
     * For the ninety-day sweep in `crons.ts`, the same way
     * `cliSessions.by_expiresAt` serves its reaper. Without it the sweep is a
     * `filter` that walks every row ever written on every run — and in the
     * steady state, where there is nothing to delete, it walks all of them and
     * finds none. The rows worth deleting are a prefix of this index.
     */
    .index("by_observed", ["observedAt"]),

  auditLog: defineTable({
    actorId: v.optional(v.id("members")),
    action: v.string(),
    subjectId: v.optional(v.id("members")),
    meta: v.record(v.string(), v.string()),
    at: v.number(),
  }).index("by_at", ["at"]),
});

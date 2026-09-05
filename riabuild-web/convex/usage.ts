/**
 * What Claude Code cost a developer, as their own status line already knew.
 *
 * The collector is `riabuild internal statusline`, which appends a line to a
 * spool and exits; `riabuild internal usage-flush` sends the spool on, at most
 * once a minute and only while somebody is working. Every Claude Code account
 * riabuild manages reports — it was opt-in per account until 2026-09-05, and
 * what that produced was this table staying empty. Nothing here reaches a
 * laptop, and nothing here decides what runs on one: this module only receives,
 * merges and reports.
 *
 * Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md`.
 */

import { v } from "convex/values";
import { internalMutation, query } from "./_generated/server";
import { Doc } from "./_generated/dataModel";
import { requireLead } from "./members";

/**
 * One session's cumulative totals, as the CLI sends them.
 *
 * Every field is what the harness reported, and **`memberId` is not among
 * them**. The flush authenticates as the member, so the server already knows
 * who this is from the bearer token; a member named in the body would be a
 * client-supplied claim standing in front of one the request had already
 * proved.
 *
 * Everything but the three key fields is optional, and optional here means
 * "this harness did not report it" rather than zero. An API-key login gets no
 * `rate_limits`, a session that has not called the API yet has no cost, and
 * Grok and Codex will each arrive with a different subset again.
 */
export const usageSample = v.object({
  harness: v.string(),
  accountId: v.string(),
  sessionId: v.string(),
  model: v.optional(v.string()),
  costUsd: v.optional(v.number()),
  durationMs: v.optional(v.number()),
  apiDurationMs: v.optional(v.number()),
  linesAdded: v.optional(v.number()),
  linesRemoved: v.optional(v.number()),
  fiveHourPct: v.optional(v.number()),
  fiveHourResetsAt: v.optional(v.number()),
  sevenDayPct: v.optional(v.number()),
  sevenDayResetsAt: v.optional(v.number()),
});

/**
 * How many samples one request may carry.
 *
 * The flush compacts its spool to one line per session before sending, so a
 * laptop that has been offline for a week sends its *session* count and not its
 * message count — two hundred is far past any real one. It is a bound on the
 * work a single mutation does, not a rate limit: each sample is an indexed read
 * and a write, and an unbounded array is an unbounded transaction.
 */
export const MAX_SAMPLES_PER_REQUEST = 200;

/** How long a row survives. Ninety days, swept hourly by `crons.ts`. */
export const RETENTION_DAYS = 90;

/**
 * The largest of the two, where either may be missing.
 *
 * **The maximum, never a sum.** `total_cost_usd` and the duration counters are
 * cumulative for a session and reset when `/clear` starts a new one, so the
 * newest sample is the whole truth about that session and adding two of them
 * together overstates by roughly the number of messages in it. Largest rather
 * than latest, so a sample that overtakes another in flight — three windows on
 * one laptop, three flushes racing — cannot walk a total backwards.
 *
 * Two absent values stay absent rather than becoming `0`: a zero is a
 * measurement that says the session cost nothing, and "nobody measured this"
 * is a different statement that the panel renders differently.
 */
function largest(
  existing: number | undefined,
  incoming: number | undefined,
): number | undefined {
  if (incoming === undefined) return existing;
  if (existing === undefined) return incoming;
  return Math.max(existing, incoming);
}

/**
 * Upserts one flush's worth of samples, keyed by `(memberId, accountId,
 * sessionId)`.
 *
 * Deliberately writes **no `auditLog` row**. That table is the record of
 * changes to access — a role promotion, a suspension, a revoked session, a
 * credential handed out — and it is read by a human scrolling a list. A row per
 * sample would bury all of that under a flush that fires every sixty seconds
 * per active developer, which is not an audit trail with extra detail in it but
 * an audit trail nobody can use. Usage is not an access event.
 */
export const record = internalMutation({
  args: {
    memberId: v.id("members"),
    /** Unix seconds, stamped by the endpoint. Never the laptop's clock. */
    observedAt: v.number(),
    samples: v.array(usageSample),
  },
  returns: v.object({ accepted: v.number() }),
  handler: async (ctx, args) => {
    for (const sample of args.samples) {
      const existing = await ctx.db
        .query("usageSessions")
        .withIndex("by_member_account_session", (q) =>
          q
            .eq("memberId", args.memberId)
            .eq("accountId", sample.accountId)
            .eq("sessionId", sample.sessionId),
        )
        // `unique` rather than `first`: this mutation is the only writer, and
        // Convex transactions are serializable, so a second row for one key
        // cannot exist. If one ever does, the loud failure is the useful one —
        // `first` would quietly merge into whichever row it happened to find
        // and split one session's totals across two.
        .unique();

      if (existing === null) {
        await ctx.db.insert("usageSessions", {
          memberId: args.memberId,
          accountId: sample.accountId,
          sessionId: sample.sessionId,
          harness: sample.harness,
          model: sample.model,
          observedAt: args.observedAt,
          costUsd: sample.costUsd,
          durationMs: sample.durationMs,
          apiDurationMs: sample.apiDurationMs,
          linesAdded: sample.linesAdded,
          linesRemoved: sample.linesRemoved,
          fiveHourPct: sample.fiveHourPct,
          fiveHourResetsAt: sample.fiveHourResetsAt,
          sevenDayPct: sample.sevenDayPct,
          sevenDayResetsAt: sample.sevenDayResetsAt,
        });
        continue;
      }

      await ctx.db.patch("usageSessions", existing._id, {
        // Newest wins for everything that describes *when* rather than *how
        // much*: this row was last heard of now.
        observedAt: args.observedAt,
        harness: sample.harness,
        // The newest non-null. A sample that does not name a model is not a
        // sample saying the model was forgotten.
        model: sample.model ?? existing.model,

        costUsd: largest(existing.costUsd, sample.costUsd),
        durationMs: largest(existing.durationMs, sample.durationMs),
        apiDurationMs: largest(existing.apiDurationMs, sample.apiDurationMs),
        linesAdded: largest(existing.linesAdded, sample.linesAdded),
        linesRemoved: largest(existing.linesRemoved, sample.linesRemoved),

        // The one group that takes the newest value rather than the largest,
        // and the exception is the point: a rate-limit percentage *falls* when
        // its window rolls over, so "the largest we ever saw" would report a
        // developer as permanently out of headroom from the one busy afternoon
        // they had. A percentage is a reading, not a total.
        fiveHourPct: sample.fiveHourPct ?? existing.fiveHourPct,
        fiveHourResetsAt: sample.fiveHourResetsAt ?? existing.fiveHourResetsAt,
        sevenDayPct: sample.sevenDayPct ?? existing.sevenDayPct,
        sevenDayResetsAt: sample.sevenDayResetsAt ?? existing.sevenDayResetsAt,
      });
    }

    return { accepted: args.samples.length };
  },
});

/** One member's line in the rollup. */
const usageRow = v.object({
  memberId: v.id("members"),
  githubLogin: v.string(),
  /** Sessions with at least one sample inside the window. */
  sessions: v.number(),
  /**
   * Summed across sessions — which is the one place summing is right, because
   * each session's own number is already the whole of that session.
   *
   * Labelled "list-price equivalent" wherever it is shown and never "spend":
   * these are personal Pro and Max subscriptions, so this is what the work
   * would have cost against the public API price sheet and not money anybody
   * paid. Left unlabelled it ends up in a budget.
   */
  costUsd: v.number(),
  linesAdded: v.number(),
  linesRemoved: v.number(),
  /** From this member's newest sample. `null` where the harness reported none. */
  fiveHourPct: v.union(v.number(), v.null()),
  fiveHourResetsAt: v.union(v.number(), v.null()),
  sevenDayPct: v.union(v.number(), v.null()),
  sevenDayResetsAt: v.union(v.number(), v.null()),
  /** Unix seconds. When riabuild last heard anything from this member. */
  lastObservedAt: v.number(),
  /**
   * This member had more sessions in the window than one read may return, so
   * the totals beside it are a floor rather than the answer. Said out loud
   * rather than silently truncated — a `take()` nobody reports is a number
   * that is quietly wrong.
   */
  truncated: v.boolean(),
});

/** The default window, and what the panel says it is showing. */
const DEFAULT_WINDOW_DAYS = 7;

/**
 * The bound on one member's read.
 *
 * Sessions, not samples — the table holds one row per session however many
 * renders described it, so a heavy week is tens of rows and this is two orders
 * of magnitude past that. Ordered newest-first so that if it ever *is* reached
 * the rows dropped are the oldest, which keeps the rate-limit reading (which
 * only the newest row can supply) correct even in the truncated case.
 */
const SESSIONS_PER_MEMBER = 500;

/** The bound on the member list, matching `members.list`. */
const MEMBER_LIMIT = 200;

export const rollup = query({
  args: { windowDays: v.optional(v.number()) },
  returns: v.object({
    windowDays: v.number(),
    /** Unix seconds — the start of the window these rows describe. */
    since: v.number(),
    rows: v.array(usageRow),
  }),
  handler: async (ctx, args) => {
    // Lead-only, and it is the whole gate: this is every developer's usage in
    // one table, which is not something a developer or a candidate gets to
    // read about their colleagues.
    await requireLead(ctx);

    const windowDays = clampWindow(args.windowDays ?? DEFAULT_WINDOW_DAYS);
    const nowSeconds = Math.floor(Date.now() / 1000);
    const since = nowSeconds - windowDays * 24 * 60 * 60;

    const members = await ctx.db.query("members").take(MEMBER_LIMIT);
    const rows = [];

    for (const member of members) {
      const sessions = await ctx.db
        .query("usageSessions")
        .withIndex("by_member_observed", (q) =>
          q.eq("memberId", member._id).gte("observedAt", since),
        )
        // Newest first, so truncation drops the oldest rows rather than the
        // one row that carries the current rate-limit reading.
        .order("desc")
        .take(SESSIONS_PER_MEMBER + 1);

      if (sessions.length === 0) continue;
      const truncated = sessions.length > SESSIONS_PER_MEMBER;
      const kept = truncated
        ? sessions.slice(0, SESSIONS_PER_MEMBER)
        : sessions;
      rows.push(rowFor(member, kept, truncated));
    }

    // Fullest window first: a lead opening this is looking for who is close to
    // running out, and that person should not be somewhere down a list sorted
    // by when somebody joined.
    rows.sort((a, b) => headroomRank(b) - headroomRank(a));

    return { windowDays, since, rows };
  },
});

/**
 * A window a lead can actually ask for.
 *
 * Bounded at both ends rather than trusted: the argument reaches an indexed
 * range read, and a window of a million days is a read of the whole table
 * dressed up as a preference. Ninety is the retention period — asking for more
 * would promise rows the reaper has already deleted.
 */
function clampWindow(days: number): number {
  if (!Number.isFinite(days)) return DEFAULT_WINDOW_DAYS;
  return Math.min(Math.max(Math.floor(days), 1), RETENTION_DAYS);
}

function rowFor(
  member: Doc<"members">,
  sessions: Doc<"usageSessions">[],
  truncated: boolean,
) {
  let costUsd = 0;
  let linesAdded = 0;
  let linesRemoved = 0;
  for (const session of sessions) {
    costUsd += session.costUsd ?? 0;
    linesAdded += session.linesAdded ?? 0;
    linesRemoved += session.linesRemoved ?? 0;
  }

  return {
    memberId: member._id,
    githubLogin: member.githubLogin,
    sessions: sessions.length,
    // Cents, so a sum of floats does not surface as 12.300000000000001. The
    // number is notional to two decimal places and rounding it here is what
    // stops every consumer having to.
    costUsd: Math.round(costUsd * 100) / 100,
    linesAdded,
    linesRemoved,
    // `sessions` is newest-first, so the first row reporting a window is the
    // most recent reading of it. A member whose newest session predates their
    // last rate-limited one still gets an answer rather than a blank.
    fiveHourPct: newest(sessions, "fiveHourPct"),
    fiveHourResetsAt: newest(sessions, "fiveHourResetsAt"),
    sevenDayPct: newest(sessions, "sevenDayPct"),
    sevenDayResetsAt: newest(sessions, "sevenDayResetsAt"),
    lastObservedAt: sessions[0].observedAt,
    truncated,
  };
}

/** The first defined value for a field, over rows already ordered newest-first. */
function newest(
  sessions: Doc<"usageSessions">[],
  field:
    "fiveHourPct" | "fiveHourResetsAt" | "sevenDayPct" | "sevenDayResetsAt",
): number | null {
  for (const session of sessions) {
    const value = session[field];
    if (value !== undefined) return value;
  }
  return null;
}

/** How close to the ceiling this member is, over either window. */
function headroomRank(row: {
  fiveHourPct: number | null;
  sevenDayPct: number | null;
}): number {
  return Math.max(row.fiveHourPct ?? -1, row.sevenDayPct ?? -1);
}

/**
 * Ninety days, and then gone.
 *
 * `cliSessions` needed the same treatment and went without it for months; this
 * table would grow faster than that one did — a row per session per developer,
 * written by a flush that fires while somebody is working — and every bounded
 * read over it (`rollup` takes 500 per member) is being asked to work on a set
 * with no ceiling. An unreaped table is what turns a `take(n)` into a silent
 * truncation.
 *
 * Ninety rather than forever because nothing here is a business record. It is a
 * fortnight's worth of "who is close to their rate limit" with a long tail
 * attached, and the tail is a standing description of how much every developer
 * worked, kept for no stated reason. The window a lead can ask for is capped at
 * the same number, so the query never promises rows this has removed.
 */
export const reapOld = internalMutation({
  args: {},
  returns: v.object({ deleted: v.number() }),
  handler: async (ctx) => {
    const cutoff =
      Math.floor(Date.now() / 1000) - RETENTION_DAYS * 24 * 60 * 60;
    const old = await ctx.db
      .query("usageSessions")
      .withIndex("by_observed", (q) => q.lt("observedAt", cutoff))
      .take(500);
    for (const row of old) {
      await ctx.db.delete("usageSessions", row._id);
    }
    return { deleted: old.length };
  },
});

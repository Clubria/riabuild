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
  accountEmail: v.optional(v.string()),
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
 * Still keyed by the member even though a lead reads this grouped by Claude
 * account: `memberId` is what the bearer token proved and the index the rollup
 * reads by, and an email is a *label* a laptop supplied. Keying a stored row on
 * something the client says would let a mistyped address merge two people's
 * sessions into one; grouping the answer by it merges nothing that cannot be
 * ungrouped by reading again.
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
          accountEmail: sample.accountEmail,
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
        // Same rule, and the same reason said about a name: `.claude.json` is
        // rewritten while Claude Code runs, so a render that caught it mid-write
        // reports no email. That is one unreadable file, never an account that
        // has become anonymous, and letting it clear a name already stored would
        // move a live session into the unnamed row.
        accountEmail: sample.accountEmail ?? existing.accountEmail,

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

/**
 * One **Claude account's** line in the rollup, which is what a lead reads.
 *
 * Not one member's, and the difference is the whole of why this is keyed the
 * way it is: a five-hour window belongs to an Anthropic account. One developer
 * signed in to two of them has two windows and a row each; the same account
 * open on a laptop and on a server is one window seen twice, and folding those
 * two config directories into a person would report a window nobody has.
 */
const usageRow = v.object({
  /**
   * Unique per row, and nothing else reads it: `email:<address>` for an account
   * that named itself, `account:<uuid>` for one that could not. It exists
   * because a table needs a key and neither field alone is one — an email is
   * absent on some rows, and an account uuid is one of several behind others.
   */
  accountKey: v.string(),
  /** `null` where riabuild has never read an email for this account. */
  accountEmail: v.union(v.string(), v.null()),
  /**
   * The config-directory uuid, and only on a row no email could be found for —
   * the one case where it is single-valued, and the only thing left to tell two
   * unnamed rows apart. A named row can stand for several directories on
   * several machines, so it carries none.
   */
  accountId: v.union(v.string(), v.null()),
  /** Sessions with at least one sample inside the window. */
  sessions: v.number(),
  /** From this account's newest sample. `null` where the harness reported none. */
  fiveHourPct: v.union(v.number(), v.null()),
  sevenDayPct: v.union(v.number(), v.null()),
  /** Unix seconds. When riabuild last heard anything from this account. */
  lastObservedAt: v.number(),
  /**
   * A member feeding this row had more sessions in the window than one read may
   * return, so the count beside it is a floor rather than the answer. Said out
   * loud rather than silently truncated — a `take()` nobody reports is a number
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

/**
 * A value and the instant it was observed, so that "newest wins" survives being
 * folded out of order.
 *
 * The rollup used to read the first defined value off a newest-first list,
 * which was sound while one member's sessions arrived in one ordered read. An
 * account can be fed by several members' reads now — two developers signed in
 * to one address is exactly what this grouping exists to show — so the order
 * rows arrive in says nothing, and each field carries the instant it belongs
 * to instead.
 */
type Newest<T> = { value: T; at: number };

function newer<T>(
  held: Newest<T> | null,
  value: T | undefined,
  at: number,
): Newest<T> | null {
  if (value === undefined) return held;
  if (held !== null && held.at >= at) return held;
  return { value, at };
}

function newerOf<T>(
  held: Newest<T> | null,
  incoming: Newest<T> | null,
): Newest<T> | null {
  if (incoming === null) return held;
  return newer(held, incoming.value, incoming.at);
}

function valueOf<T>(held: Newest<T> | null): T | null {
  return held === null ? null : held.value;
}

/** What one Claude account, or one row of them, adds up to while it is folded. */
type Tally = {
  accountId: string;
  email: Newest<string> | null;
  sessions: number;
  fiveHour: Newest<number> | null;
  sevenDay: Newest<number> | null;
  lastObservedAt: number;
  truncated: boolean;
};

function emptyTally(accountId: string): Tally {
  return {
    accountId,
    email: null,
    sessions: 0,
    fiveHour: null,
    sevenDay: null,
    lastObservedAt: 0,
    truncated: false,
  };
}

/**
 * Folds one session into the account that produced it.
 *
 * By `accountId` rather than by email, which is what makes the changeover free:
 * a session stored before riabuild sent an email at all, or one whose render
 * caught `.claude.json` mid-write, is still that account's session and takes
 * the account's name from whichever of its sessions did report one. Grouping on
 * the email itself would have split one account into a named row and an unnamed
 * one for the length of the window.
 */
function foldSession(
  into: Map<string, Tally>,
  session: Doc<"usageSessions">,
  truncated: boolean,
) {
  const tally = into.get(session.accountId) ?? emptyTally(session.accountId);
  const at = session.observedAt;
  tally.sessions += 1;
  tally.email = newer(tally.email, session.accountEmail, at);
  tally.fiveHour = newer(tally.fiveHour, session.fiveHourPct, at);
  tally.sevenDay = newer(tally.sevenDay, session.sevenDayPct, at);
  tally.lastObservedAt = Math.max(tally.lastObservedAt, at);
  tally.truncated = tally.truncated || truncated;
  into.set(session.accountId, tally);
}

/** `email:<address>`, or `account:<uuid>` for an account riabuild cannot name. */
function keyFor(tally: Tally): string {
  const email = valueOf(tally.email);
  return email === null ? `account:${tally.accountId}` : `email:${email}`;
}

/**
 * One row per address, folding together the config directories that share one.
 *
 * A developer with the same Claude account on their laptop and on two servers
 * has three `accountId`s and one rate-limit window, so the percentages take the
 * newest reading of the three rather than any kind of average. Sessions really
 * are three separate things being counted, so those are summed. Nothing merges
 * the email: every account in a group has the same one, because that is what
 * the key is made of.
 */
function rowsFrom(accounts: Map<string, Tally>) {
  const grouped = new Map<string, Tally>();
  for (const account of accounts.values()) {
    const key = keyFor(account);
    const held = grouped.get(key);
    if (held === undefined) {
      grouped.set(key, account);
      continue;
    }
    held.sessions += account.sessions;
    held.fiveHour = newerOf(held.fiveHour, account.fiveHour);
    held.sevenDay = newerOf(held.sevenDay, account.sevenDay);
    held.lastObservedAt = Math.max(held.lastObservedAt, account.lastObservedAt);
    held.truncated = held.truncated || account.truncated;
  }

  return [...grouped.entries()].map(([accountKey, tally]) => {
    const email = valueOf(tally.email);
    return {
      accountKey,
      accountEmail: email,
      // Only where the row *is* one unnamed account. See `usageRow`.
      accountId: email === null ? tally.accountId : null,
      sessions: tally.sessions,
      fiveHourPct: valueOf(tally.fiveHour),
      sevenDayPct: valueOf(tally.sevenDay),
      lastObservedAt: tally.lastObservedAt,
      truncated: tally.truncated,
    };
  });
}

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

    // Still read per member: `by_member_observed` is the index that exists, and
    // it is a bounded read each rather than one unbounded scan of the table.
    // Who a session belonged to simply stops being what the answer is grouped
    // by — the accounts are folded across every member's read, and the rows
    // come out of that.
    const accounts = new Map<string, Tally>();
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
      for (const session of kept) foldSession(accounts, session, truncated);
    }

    const rows = rowsFrom(accounts);

    // Fullest window first: a lead opening this is looking for who is close to
    // running out, and that account should not be somewhere down a list sorted
    // by whichever member happened to be read first.
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

/** How close to the ceiling this account is, over either window. */
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

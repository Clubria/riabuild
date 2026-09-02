import { cronJobs } from "convex/server";
import { internal } from "./_generated/api";

/**
 * `POST /api/v1/cli/device` is unauthenticated and writes a row per call, and
 * most of those rows are logins nobody ever completed. Without a sweep the
 * table only grows, so dead requests are deleted an hour after they expire.
 *
 * Hourly rather than continuously: a row is a couple of hundred bytes and
 * already useless the moment it expires, so nothing is gained by reaping it
 * promptly.
 */
const crons = cronJobs();

crons.interval(
  "reap expired device codes",
  { hours: 1 },
  internal.cliAuth.reapExpired,
  {},
);

/**
 * `cliSessions` needed the same treatment and never had it. A session lives
 * ninety days, a member collects one per laptop and one per delegated server,
 * and nothing deleted the row when it died — so the table only ever grew, and
 * the bounded reads over it (`sessions.listMine` takes 50; `members.setStatus`
 * pages a member's sessions) were being asked to work on a set with no ceiling.
 * An unreaped table is what turns a `take(n)` into a silent truncation.
 */
crons.interval(
  "reap dead CLI sessions",
  { hours: 1 },
  internal.sessions.reapDead,
  {},
);

/**
 * `usageSessions` is the fastest-growing table here — a row per Claude Code
 * session per developer, written by a flush that fires while somebody is
 * working — and the lead rollup reads it with a `take(500)` per member, which
 * is the shape an unreaped table turns into a silent truncation.
 *
 * Ninety days because nothing in it is a business record. It is "who is close
 * to their rate limit" plus a fortnight of context, and everything past that is
 * a standing description of how much each developer worked, kept for no stated
 * reason — on data collected from personal Pro and Max subscriptions, which is
 * exactly the kind of tail to delete on a timer rather than to decide about
 * later. `usage.rollup` caps the window a lead may ask for at the same ninety
 * days, so the query never promises rows this has already removed.
 *
 * Hourly, like the two above, and bounded per run: a sweep that must finish in
 * one transaction is a sweep that eventually cannot.
 */
crons.interval("reap old usage rows", { hours: 1 }, internal.usage.reapOld, {});

export default crons;

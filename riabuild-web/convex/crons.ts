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

export default crons;

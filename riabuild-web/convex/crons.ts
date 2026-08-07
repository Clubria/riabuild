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

export default crons;

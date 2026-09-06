import { useData } from "../data/context";
import { UsageRow } from "../data/types";
import { formatTime } from "../lib/time";
import {
  Alert,
  Badge,
  Column,
  DataTable,
  Empty,
  Loading,
  TEXT_TONE,
  Tone,
} from "../ui";

/**
 * How much of each Claude account's allowance is gone, led by the only number
 * that runs out.
 *
 * **A row is an account, not a person.** These are personal Pro and Max
 * subscriptions and the five-hour and seven-day rate-limit windows are the real
 * budget — and a window belongs to the Anthropic account it was spent from. One
 * developer signed in to two accounts has two windows and a row each; the same
 * account open on a laptop and on a server is one window seen twice. A table
 * keyed by developer could say neither.
 *
 * Deliberately absent: which repository, which model, and anything about what
 * the work *was*. The status line payload carries `workspace.repo` and this
 * drops it — a usage tracker that also reports what each developer was working
 * on is a different product with a different conversation attached to it.
 *
 * Design: `docs/superpowers/specs/2026-08-29-usage-tracking-design.md`.
 */

/** Where a used-percentage stops being ordinary. */
const WARN_AT = 75;
const DANGER_AT = 90;

function pctTone(pct: number | null): Tone {
  if (pct === null) return "muted";
  if (pct >= DANGER_AT) return "danger";
  if (pct >= WARN_AT) return "warn";
  return "ok";
}

/** Cells in the bar. Eight, because a tenth column of blocks buys nothing at 380px. */
const CELLS = 8;

/**
 * A used-percentage, as a bar and a number.
 *
 * The blocks are the same `█`/`░` vocabulary the status line itself prints, and
 * they are `aria-hidden`: a screen reader announcing "full block, full block,
 * light shade" eight times is worse than silence, and the percentage beside
 * them is the same fact in a form it can read. No charting library — a bar made
 * of two characters is a bar, and this is a terminal.
 */
function Meter({ pct, label }: { pct: number | null; label: string }) {
  if (pct === null) {
    return (
      <span
        className="text-fg-faint"
        title="This account reports no rate-limit window."
      >
        &mdash;
      </span>
    );
  }
  const clamped = Math.min(Math.max(pct, 0), 100);
  const filled = Math.round((clamped / 100) * CELLS);
  const tone = TEXT_TONE[pctTone(pct)];
  return (
    <span
      className={`inline-flex items-baseline gap-1.5 whitespace-nowrap ${tone}`}
    >
      <span aria-hidden="true">
        {"█".repeat(filled)}
        {"░".repeat(CELLS - filled)}
      </span>
      <span>
        <span className="sr-only">{label} </span>
        {Math.round(clamped)}%
      </span>
    </span>
  );
}

/**
 * A gap, in the shortest form that is still true: `4m`, `3h`, `6d`.
 *
 * Compact because the column that uses it sits to the right of three others,
 * and the full `25 Jul 2026, 19:20` in it pushed the table into a sideways
 * scroll at 1440px. The exact instant is still there, in the `title`.
 */
function shortGap(ms: number): string {
  const minutes = Math.round(ms / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

/** When riabuild last heard anything, counted backwards. */
function seenAgo(seconds: number, now: number): string {
  const ms = now - seconds * 1000;
  return ms <= 0 ? "just now" : `${shortGap(ms)} ago`;
}

/** The instant itself, for the `title` behind a relative one. */
function fromSeconds(seconds: number | null): string {
  return seconds === null ? "never" : formatTime(seconds * 1000);
}

/**
 * How much of an account uuid is worth showing, on the rows that have nothing
 * else. Enough to tell two of them apart, and never presented as something to
 * type: it names a directory on somebody's laptop.
 */
const ID_SHOWN = 8;

/**
 * Who the row is, which is an email address wherever riabuild has read one.
 *
 * An account with no email is still an account somebody is spending a window
 * from, so it is a row rather than a gap — and it says it is unnamed rather
 * than borrowing a developer's login, which would be riabuild asserting a
 * link it did not observe. It happens for an account signed out at the moment
 * of the read, a `.claude.json` caught mid-write, and every sample sent by a
 * riabuild older than the one that started reporting the address.
 */
function Who({ row }: { row: UsageRow }) {
  if (row.accountEmail !== null) {
    return <span className="wrap-value text-fg">{row.accountEmail}</span>;
  }
  return (
    <span
      className="text-fg-dim"
      title="riabuild has not read an email for this Claude account — it was signed out, or the laptop is on a riabuild that predates reporting one."
    >
      unnamed account{" "}
      <span className="text-fg-faint">
        {(row.accountId ?? "").slice(0, ID_SHOWN)}
      </span>
    </span>
  );
}

export function Usage() {
  const data = useData();

  if (data.usage.state === "loading") {
    return <Loading label="loading usage" />;
  }
  if (data.usage.state === "error") {
    return (
      <Alert tone="danger" title="Could not load usage">
        <p className="wrap-value">{data.usage.message}</p>
      </Alert>
    );
  }

  const { windowDays, rows } = data.usage.value;
  // The ticking clock, so "2h ago" keeps up while the page is open rather than
  // freezing at whatever it said when the tab was opened.
  const now = data.now;

  const columns: Column<UsageRow>[] = [
    {
      key: "who",
      header: "claude account",
      grow: true,
      render: (row) => (
        <span className="inline-flex flex-wrap items-baseline gap-1.5">
          <Who row={row} />
          {/* Said out loud rather than swallowed: the count to the right of
              this badge is a floor, not a total. */}
          {row.truncated && <Badge tone="warn">partial</Badge>}
        </span>
      ),
    },
    {
      key: "fiveHour",
      header: "5h used",
      render: (row) => (
        <Meter pct={row.fiveHourPct} label="five-hour window used" />
      ),
    },
    {
      key: "sevenDay",
      header: "7d used",
      render: (row) => (
        <Meter pct={row.sevenDayPct} label="seven-day window used" />
      ),
    },
    {
      key: "sessions",
      header: "total sessions",
      align: "end",
      render: (row) => <span className="text-fg-dim">{row.sessions}</span>,
    },
    {
      key: "seen",
      header: "last seen",
      align: "end",
      priority: "wide",
      render: (row) => (
        <span
          className="whitespace-nowrap text-fg-faint"
          title={fromSeconds(row.lastObservedAt)}
        >
          {seenAgo(row.lastObservedAt, now)}
        </span>
      ),
    },
  ];

  return (
    <>
      <p className="mb-3 max-w-prose text-fg-dim">
        The last {windowDays} days, per Claude account, from each
        developer&rsquo;s own status line. Rate-limit headroom first: on a
        subscription nobody pays per token, so the window is the only thing that
        actually runs out.
      </p>
      <DataTable
        caption={`Claude Code usage per account over the last ${windowDays} days`}
        columns={columns}
        rows={rows}
        rowKey={(row) => row.accountKey}
        empty={
          <Empty glyph="◔" title="Nothing reported yet.">
            Every Claude Code account riabuild manages reports its own usage,
            about once a minute, while somebody is working. A panel this empty
            means no session has run since the team upgraded to a riabuild that
            collects &mdash; or that nobody has run{" "}
            <span className="text-fg-dim">riabuild</span> since, which is what
            installs the status line that does the reporting.
          </Empty>
        }
      />
      {/* Only beside a table. It explains a badge and a row shape, neither of
          which exists on an empty panel — a legend for a table that is not
          there reads as a description of data being withheld. */}
      {rows.length > 0 && (
        <p className="mt-3 max-w-prose text-xs text-fg-faint">
          One row is one Claude account, so a developer signed in to two of them
          is two rows and one account used from two machines is a single row. A
          row marked <span className="text-warn">partial</span> had more
          sessions than one read returns, so its count is a floor. Nothing here
          records which repository, which file or which prompt.
        </p>
      )}
    </>
  );
}

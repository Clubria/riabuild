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
 * What Claude Code cost the team, led by the only number that runs out.
 *
 * These are personal Pro and Max subscriptions, so nobody pays per token and
 * the five-hour and seven-day rate-limit windows are the real budget. Cost is
 * here as a measure of relative effort and is labelled **list-price
 * equivalent** in the header, in the footnote and in the column's accessible
 * name — never "spend". A developer's own subscription is not the team's money,
 * and an unlabelled dollar figure ends up in a budget.
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
 * Compact because both columns that use it sit to the right of six others, and
 * the full `25 Jul 2026, 19:20` in each of them pushed the table into a
 * sideways scroll at 1440px — a lead reading a rate-limit window wants "in two
 * hours", not a date. The exact instant is still there, in the `title`.
 */
function shortGap(ms: number): string {
  const minutes = Math.round(ms / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

/** When a window rolls over, counted forwards. */
function resetsIn(seconds: number | null, now: number): string {
  if (seconds === null) return "—";
  const ms = seconds * 1000 - now;
  return ms <= 0 ? "any moment" : `in ${shortGap(ms)}`;
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
 * Two decimal places, always, and a leading `$`.
 *
 * `toFixed` rather than a locale formatter: the value is notional and the point
 * of the column is comparing one row against another, which a thousands
 * separator that moves with the reader's locale makes harder rather than
 * easier.
 */
function listPrice(usd: number): string {
  return `$${usd.toFixed(2)}`;
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
  // The ticking clock, so "resets in 2h" counts down while the page is open
  // rather than freezing at whatever it said when the tab was opened.
  const now = data.now;

  const columns: Column<UsageRow>[] = [
    {
      key: "who",
      header: "github",
      grow: true,
      render: (row) => (
        <span className="inline-flex flex-wrap items-baseline gap-1.5">
          <span className="text-fg">@{row.githubLogin}</span>
          {/* Said out loud rather than swallowed: everything to the right of
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
      header: "sessions",
      align: "end",
      render: (row) => <span className="text-fg-dim">{row.sessions}</span>,
    },
    {
      key: "cost",
      header: "list-price equiv",
      align: "end",
      render: (row) => (
        <span
          className="text-fg-dim"
          title="List-price equivalent — not money anyone spent."
        >
          {listPrice(row.costUsd)}
        </span>
      ),
    },
    {
      key: "lines",
      header: "lines",
      align: "end",
      priority: "wide",
      render: (row) => (
        <span className="whitespace-nowrap text-fg-faint">
          <span className="text-ok">+{row.linesAdded}</span>{" "}
          <span className="text-danger">&minus;{row.linesRemoved}</span>
        </span>
      ),
    },
    {
      key: "resets",
      header: "5h resets",
      align: "end",
      priority: "wide",
      render: (row) => (
        <span
          className="whitespace-nowrap text-fg-faint"
          title={fromSeconds(row.fiveHourResetsAt)}
        >
          {resetsIn(row.fiveHourResetsAt, now)}
        </span>
      ),
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
        The last {windowDays} days, from each developer&rsquo;s own status line.
        Rate-limit headroom first: on a subscription nobody pays per token, so
        the window is the only thing that actually runs out.
      </p>
      <DataTable
        caption={`Claude Code usage per member over the last ${windowDays} days`}
        columns={columns}
        rows={rows}
        rowKey={(row) => row.memberId}
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
      {/* Only beside a table. It explains three columns and a badge, none of
          which exist on an empty panel — a legend for a table that is not there
          reads as a description of data being withheld. */}
      {rows.length > 0 && (
        <p className="mt-3 max-w-prose text-xs text-fg-faint">
          <span className="text-fg-dim">list-price equivalent</span> is what the
          work would have cost against the public API price sheet. These are
          personal Pro and Max subscriptions, so it is a measure of relative
          effort and not money anyone spent &mdash; it is not a spend report and
          does not belong in a budget. A row marked{" "}
          <span className="text-warn">partial</span> had more sessions than one
          read returns, so its totals are a floor. Nothing here records which
          repository, which file or which prompt.
        </p>
      )}
    </>
  );
}

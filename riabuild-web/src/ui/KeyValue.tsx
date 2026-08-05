import { ReactNode } from "react";
import { TEXT_TONE, Tone } from "./tone";

export type KeyValueRow = {
  label: string;
  value: ReactNode;
  tone?: Tone;
};

/**
 * The machine-fact grid: device, version, callback, repo. Values wrap rather
 * than widen — a 300-character device label must not push the page sideways.
 */
export function KeyValue({ rows }: { rows: KeyValueRow[] }) {
  return (
    <dl className="grid grid-cols-[minmax(0,6rem)_minmax(0,1fr)] gap-x-4 gap-y-1.5 sm:grid-cols-[minmax(0,8rem)_minmax(0,1fr)]">
      {rows.map((row) => (
        <div key={row.label} className="contents">
          <dt className="min-w-0 text-fg-faint wrap-value">{row.label}</dt>
          <dd
            className={`m-0 min-w-0 wrap-value ${TEXT_TONE[row.tone ?? "default"]}`}
          >
            {row.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

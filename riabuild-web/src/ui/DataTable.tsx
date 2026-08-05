import { ReactNode } from "react";

export type Column<T> = {
  key: string;
  header: string;
  align?: "start" | "end";
  /** `wide` columns are hidden below 640px, where there is no room for them. */
  priority?: "always" | "wide";
  /** Let one column absorb the slack so the others stay at their natural width. */
  grow?: boolean;
  render: (row: T) => ReactNode;
};

/**
 * One list implementation for the whole app. Members, machines and the audit log
 * were three hand-rolled versions of this before it existed; a fourth list
 * should configure this with columns rather than grow a fourth.
 *
 * A real `<table>` because the data is tabular — a screen reader announcing
 * "column: role, row 3" is the whole point, and a grid of divs cannot do it.
 * The wrapper scrolls itself so a wide table never scrolls the document.
 */
export function DataTable<T>({
  columns,
  rows,
  rowKey,
  renderActions,
  empty,
  caption,
}: {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  renderActions?: (row: T) => ReactNode;
  empty: ReactNode;
  caption: string;
}) {
  if (rows.length === 0) return <>{empty}</>;

  return (
    <div className="-mx-3 overflow-x-auto px-3 sm:mx-0 sm:px-0">
      <table className="w-full border-collapse text-left">
        <caption className="sr-only">{caption}</caption>
        <thead>
          <tr className="border-b border-rule">
            {columns.map((column) => (
              <th
                key={column.key}
                scope="col"
                className={`py-1.5 pr-4 text-xs font-normal tracking-wider text-fg-faint uppercase ${
                  column.align === "end" ? "text-right" : "text-left"
                } ${column.priority === "wide" ? "hidden sm:table-cell" : ""} ${
                  column.grow === true ? "w-full" : "whitespace-nowrap"
                }`}
              >
                {column.header}
              </th>
            ))}
            {renderActions !== undefined && (
              <th scope="col" className="py-1.5 text-right">
                <span className="sr-only">Actions</span>
              </th>
            )}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr
              key={rowKey(row)}
              className="border-b border-rule/60 align-baseline last:border-b-0 hover:bg-bg-raised"
            >
              {columns.map((column) => (
                <td
                  key={column.key}
                  className={`py-2 pr-4 ${
                    column.align === "end" ? "text-right" : "text-left"
                  } ${column.priority === "wide" ? "hidden sm:table-cell" : ""} ${
                    column.grow === true ? "min-w-0 wrap-value" : "whitespace-nowrap"
                  }`}
                >
                  {column.render(row)}
                </td>
              ))}
              {renderActions !== undefined && (
                <td className="py-2 text-right whitespace-nowrap">
                  <span className="inline-flex flex-wrap justify-end gap-1.5">
                    {renderActions(row)}
                  </span>
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

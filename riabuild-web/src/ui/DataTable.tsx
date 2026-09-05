import { Fragment, ReactNode, useEffect, useRef, useState } from "react";

/**
 * Whether the element's content is wider than the element.
 *
 * Measured rather than assumed: a hint that says "scroll" when there is nothing
 * to scroll is the same class of lie as a keybinding we do not handle.
 */
function useOverflows(ref: React.RefObject<HTMLElement | null>): boolean {
  const [overflows, setOverflows] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (el === null) return;
    const measure = () => setOverflows(el.scrollWidth > el.clientWidth + 1);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    for (const child of el.children) observer.observe(child);
    return () => observer.disconnect();
  }, [ref]);
  return overflows;
}

export type Column<T> = {
  key: string;
  header: string;
  align?: "start" | "end";
  /** `wide` columns are hidden below 640px, where there is no room for them. */
  priority?: "always" | "wide";
  /**
   * Let one column absorb the slack so the others stay at their natural width.
   * It also gets a 14ch floor: without one, action controls that refuse to
   * shrink squeeze it to a sliver and a long value stacks a character or two
   * per line down a column six characters wide.
   */
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
  renderSubRow,
  empty,
  caption,
}: {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  renderActions?: (row: T) => ReactNode;
  /**
   * A second line under a row, spanning every column — prose about the row
   * rather than another field of it. `null` for a row that has none, which is
   * the ordinary case and costs that row nothing: no cell, no blank line.
   *
   * It exists because a sentence is the one thing a column cannot hold. Columns
   * size to their content and `grow` bottoms out at 14ch, so a description put
   * in one wraps a word per line the moment another row carries a long name —
   * and the row it makes two hundred pixels tall is not even the row it belongs
   * to. Full width is also how the CLI's own boxes draw the same thing.
   */
  renderSubRow?: (row: T) => ReactNode;
  empty: ReactNode;
  caption: string;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const overflows = useOverflows(scroller);

  if (rows.length === 0) return <>{empty}</>;

  return (
    <>
      {/* `tabIndex` is not decoration: a region that scrolls with a mouse and
          not with a keyboard strands anyone who does not use one. The browser
          handles the arrow keys once the region is focusable — the page still
          listens for no keystrokes of its own. */}
      <div
        ref={scroller}
        // `contain: paint` is not decoration. `overflow-x: auto` alone sizes and
        // scrolls this box correctly, but the overflowing table still extends the
        // *document's* scrollable region — the page picks up a horizontal
        // scrollbar for a table that is already scrolling itself. Declaring that
        // descendants must not paint outside this box is what stops it.
        // Measured, not guessed: without it the document is 957px wide at a
        // 768px viewport; with it, 768px.
        className="-mx-3 overflow-x-auto px-3 [contain:paint] sm:mx-0 sm:px-0"
        tabIndex={0}
        role="region"
        aria-label={caption}
      >
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
            {rows.map((row) => {
              const under = renderSubRow?.(row) ?? null;
              return (
                <Fragment key={rowKey(row)}>
                  <tr
                    className={`align-baseline hover:bg-bg-raised ${
                      under === null
                        ? "border-b border-rule/60 last:border-b-0"
                        : "border-b-0"
                    }`}
                  >
                    {columns.map((column) => (
                      <td
                        key={column.key}
                        className={`py-2 pr-4 ${
                          column.align === "end" ? "text-right" : "text-left"
                        } ${column.priority === "wide" ? "hidden sm:table-cell" : ""} ${
                          column.grow === true
                            ? "min-w-[14ch] wrap-value"
                            : "whitespace-nowrap"
                        }`}
                      >
                        {column.render(row)}
                      </td>
                    ))}
                    {renderActions !== undefined && (
                      <td className="py-2 text-right whitespace-nowrap">
                        {/* Never wraps, and children never shrink. A `grow` column
                      takes every spare pixel; without both, the controls here
                      are squeezed until a role `select` is 20px wide and
                      unreadable, or stacked into a ragged column even at
                      1440px. They keep their size and the region scrolls. */}
                        <span className="inline-flex flex-nowrap items-center justify-end gap-1.5 [&>*]:shrink-0">
                          {renderActions(row)}
                        </span>
                      </td>
                    )}
                  </tr>
                  {under !== null && (
                    <tr className="border-b border-rule/60 last:border-b-0 hover:bg-bg-raised">
                      {/* One cell across the whole row rather than a second line
                    inside a column. A description put in a column is sized by
                    that column: `grow` bottoms out at 14ch once another row
                    carries a long name, and a sentence wrapping one word per
                    line turns a one-line row into two hundred pixels of
                    nothing — visibly, on the row it is not even about. */}
                      <td
                        colSpan={
                          columns.length + (renderActions === undefined ? 0 : 1)
                        }
                        className="pb-2 wrap-value"
                      >
                        {under}
                      </td>
                    </tr>
                  )}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
      {overflows && (
        <p className="mt-1.5 text-right text-xs text-fg-faint">
          <span aria-hidden="true">◂ </span>
          this table scrolls sideways
          <span aria-hidden="true"> ▸</span>
        </p>
      )}
    </>
  );
}

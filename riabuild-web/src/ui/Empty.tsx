import { ReactNode } from "react";

/**
 * The nothing-here state. An empty list always says why it is empty and what
 * would fill it — "no rows" alone leaves a reader unsure whether it is broken.
 */
export function Empty({
  glyph = "∅",
  title,
  children,
  action,
}: {
  glyph?: string;
  title: string;
  children?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center gap-2 border border-dashed border-rule px-4 py-8 text-center">
      <span aria-hidden="true" className="text-xl text-fg-faint">
        {glyph}
      </span>
      <p className="text-fg-dim">{title}</p>
      {children !== undefined && (
        <div className="max-w-prose text-xs text-fg-faint">{children}</div>
      )}
      {action !== undefined && <div className="mt-1">{action}</div>}
    </div>
  );
}

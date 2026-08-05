import { ReactNode } from "react";

/**
 * The pinned bottom line. It reports state and nothing else — no `^K cmd`, no
 * `q quit`. The page handles no keystrokes, so a keybinding hint here would be
 * decoration that lies.
 */
export function StatusBar({
  left,
  right,
}: {
  left?: ReactNode;
  right?: ReactNode;
}) {
  return (
    <footer className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-rule bg-bg-raised px-3 py-1.5 text-xs sm:px-4">
      <span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
        {left}
      </span>
      <span className="ml-auto flex flex-wrap items-center gap-x-3 gap-y-1 text-fg-faint">
        {right}
      </span>
    </footer>
  );
}

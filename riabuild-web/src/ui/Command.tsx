import { useState } from "react";
import { Button } from "./Button";

type CopyState = "idle" | "copied" | "failed";

/**
 * A shell line with a copy target. Copy targets exist so nobody retypes a
 * command and mistypes the tap name.
 *
 * The clipboard API is absent in insecure contexts and can be denied outright,
 * so failure is a rendered state rather than a silent no-op — otherwise the
 * button looks like it worked and the paste is stale.
 */
export function Command({
  command,
  prompt = "$",
}: {
  command: string;
  prompt?: string;
}) {
  const [state, setState] = useState<CopyState>("idle");

  function copy() {
    const clipboard = navigator.clipboard;
    if (clipboard === undefined) {
      setState("failed");
      return;
    }
    void clipboard.writeText(command).then(
      () => {
        setState("copied");
        setTimeout(() => setState("idle"), 1600);
      },
      () => setState("failed"),
    );
  }

  return (
    <div className="flex items-stretch border border-rule bg-bg-sunk">
      {/* Wraps rather than scrolls. A real terminal wraps a long command too,
          and a horizontally scrolling code block hides the end of the very
          string the reader came to copy — from a keyboard user entirely, unless
          the region is made a tab stop it has no reason to be. Newlines survive
          because `pre-wrap` keeps them. */}
      <code className="min-w-0 flex-1 px-2.5 py-2 whitespace-pre-wrap text-fg wrap-value">
        <span aria-hidden="true" className="mr-2 text-fg-faint select-none">
          {prompt}
        </span>
        {command}
      </code>
      <span className="flex shrink-0 items-start border-l border-rule px-1.5 py-1.5">
        <Button
          variant="quiet"
          onClick={copy}
          aria-label={`Copy command: ${command}`}
        >
          {state === "copied"
            ? "copied"
            : state === "failed"
              ? "copy failed"
              : "copy"}
        </Button>
      </span>
    </div>
  );
}

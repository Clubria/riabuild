import { useState } from "react";
import { Button } from "./Button";

type CopyState = "idle" | "copied" | "failed";

/**
 * An opaque value a developer copies but never reads aloud — a member id, which
 * names their directory on a shared server.
 *
 * Not a `Command` prop: `Command`'s `$` prompt means *this is a shell command*,
 * and an identifier is not one.
 */
export function Copyable({ value, label }: { value: string; label: string }) {
  const [state, setState] = useState<CopyState>("idle");
  const short = value.split("-")[0] || value;

  function copy() {
    const clipboard = navigator.clipboard;
    if (clipboard === undefined) {
      setState("failed");
      return;
    }
    void clipboard.writeText(value).then(
      () => {
        setState("copied");
        setTimeout(() => setState("idle"), 1600);
      },
      () => setState("failed"),
    );
  }

  return (
    <span className="inline-flex items-center gap-2">
      {/* `title` and the visually hidden span carry the full value. An
          `aria-label` on a plain span is `aria-prohibited-attr` under axe,
          which e2e/helpers.ts runs on every page. */}
      <code className="font-mono text-fg-dim" title={value}>
        <span aria-hidden="true">{short}…</span>
        <span className="sr-only">{value}</span>
      </code>
      <Button variant="quiet" onClick={copy} aria-label={`Copy ${label}`}>
        {state === "copied" ? "copied" : state === "failed" ? "copy failed" : "copy"}
      </Button>
    </span>
  );
}

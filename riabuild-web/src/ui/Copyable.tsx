import { useState } from "react";
import { Button } from "./Button";

type CopyState = "idle" | "copied" | "failed";

// The visible prefix length, whether or not the value contains a dash — a
// UUID's first segment happens to be this long, but the cap applies
// regardless, so a long dash-less token doesn't render in full.
const SHORT_LENGTH = 8;

/**
 * An opaque value a developer copies but never reads aloud — a member id, which
 * names their directory on a shared server.
 *
 * Not a `Command` prop: `Command`'s `$` prompt means *this is a shell command*,
 * and an identifier is not one.
 */
export function Copyable({ value, label }: { value: string; label: string }) {
  const [state, setState] = useState<CopyState>("idle");
  const dashIndex = value.indexOf("-");
  const prefix = dashIndex === -1 ? value : value.slice(0, dashIndex);
  const short = prefix.slice(0, SHORT_LENGTH);
  // Only claim truncation happened if it actually did — a value that fits
  // inside the cap renders whole, with no ellipsis appended to it.
  const truncated = short.length < value.length;

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

  // The button's own aria-label stays fixed to "Copy <label>" — a dynamic
  // label would make a screen reader re-announce the control itself on every
  // state change, rather than the result. This separate live region carries
  // the result instead, independent of the button's accessible name.
  const announcement =
    state === "copied" ? `${label} copied` : state === "failed" ? `Copy failed: ${label}` : "";

  return (
    <span className="inline-flex min-w-0 items-center gap-2">
      {/* `title` and the visually hidden span carry the full value. An
          `aria-label` on a plain span is `aria-prohibited-attr` under axe,
          which e2e/helpers.ts runs on every page. */}
      <code className="min-w-0 font-mono text-fg-dim wrap-value" title={value}>
        <span aria-hidden="true">
          {short}
          {truncated ? "…" : ""}
        </span>
        <span className="sr-only">{value}</span>
      </code>
      <Button variant="quiet" onClick={copy} aria-label={`Copy ${label}`}>
        {state === "copied" ? "copied" : state === "failed" ? "copy failed" : "copy"}
      </Button>
      <span aria-live="polite" className="sr-only">
        {announcement}
      </span>
    </span>
  );
}

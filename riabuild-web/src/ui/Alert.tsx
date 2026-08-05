import { ReactNode } from "react";
import { BORDER_TONE, TEXT_TONE, Tone } from "./tone";

const GLYPH: Record<Tone, string> = {
  default: "»",
  accent: "»",
  ok: "✓",
  warn: "!",
  danger: "✗",
  muted: "»",
};

/**
 * An inline message. Anything a person must read before acting goes here rather
 * than into prose, so it survives being skimmed.
 */
export function Alert({
  tone = "warn",
  title,
  children,
}: {
  tone?: Tone;
  title: string;
  children?: ReactNode;
}) {
  return (
    <div
      role={tone === "danger" ? "alert" : undefined}
      className={`border-l-2 bg-bg-raised px-3 py-2.5 ${BORDER_TONE[tone]}`}
    >
      <p
        className={`flex items-baseline gap-2 text-xs tracking-wider uppercase ${TEXT_TONE[tone]}`}
      >
        <span aria-hidden="true">{GLYPH[tone]}</span>
        <span className="wrap-value">{title}</span>
      </p>
      {children !== undefined && (
        <div className="mt-1.5 max-w-prose text-fg-dim wrap-value">{children}</div>
      )}
    </div>
  );
}

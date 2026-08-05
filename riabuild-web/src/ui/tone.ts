/**
 * One tone vocabulary for the whole library. Components map a tone to classes
 * through these tables rather than each inventing its own colour prop, so a
 * `danger` badge and a `danger` panel are the same red.
 */
export type Tone = "default" | "accent" | "ok" | "warn" | "danger" | "muted";

export const TEXT_TONE: Record<Tone, string> = {
  default: "text-fg",
  accent: "text-accent",
  ok: "text-ok",
  warn: "text-warn",
  danger: "text-danger",
  muted: "text-fg-faint",
};

export const BORDER_TONE: Record<Tone, string> = {
  default: "border-rule",
  accent: "border-accent",
  ok: "border-ok",
  warn: "border-warn",
  danger: "border-danger",
  muted: "border-rule",
};

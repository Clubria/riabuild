import { TEXT_TONE, Tone } from "./tone";

/**
 * A status indicator. The glyph is decorative and the label carries the meaning,
 * so this stays readable with colour vision differences and in a screen reader.
 */
export function Dot({ tone = "default", label }: { tone?: Tone; label: string }) {
  return (
    <span className="inline-flex min-w-0 max-w-full items-center gap-1.5">
      <span aria-hidden="true" className={TEXT_TONE[tone]}>
        ●
      </span>
      <span className="min-w-0 text-fg-dim wrap-value">{label}</span>
    </span>
  );
}

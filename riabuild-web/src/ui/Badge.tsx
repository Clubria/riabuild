import { ReactNode } from "react";
import { BORDER_TONE, TEXT_TONE, Tone } from "./tone";

/**
 * A state chip.
 *
 * Wraps rather than refusing to: real badges are one word and never wrap, but a
 * badge is not allowed to be the thing that pushes a 380px page sideways when
 * some unexpected value lands in it.
 */
export function Badge({
  tone = "default",
  children,
}: {
  tone?: Tone;
  children: ReactNode;
}) {
  return (
    <span
      className={`inline-block max-w-full border px-1.5 py-px text-[0.6875rem] tracking-wider uppercase wrap-value ${BORDER_TONE[tone]} ${TEXT_TONE[tone]}`}
    >
      {children}
    </span>
  );
}

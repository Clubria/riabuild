import { ReactNode } from "react";
import { BORDER_TONE, TEXT_TONE, Tone } from "./tone";

/** A state chip. `‹active›`-style angle marks keep it legible without colour. */
export function Badge({
  tone = "default",
  children,
}: {
  tone?: Tone;
  children: ReactNode;
}) {
  return (
    <span
      className={`inline-block border px-1.5 py-px text-[0.6875rem] tracking-wider uppercase whitespace-nowrap ${BORDER_TONE[tone]} ${TEXT_TONE[tone]}`}
    >
      {children}
    </span>
  );
}

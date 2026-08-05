import { ReactNode } from "react";
import { BORDER_TONE, TEXT_TONE, Tone } from "./tone";

/**
 * A titled box. The title is notched into the top rule the way a TUI draws it —
 * absolutely positioned over the border with the page background behind it —
 * rather than assembled from box-drawing characters, which would need a fixed
 * character grid and would be read aloud by a screen reader.
 *
 * `index` is the step number. The numbering is not decoration: onboarding is a
 * sequence and the number is what someone refers to when they get stuck on one.
 */
export function Panel({
  title,
  index,
  subtitle,
  tone = "default",
  actions,
  dense = false,
  id,
  children,
}: {
  title?: string;
  index?: string;
  subtitle?: ReactNode;
  tone?: Tone;
  actions?: ReactNode;
  dense?: boolean;
  id?: string;
  children: ReactNode;
}) {
  const labelled = title !== undefined;
  return (
    <section
      id={id}
      // `min-w-0` is load-bearing: a flex item defaults to `min-width: auto`,
      // so without it a DataTable's `overflow-x-auto` wrapper widens the panel
      // instead of scrolling, and a 60-character login pushes the whole
      // document sideways at 380px.
      className={`relative min-w-0 border ${BORDER_TONE[tone]} ${
        dense ? "px-3 py-3" : "px-3 py-4 sm:px-5 sm:py-5"
      } ${labelled ? "mt-3" : ""}`}
    >
      {labelled && (
        <h2 className="absolute -top-[0.75em] left-3 flex max-w-[calc(100%-1.5rem)] items-baseline gap-2 bg-bg px-2 text-xs tracking-widest uppercase">
          {index !== undefined && (
            <span className={TEXT_TONE[tone === "default" ? "accent" : tone]}>
              {index}
              {/* Separated, or `lead` + `members and roles` reads as one
                  phrase rather than a label and a title. */}
              <span aria-hidden="true" className="ml-2 text-fg-faint">
                ·
              </span>
            </span>
          )}
          <span className={tone === "default" ? "text-fg-dim" : TEXT_TONE[tone]}>
            {title}
          </span>
        </h2>
      )}

      {(subtitle !== undefined || actions !== undefined) && (
        <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
          {subtitle !== undefined ? (
            <p className="min-w-0 max-w-prose text-fg-dim">{subtitle}</p>
          ) : (
            <span />
          )}
          {actions !== undefined && (
            <span className="flex flex-wrap gap-2">{actions}</span>
          )}
        </div>
      )}

      {children}
    </section>
  );
}

import { ReactNode, useEffect, useState } from "react";

const SPINNER = ["|", "/", "-", "\\"];

/** The pending spinner a terminal would draw. Stops dead under reduced motion. */
function useSpinner(active: boolean): string {
  const [frame, setFrame] = useState(0);
  useEffect(() => {
    if (!active) return;
    if (
      typeof matchMedia === "function" &&
      matchMedia("(prefers-reduced-motion: reduce)").matches
    ) {
      return;
    }
    const timer = setInterval(() => setFrame((f) => f + 1), 110);
    return () => clearInterval(timer);
  }, [active]);
  return SPINNER[frame % SPINNER.length];
}

export type ButtonVariant = "primary" | "quiet" | "danger";

const VARIANT: Record<ButtonVariant, string> = {
  primary:
    "border-accent bg-accent text-bg hover:bg-fg hover:border-fg disabled:hover:bg-accent disabled:hover:border-accent",
  quiet:
    "border-rule text-fg-dim hover:border-fg-dim hover:text-fg disabled:hover:border-rule disabled:hover:text-fg-dim",
  danger:
    "border-danger text-danger hover:bg-danger hover:text-bg disabled:hover:bg-transparent disabled:hover:text-danger",
};

/**
 * Every action on the page is one of these. The brackets are drawn as text so
 * the control reads as `[ save ]` the way a TUI button does, but the element is
 * an ordinary button or link — mouse, touch and tab order all work, and nothing
 * depends on the page interpreting a keypress.
 */
export function Button({
  children,
  variant = "quiet",
  onClick,
  href,
  type = "button",
  disabled = false,
  pending = false,
  pendingLabel,
  pressed,
  title,
  "aria-label": ariaLabel,
}: {
  children: ReactNode;
  variant?: ButtonVariant;
  onClick?: () => void;
  href?: string;
  type?: "button" | "submit";
  disabled?: boolean;
  pending?: boolean;
  pendingLabel?: string;
  /**
   * A button that is on or off rather than one that does something once —
   * picking who gets an SSH key, or which keys somebody is invited with.
   *
   * It exists because the callers were already trying to say this. Both passed
   * `aria-pressed` straight through and this component's explicit prop list
   * dropped it on the floor, so every toggle in the dashboard changed colour
   * and announced nothing: to a screen reader they were plain buttons, and
   * which people held a key was information only the sighted got.
   */
  pressed?: boolean;
  title?: string;
  "aria-label"?: string;
}) {
  const spinner = useSpinner(pending);
  const className =
    "appearance-none inline-flex max-w-full items-center gap-1.5 border px-2.5 py-1 text-xs tracking-wider uppercase no-underline transition-colors disabled:cursor-not-allowed disabled:opacity-45 " +
    VARIANT[variant];

  const label = (
    <>
      <span aria-hidden="true" className="opacity-50">
        [
      </span>
      {pending && (
        <span aria-hidden="true" className="w-[1ch] text-center">
          {spinner}
        </span>
      )}
      <span className="min-w-0 wrap-value">
        {pending && pendingLabel !== undefined ? pendingLabel : children}
      </span>
      <span aria-hidden="true" className="opacity-50">
        ]
      </span>
    </>
  );

  if (href !== undefined && !disabled && !pending) {
    return (
      <a href={href} className={className} title={title} aria-label={ariaLabel}>
        {label}
      </a>
    );
  }

  return (
    <button
      type={type}
      className={className}
      onClick={onClick}
      disabled={disabled || pending}
      aria-busy={pending || undefined}
      aria-pressed={pressed}
      title={title}
      aria-label={ariaLabel}
    >
      {label}
    </button>
  );
}

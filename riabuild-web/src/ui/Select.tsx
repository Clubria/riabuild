import { ReactNode, useId } from "react";
import { CONTROL_CLASS, FieldShell } from "./Field";

export function Select({
  label,
  value,
  options,
  onChange,
  hint,
  disabled = false,
  compact = false,
}: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
  hint?: ReactNode;
  disabled?: boolean;
  /** Hides the visible label for use inside a table row, keeping it for screen readers. */
  compact?: boolean;
}) {
  const id = useId();
  const describedBy = `${id}-hint`;

  /*
   * The shell exists only to hang the caret on. `appearance-none` takes the
   * native arrow with the native widget, and a select with no arrow does not
   * read as a control at all. It stays a real `<select>` underneath — the
   * browser's keyboard handling, typeahead and mobile picker are worth more
   * than a styleable option list.
   */
  const control = (
    <span className={`select-shell ${compact ? "inline-block" : "block"}`}>
      <select
        id={id}
        className={`${CONTROL_CLASS} ${compact ? "w-auto min-w-[12ch] py-0.5" : ""}`}
        value={value}
        disabled={disabled}
        aria-describedby={hint !== undefined ? describedBy : undefined}
        aria-label={compact ? label : undefined}
        onChange={(event) => onChange(event.target.value)}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    </span>
  );

  if (compact) return control;

  return (
    <FieldShell
      label={label}
      hint={hint}
      htmlFor={id}
      describedBy={describedBy}
    >
      {control}
    </FieldShell>
  );
}

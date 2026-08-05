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

  const control = (
    <select
      id={id}
      className={`${CONTROL_CLASS} ${compact ? "w-auto py-0.5" : ""}`}
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

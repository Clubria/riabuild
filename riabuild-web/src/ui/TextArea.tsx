import { ReactNode, useId } from "react";
import { CONTROL_CLASS, FieldShell } from "./Field";

export function TextArea({
  label,
  value,
  onChange,
  rows = 10,
  hint,
  error = null,
  placeholder,
  disabled = false,
  spellCheck = false,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  rows?: number;
  hint?: ReactNode;
  error?: string | null;
  placeholder?: string;
  disabled?: boolean;
  spellCheck?: boolean;
}) {
  const id = useId();
  const describedBy = `${id}-hint`;
  const invalid = error !== null;
  return (
    <FieldShell
      label={label}
      hint={hint}
      error={error}
      htmlFor={id}
      describedBy={describedBy}
    >
      <textarea
        id={id}
        className={`${CONTROL_CLASS} resize-y leading-relaxed`}
        rows={rows}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        spellCheck={spellCheck}
        aria-invalid={invalid || undefined}
        aria-describedby={
          hint !== undefined || invalid ? describedBy : undefined
        }
        onChange={(event) => onChange(event.target.value)}
      />
    </FieldShell>
  );
}

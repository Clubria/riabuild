import { ReactNode, useId } from "react";

/**
 * `appearance-none` is first and is not optional. `color-scheme: dark` makes the
 * browser draw its own dark widget — rounded, its own fill, its own arrow — and
 * without this it renders *inside* our flat rule border rather than instead of
 * it. One native control is enough to stop the page being a terminal.
 */
export const CONTROL_CLASS =
  "appearance-none w-full min-w-0 border border-rule bg-bg-sunk px-2 py-1.5 text-fg placeholder:text-fg-faint focus:border-accent disabled:opacity-50 aria-[invalid=true]:border-danger";

export function FieldShell({
  label,
  hint,
  error,
  htmlFor,
  describedBy,
  children,
}: {
  label: string;
  hint?: ReactNode;
  error?: string | null;
  htmlFor: string;
  describedBy: string;
  children: ReactNode;
}) {
  return (
    <div className="min-w-0">
      <label
        htmlFor={htmlFor}
        className="mb-1 block text-xs tracking-wider text-fg-dim uppercase"
      >
        {label}
      </label>
      {children}
      {(hint !== undefined || (error !== undefined && error !== null)) && (
        <p
          id={describedBy}
          className={`mt-1 text-xs wrap-value ${
            error !== undefined && error !== null ? "text-danger" : "text-fg-faint"
          }`}
        >
          {error !== undefined && error !== null ? error : hint}
        </p>
      )}
    </div>
  );
}

/**
 * A labelled input. The label is always a real `<label>` bound by id — a dim
 * placeholder standing in for a label disappears the moment someone types.
 */
export function Field({
  label,
  value,
  onChange,
  type = "text",
  hint,
  error = null,
  placeholder,
  autoComplete,
  disabled = false,
  required = false,
  spellCheck,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  type?: string;
  hint?: ReactNode;
  error?: string | null;
  placeholder?: string;
  autoComplete?: string;
  disabled?: boolean;
  required?: boolean;
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
      <input
        id={id}
        className={CONTROL_CLASS}
        type={type}
        value={value}
        placeholder={placeholder}
        autoComplete={autoComplete}
        disabled={disabled}
        required={required}
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

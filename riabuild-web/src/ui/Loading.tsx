/**
 * The waiting state, drawn as a prompt that has not answered yet. Announced
 * politely so a screen reader says something is in flight rather than nothing.
 */
export function Loading({ label = "loading" }: { label?: string }) {
  return (
    <p
      role="status"
      aria-live="polite"
      className="flex items-center gap-2 text-fg-dim"
    >
      <span aria-hidden="true" className="text-fg-faint">
        $
      </span>
      <span>{label}</span>
      <span aria-hidden="true" className="cursor text-accent" />
    </p>
  );
}

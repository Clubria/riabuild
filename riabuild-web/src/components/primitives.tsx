import { ReactNode, useState } from "react";

export function Chip({
  tone,
  children,
}: {
  tone: "ink" | "signal" | "verified" | "muted";
  children: ReactNode;
}) {
  const color = {
    ink: "text-ink",
    signal: "text-signal",
    verified: "text-verified",
    muted: "text-muted",
  }[tone];
  return <span className={`chip ${color}`}>{children}</span>;
}

/**
 * A numbered step. The numbering is not decoration: onboarding is a sequence,
 * and the number is what a developer refers to when they get stuck on one.
 */
export function Step({
  index,
  title,
  children,
  delayMs = 0,
}: {
  index: string;
  title: string;
  children: ReactNode;
  delayMs?: number;
}) {
  return (
    <section
      className="step-in grid grid-cols-[3rem_1fr] gap-x-4 border-t border-rule py-7 sm:grid-cols-[5rem_1fr] sm:gap-x-8"
      style={{ animationDelay: `${delayMs}ms` }}
    >
      <div className="mono pt-1 text-ink">{index}</div>
      <div>
        <h2 className="eyebrow mb-3">{title}</h2>
        {children}
      </div>
    </section>
  );
}

export function Field({
  label,
  value,
  onChange,
  type = "text",
  autoComplete,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  type?: string;
  autoComplete?: string;
}) {
  return (
    <label className="block">
      <span className="eyebrow mb-1 block">{label}</span>
      <input
        className="field"
        type={type}
        value={value}
        autoComplete={autoComplete}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

/** Copy targets exist so nobody retypes a command and mistypes the tap name. */
export function CommandLine({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="flex items-stretch gap-0 border border-rule bg-paper-sunk">
      <code className="mono flex-1 overflow-x-auto px-3 py-2.5 text-graphite">
        <span className="text-muted select-none">$ </span>
        {command}
      </code>
      <button
        className="btn btn-quiet border-0 border-l border-rule"
        onClick={() => {
          void navigator.clipboard.writeText(command).then(
            () => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1600);
            },
            () => setCopied(false),
          );
        }}
      >
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

export function Notice({
  tone,
  title,
  children,
}: {
  tone: "signal" | "ink";
  title: string;
  children?: ReactNode;
}) {
  const border = tone === "signal" ? "border-signal" : "border-ink";
  const text = tone === "signal" ? "text-signal" : "text-ink";
  return (
    <div className={`border-l-2 ${border} bg-paper-sunk px-4 py-3`}>
      <p className={`eyebrow ${text} mb-1`}>{title}</p>
      {children}
    </div>
  );
}

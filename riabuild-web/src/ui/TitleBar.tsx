import { ReactNode } from "react";

export type Tab = {
  id: string;
  label: string;
  href: string;
};

/**
 * The window chrome. Tabs are real anchors — on the dashboard they jump to a
 * section that genuinely exists. A tab strip that only looks navigable would be
 * the same lie as a keybinding hint we never handle.
 */
export function TitleBar({
  title,
  subtitle,
  tabs,
  active,
  actions,
}: {
  title: string;
  subtitle?: string;
  tabs?: Tab[];
  active?: string;
  actions?: ReactNode;
}) {
  return (
    <header className="border-b border-rule bg-bg-raised">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-3 py-2 sm:px-4">
        <span className="flex items-center gap-2">
          <span aria-hidden="true" className="text-fg-faint tracking-widest">
            ●●●
          </span>
          <a
            href="/"
            className="font-bold text-accent no-underline hover:underline"
          >
            {title}
          </a>
          {subtitle !== undefined && (
            <span className="hidden text-fg-faint sm:inline">— {subtitle}</span>
          )}
        </span>

        {tabs !== undefined && tabs.length > 0 && (
          <nav aria-label="Sections" className="flex flex-wrap gap-1">
            {tabs.map((tab) => {
              const isActive = tab.id === active;
              return (
                <a
                  key={tab.id}
                  href={tab.href}
                  aria-current={isActive ? "page" : undefined}
                  className={
                    "px-2 py-0.5 no-underline " +
                    (isActive
                      ? "bg-accent text-bg"
                      : "text-fg-dim hover:bg-bg hover:text-fg")
                  }
                >
                  {tab.label}
                </a>
              );
            })}
          </nav>
        )}

        {actions !== undefined && (
          <span className="ml-auto flex items-center gap-2">{actions}</span>
        )}
      </div>
    </header>
  );
}

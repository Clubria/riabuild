import { ReactNode } from "react";
import { Tab, TitleBar } from "./TitleBar";
import { StatusBar } from "./StatusBar";

/**
 * The terminal window everything lives inside. Three rows: chrome, a scrolling
 * body, a pinned status line. The body scrolls rather than the document so the
 * status line stays where a terminal's would be.
 */
export function Screen({
  title,
  subtitle,
  tabs,
  activeTab,
  actions,
  statusLeft,
  statusRight,
  children,
}: {
  title: string;
  subtitle?: string;
  tabs?: Tab[];
  activeTab?: string;
  actions?: ReactNode;
  statusLeft?: ReactNode;
  statusRight?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="min-h-dvh bg-bg-sunk sm:p-3">
      <div className="mx-auto flex min-h-dvh max-w-5xl flex-col border-rule bg-bg sm:min-h-[calc(100dvh-1.5rem)] sm:border">
        <TitleBar
          title={title}
          subtitle={subtitle}
          tabs={tabs}
          active={activeTab}
          actions={actions}
        />
        <main className="min-w-0 flex-1 overflow-x-hidden px-3 py-5 sm:px-5 sm:py-6">
          {children}
        </main>
        <StatusBar left={statusLeft} right={statusRight} />
      </div>
    </div>
  );
}

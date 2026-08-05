import { ReactNode } from "react";
import { DataContext } from "../data/context";
import { Data } from "../data/types";
import { SCENARIOS, SCENARIO_NAMES } from "./scenarios";
import { scenarioName } from "./scenarioName";
import { Alert, Panel } from "../ui";

/**
 * Feeds the app fixtures instead of Convex. Dev builds only — `main.tsx` reaches
 * this module through a dynamic import guarded by `import.meta.env.DEV`, so it
 * is never in a production bundle.
 *
 * An unknown scenario name is a loud failure rather than a silent fall-through
 * to the real backend: a typo in a test URL that quietly renders live data is a
 * green suite that proved nothing.
 */
export function DevDataProvider({ children }: { children: ReactNode }) {
  const name = scenarioName();
  const build = name === null ? undefined : SCENARIOS[name];

  if (build === undefined) {
    return (
      <div className="min-h-dvh bg-bg-sunk p-4">
        <Panel title="unknown scenario" tone="danger" index="dev">
          <Alert tone="danger" title={`No fixture named "${name ?? ""}"`}>
            <p>Known scenarios:</p>
            <ul className="mt-2 grid gap-0.5 sm:grid-cols-2">
              {SCENARIO_NAMES.map((known) => (
                <li key={known}>
                  <a
                    className="text-accent"
                    href={`?scenario=${encodeURIComponent(known)}`}
                  >
                    {known}
                  </a>
                </li>
              ))}
            </ul>
          </Alert>
        </Panel>
      </div>
    );
  }

  // Thrown here rather than swallowed, so the `boom` scenario reaches the error
  // boundary the same way a real render failure would.
  const data: Data = build();

  return <DataContext.Provider value={data}>{children}</DataContext.Provider>;
}

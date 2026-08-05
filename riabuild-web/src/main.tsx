import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ConvexAuthProvider } from "@convex-dev/auth/react";
import { ConvexReactClient } from "convex/react";
import "./index.css";
import App from "./App.tsx";
import { ConvexDataProvider } from "./data/convexProvider";
import { DataContext } from "./data/context";
import { offlineData } from "./data/offlineData";
import { ErrorBoundary } from "./app/ErrorBoundary";
import { scenarioName } from "./dev/scenarioName";

const root = createRoot(document.getElementById("root")!);

/**
 * Three data sources, chosen once at boot.
 *
 * The fixture provider is behind a dynamic import so it lands in its own chunk
 * and is never fetched by a production visitor. `import.meta.env.DEV` is a
 * compile-time constant, so in a production build that branch and the chunk it
 * references are eliminated entirely — the fixtures cannot ship.
 */
if (import.meta.env.DEV && scenarioName() !== null) {
  void import("./dev/DevDataProvider").then(({ DevDataProvider }) => {
    root.render(
      <StrictMode>
        <ErrorBoundary>
          <DevDataProvider>
            <App />
          </DevDataProvider>
        </ErrorBoundary>
      </StrictMode>,
    );
  });
} else {
  root.render(
    <StrictMode>
      <ErrorBoundary>{live()}</ErrorBoundary>
    </StrictMode>,
  );
}

/**
 * Constructing the Convex client throws on a missing or malformed URL, and it
 * happens before React mounts — so an unconfigured deployment renders a blank
 * page rather than any of the error screens built for exactly that situation.
 * Failing over to offline data keeps the 404, the error boundary and the shell
 * alive and says what is wrong.
 */
function live() {
  const url = import.meta.env.VITE_CONVEX_URL as string | undefined;
  let convex: ConvexReactClient;
  try {
    if (typeof url !== "string" || url === "") {
      throw new Error("VITE_CONVEX_URL is not set for this build.");
    }
    convex = new ConvexReactClient(url);
  } catch (cause) {
    const message =
      cause instanceof Error ? cause.message : "Could not reach the backend.";
    console.error("riabuild: no Convex client —", message);
    return (
      <DataContext.Provider value={offlineData(message)}>
        <App />
      </DataContext.Provider>
    );
  }

  return (
    <ConvexAuthProvider client={convex}>
      <ConvexDataProvider>
        <App />
      </ConvexDataProvider>
    </ConvexAuthProvider>
  );
}

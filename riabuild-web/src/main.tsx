import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ConvexAuthProvider } from "@convex-dev/auth/react";
import { ConvexReactClient } from "convex/react";
import "./index.css";
import App from "./App.tsx";
import { ConvexDataProvider } from "./data/convexProvider";
import { ErrorBoundary } from "./app/ErrorBoundary";
import { scenarioName } from "./dev/scenarioName";

const root = createRoot(document.getElementById("root")!);

/**
 * Two data sources, chosen once at boot.
 *
 * The fixture provider is behind a dynamic import so it lands in its own chunk
 * and is never fetched by a production visitor. `import.meta.env.DEV` is a
 * compile-time constant, so in a production build this branch and the chunk it
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
  const convex = new ConvexReactClient(
    import.meta.env.VITE_CONVEX_URL as string,
  );
  root.render(
    <StrictMode>
      <ErrorBoundary>
        <ConvexAuthProvider client={convex}>
          <ConvexDataProvider>
            <App />
          </ConvexDataProvider>
        </ConvexAuthProvider>
      </ErrorBoundary>
    </StrictMode>,
  );
}

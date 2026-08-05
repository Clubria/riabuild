import { lazy, Suspense } from "react";
import { useData } from "./data/context";
import { route } from "./app/route";
import { ErrorBoundary } from "./app/ErrorBoundary";
import { Dashboard, DASHBOARD_TABS } from "./routes/Dashboard";
import { CliAuthorize } from "./routes/CliAuthorize";
import { NotFound } from "./routes/NotFound";
import { SignIn } from "./components/SignIn";
import { Button, Dot, Loading, Screen } from "./ui";

/**
 * The gallery imports the fixtures, so it must not merely be unreachable in
 * production — it must not be emitted. `import.meta.env.DEV` is a compile-time
 * constant, so this ternary collapses at build time and the dynamic import
 * disappears with the dead branch. Verified by the fixture-leak check in CI:
 *
 *   grep -l "Fixture scenario" dist/assets/*.js   # must match nothing
 */
const Gallery = import.meta.env.DEV
  ? lazy(() => import("./routes/Gallery").then((m) => ({ default: m.Gallery })))
  : lazy(() => Promise.resolve({ default: () => null }));

export default function App() {
  const data = useData();
  const current = route(window.location.pathname);

  const viewer =
    data.viewer.state === "ready" ? data.viewer.value : null;

  return (
    <Screen
      title="riabuild"
      subtitle={subtitleFor(current.kind)}
      tabs={current.kind === "dashboard" && viewer !== null ? DASHBOARD_TABS(viewer.role === "lead") : undefined}
      actions={
        data.auth === "signed-in" ? (
          <Button variant="quiet" onClick={() => void data.signOut()}>
            sign out
          </Button>
        ) : undefined
      }
      statusLeft={<StatusLeft />}
      statusRight={<span>riabuild.clubria.com</span>}
    >
      <ErrorBoundary>
        <Body kind={current.kind} path={pathOf(current)} />
      </ErrorBoundary>
    </Screen>
  );
}

function pathOf(current: ReturnType<typeof route>): string {
  return current.kind === "notFound" ? current.path : "/";
}

function subtitleFor(kind: ReturnType<typeof route>["kind"]): string {
  switch (kind) {
    case "dashboard":
      return "clubria provisioner";
    case "authorize":
      return "device authorisation";
    case "gallery":
      return "component gallery";
    case "notFound":
      return "clubria provisioner";
  }
}

function Body({
  kind,
  path,
}: {
  kind: ReturnType<typeof route>["kind"];
  path: string;
}) {
  const data = useData();

  if (kind === "notFound") return <NotFound path={path} />;
  if (kind === "gallery") {
    return (
      <Suspense fallback={<Loading label="loading gallery" />}>
        <Gallery />
      </Suspense>
    );
  }

  if (data.auth === "loading") {
    return <Loading label="connecting to riabuild" />;
  }

  if (data.auth === "signed-out") {
    return (
      <SignIn
        heading={
          kind === "authorize"
            ? "Sign in to approve this machine."
            : "Set up your Clubria machine."
        }
        redirectTo={
          kind === "authorize"
            ? window.location.pathname + window.location.search
            : undefined
        }
      />
    );
  }

  if (kind === "authorize") return <CliAuthorize />;

  if (data.viewer.state === "loading") {
    return <Loading label="checking your access" />;
  }
  if (data.viewer.state === "error") {
    return <Loading label={data.viewer.message} />;
  }
  if (data.viewer.value === null) {
    return (
      <Loading label="your riabuild account is still being created — reload in a moment" />
    );
  }

  return <Dashboard member={data.viewer.value} />;
}

/** State only. Never a keybinding hint — the page handles no keystrokes. */
function StatusLeft() {
  const data = useData();

  if (data.auth !== "signed-in") {
    return <Dot tone="muted" label="signed out" />;
  }

  const viewer = data.viewer.state === "ready" ? data.viewer.value : null;
  const membership = data.membership;

  return (
    <>
      {viewer !== null && (
        // A GitHub login can be 39 characters and the status bar is the
        // narrowest thing on the page; without wrapping it pushes the document
        // sideways at 380px.
        <span className="min-w-0 text-fg-dim wrap-value">
          @{viewer.githubLogin}
        </span>
      )}
      {viewer !== null && (
        <span className="min-w-0 text-accent wrap-value">{viewer.role}</span>
      )}
      {membership.status === "member" && <Dot tone="ok" label={membership.org} />}
      {membership.status === "checking" && (
        <Dot tone="muted" label="checking github" />
      )}
      {membership.status === "not_member" && (
        <Dot tone="danger" label={`not in ${membership.org}`} />
      )}
      {membership.status === "unavailable" && (
        <Dot tone="warn" label="github unreachable" />
      )}
    </>
  );
}

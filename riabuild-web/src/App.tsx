import { Authenticated, Unauthenticated, useQuery } from "convex/react";
import { useAuthActions } from "@convex-dev/auth/react";
import { api } from "../convex/_generated/api";
import { SignIn } from "./components/SignIn";
import { Dashboard } from "./routes/Dashboard";
import { CliAuthorize } from "./routes/CliAuthorize";
import { useOrgMembership } from "./useOrgMembership";

/**
 * Two routes and no router. `/cli/authorize` is reached only by the CLI opening
 * a browser, and everything else is the dashboard — a routing library would be
 * more moving parts than the product has destinations.
 */
export default function App() {
  const isAuthorizeRoute = window.location.pathname === "/cli/authorize";
  return (
    <div className="mx-auto min-h-screen max-w-4xl px-5 pb-24 sm:px-8">
      <Masthead />
      <Unauthenticated>
        <div className="py-10">
          <h1 className="display mb-6 text-4xl sm:text-5xl">
            {isAuthorizeRoute
              ? "Sign in to approve this machine."
              : "Set up your Clubria machine."}
          </h1>
          <SignIn
            redirectTo={
              isAuthorizeRoute
                ? window.location.pathname + window.location.search
                : undefined
            }
          />
        </div>
      </Unauthenticated>
      <Authenticated>
        {isAuthorizeRoute ? <CliAuthorize signedIn={true} /> : <SignedInHome />}
      </Authenticated>
    </div>
  );
}

function SignedInHome() {
  const member = useQuery(api.members.viewer);
  const membership = useOrgMembership();

  if (member === undefined || membership.status === "checking") {
    return <p className="mono py-10 text-muted">Checking your access…</p>;
  }
  if (member === null) {
    return (
      <p className="py-10 text-muted">
        Your riabuild account is still being created. Reload in a moment.
      </p>
    );
  }
  return <Dashboard member={member} membership={membership} />;
}

function Masthead() {
  const { signOut } = useAuthActions();
  return (
    <header className="flex items-baseline justify-between gap-4 border-b border-rule py-5">
      <a href="/" className="no-underline">
        <span className="display text-xl tracking-tight">riabuild</span>
        <span className="eyebrow ml-3">Clubria provisioner</span>
      </a>
      <Authenticated>
        <button className="btn btn-quiet" onClick={() => void signOut()}>
          Sign out
        </button>
      </Authenticated>
    </header>
  );
}

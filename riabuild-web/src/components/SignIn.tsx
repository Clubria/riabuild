import { useState } from "react";
import { useData } from "../data/context";
import { Alert, Button, Panel } from "../ui";

/**
 * The only door. GitHub org membership is the invite, so there is nothing to
 * type here and no account to create.
 *
 * The dev sign-in button exists only in dev builds, and only works if the
 * deployment also sets `RIABUILD_DEV_AUTH=1`. Two independent gates, because
 * one of them shipping by accident should still leave the door shut.
 */
export function SignIn({
  heading,
  redirectTo,
}: {
  heading: string;
  redirectTo?: string;
}) {
  const data = useData();
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  function start() {
    setPending(true);
    setError(null);
    void data.signIn(redirectTo !== undefined ? { redirectTo } : undefined).catch(
      (cause: unknown) => {
        setPending(false);
        setError(cause instanceof Error ? cause.message : "Sign-in failed.");
      },
    );
  }

  return (
    <div className="mx-auto max-w-xl py-4">
      <p className="mb-1 text-fg-faint">
        <span aria-hidden="true">$ </span>riabuild login
      </p>
      <h1 className="mb-5 text-xl text-fg sm:text-2xl">{heading}</h1>

      <Panel title="authenticate">
        <p className="max-w-prose text-fg-dim">
          riabuild uses your GitHub account. If you have accepted the invite to
          the Clubria organisation, you are already in.
        </p>
        <div className="mt-5 flex flex-wrap gap-2">
          <Button
            variant="primary"
            pending={pending}
            pendingLabel="opening github"
            onClick={start}
          >
            sign in with github
          </Button>
        </div>
        {error !== null && (
          <div className="mt-5">
            <Alert tone="danger" title="Sign-in failed">
              <p className="wrap-value">{error}</p>
            </Alert>
          </div>
        )}
      </Panel>

      {import.meta.env.DEV && data.devSignIn !== undefined && (
        <div className="mt-6">
          <Panel title="dev sign-in" tone="warn" index="dev">
            <p className="max-w-prose text-fg-dim">
              Only in dev builds, and only works if the deployment sets{" "}
              <span className="text-fg">RIABUILD_DEV_AUTH=1</span>. Whether an
              account is a lead still comes from{" "}
              <span className="text-fg">RIABUILD_BOOTSTRAP_LEADS</span>.
            </p>
            <div className="mt-4 flex flex-wrap gap-2">
              {["devlead", "devuser"].map((login) => (
                <Button
                  key={login}
                  variant="quiet"
                  onClick={() => {
                    setError(null);
                    void data.devSignIn?.(login).catch((cause: unknown) =>
                      setError(
                        cause instanceof Error
                          ? cause.message
                          : "Dev sign-in failed.",
                      ),
                    );
                  }}
                >
                  sign in as {login}
                </Button>
              ))}
            </div>
          </Panel>
        </div>
      )}
    </div>
  );
}

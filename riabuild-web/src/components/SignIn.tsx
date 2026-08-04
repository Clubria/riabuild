import { useAuthActions } from "@convex-dev/auth/react";
import { useState } from "react";
import { Notice } from "./primitives";

/**
 * The only door. GitHub org membership is the invite, so there is nothing to
 * type here and no account to create.
 */
export function SignIn({ redirectTo }: { redirectTo?: string }) {
  const { signIn } = useAuthActions();
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  return (
    <div className="max-w-xl">
      <p className="mb-6">
        riabuild uses your GitHub account. If you have accepted the invite to the
        Clubria organisation, you are already in.
      </p>
      <button
        className="btn"
        disabled={pending}
        onClick={() => {
          setPending(true);
          setError(null);
          void signIn("github", redirectTo ? { redirectTo } : {}).catch(
            (cause: unknown) => {
              setPending(false);
              setError(
                cause instanceof Error ? cause.message : "Sign-in failed.",
              );
            },
          );
        }}
      >
        {pending ? "Opening GitHub…" : "Sign in with GitHub"}
      </button>
      {error !== null && (
        <div className="mt-6">
          <Notice tone="signal" title="Sign-in failed">
            <p className="mono">{error}</p>
          </Notice>
        </div>
      )}
    </div>
  );
}

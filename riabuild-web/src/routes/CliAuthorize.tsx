import { useAction } from "convex/react";
import { useState } from "react";
import { api } from "../../convex/_generated/api";
import { Notice } from "../components/primitives";

type Params = {
  state: string;
  challenge: string;
  port: number;
  label: string;
  version: string;
};

/**
 * Loopback approval, the shape `gh` uses. The CLI is listening on an ephemeral
 * port on this machine; approving sends the browser back to it with a one-time
 * code. `state` is passed through untouched so the CLI can reject a callback it
 * did not start.
 */
export function CliAuthorize({ signedIn }: { signedIn: boolean }) {
  const authorize = useAction(api.cliAuth.authorize);
  const [error, setError] = useState<string | null>(null);
  const [handedOff, setHandedOff] = useState(false);
  const [pending, setPending] = useState(false);

  const params = readParams();

  if (params === null) {
    return (
      <div className="max-w-xl py-10">
        <Notice tone="signal" title="Nothing to approve">
          <p>
            This page is opened by the riabuild CLI. Run{" "}
            <span className="mono">riabuild</span> in your terminal instead.
          </p>
        </Notice>
      </div>
    );
  }

  if (handedOff) {
    return (
      <div className="max-w-xl py-10">
        <h1 className="display mb-4 text-3xl">Back to your terminal.</h1>
        <p className="text-muted">
          riabuild has what it needs. You can close this tab.
        </p>
      </div>
    );
  }

  return (
    <div className="max-w-xl py-10">
      <p className="eyebrow mb-4">Device authorisation</p>
      <h1 className="display text-3xl sm:text-4xl">
        Sign this machine
        <br />
        in to riabuild?
      </h1>

      <dl className="mono mt-8 grid grid-cols-[7rem_1fr] gap-y-2 border-y border-rule py-4">
        <dt className="text-muted">device</dt>
        <dd className="text-graphite">{params.label}</dd>
        <dt className="text-muted">riabuild</dt>
        <dd className="text-graphite">v{params.version}</dd>
        <dt className="text-muted">callback</dt>
        <dd className="text-graphite">127.0.0.1:{params.port}</dd>
      </dl>

      <p className="mt-5 text-muted">
        Approving grants this machine a token that expires in 90 days. You can
        revoke it from the dashboard at any time.
      </p>

      {!signedIn ? (
        <p className="mt-6">Sign in above to continue.</p>
      ) : (
        <div className="mt-6 flex flex-wrap gap-3">
          <button
            className="btn"
            disabled={pending}
            onClick={() => {
              setPending(true);
              setError(null);
              void authorize({
                challenge: params.challenge,
                deviceLabel: params.label,
                cliVersion: params.version,
              })
                .then((result) => {
                  setHandedOff(true);
                  window.location.href =
                    `http://127.0.0.1:${params.port}/callback` +
                    `?code=${encodeURIComponent(result.code)}` +
                    `&state=${encodeURIComponent(params.state)}`;
                })
                .catch((cause: unknown) => {
                  setPending(false);
                  setError(
                    cause instanceof Error
                      ? cause.message
                          .replace(/^.*Uncaught Error:\s*/, "")
                          .split("\n")[0]
                      : "Authorisation failed.",
                  );
                });
            }}
          >
            {pending ? "Approving…" : "Approve this machine"}
          </button>
          <a className="btn btn-quiet" href="/">
            Cancel
          </a>
        </div>
      )}

      {error !== null && (
        <div className="mt-6">
          <Notice tone="signal" title="Not approved">
            <p>{error}</p>
          </Notice>
        </div>
      )}
    </div>
  );
}

/**
 * The port is the only value that turns into a URL, so it is the only one that
 * needs proving. Anything but a plain high port number and this page refuses to
 * send a code anywhere.
 */
function readParams(): Params | null {
  const query = new URLSearchParams(window.location.search);
  const state = query.get("state") ?? "";
  const challenge = query.get("challenge") ?? "";
  const port = Number(query.get("port") ?? "");

  if (state.length < 16 || challenge.length < 32) return null;
  if (!Number.isInteger(port) || port < 1024 || port > 65535) return null;

  return {
    state,
    challenge,
    port,
    label: (query.get("label") ?? "this machine").slice(0, 80),
    version: (query.get("version") ?? "unknown").slice(0, 32),
  };
}

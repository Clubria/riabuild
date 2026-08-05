import { useState } from "react";
import { useData } from "../data/context";
import { readError } from "../lib/errors";
import { Alert, Button, KeyValue, Panel } from "../ui";

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
export function CliAuthorize() {
  const data = useData();
  const [error, setError] = useState<string | null>(null);
  const [handedOff, setHandedOff] = useState(false);
  const [pending, setPending] = useState(false);

  const params = readParams();

  if (params === null) {
    return (
      <div className="mx-auto max-w-xl py-4">
        <Panel title="nothing to approve" tone="warn" index="!">
          <p className="max-w-prose text-fg-dim">
            This page is opened by the riabuild CLI. Run{" "}
            <span className="text-fg">riabuild</span> in your terminal instead.
          </p>
          <div className="mt-5">
            <Button variant="quiet" href="/">
              cd /
            </Button>
          </div>
        </Panel>
      </div>
    );
  }

  if (handedOff) {
    return (
      <div className="mx-auto max-w-xl py-4">
        <Panel title="approved" tone="ok" index="ok">
          <p className="text-fg">Back to your terminal.</p>
          <p className="mt-2 max-w-prose text-fg-dim">
            riabuild has what it needs. You can close this tab.
          </p>
        </Panel>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-xl py-4">
      <p className="mb-1 text-fg-faint">
        <span aria-hidden="true">$ </span>riabuild login
      </p>
      <h1 className="mb-5 text-xl text-fg sm:text-2xl">
        Sign this machine in to riabuild?
      </h1>

      <Panel title="device">
        <KeyValue
          rows={[
            { label: "device", value: params.label },
            { label: "riabuild", value: `v${params.version}` },
            { label: "callback", value: `127.0.0.1:${params.port}` },
          ]}
        />
        <p className="mt-5 max-w-prose text-fg-dim">
          Approving grants this machine a token that expires in 90 days. You can
          revoke it from the dashboard at any time.
        </p>

        <div className="mt-5 flex flex-wrap gap-2">
          <Button
            variant="primary"
            pending={pending}
            pendingLabel="approving"
            onClick={() => {
              setPending(true);
              setError(null);
              void data
                .authorizeCli({
                  challenge: params.challenge,
                  deviceLabel: params.label,
                  cliVersion: params.version,
                })
                .then((result) => {
                  setHandedOff(true);
                  data.handOffToCli(
                    `http://127.0.0.1:${params.port}/callback` +
                      `?code=${encodeURIComponent(result.code)}` +
                      `&state=${encodeURIComponent(params.state)}`,
                  );
                })
                .catch((cause: unknown) => {
                  setPending(false);
                  setError(readError(cause, "Authorisation failed."));
                });
            }}
          >
            approve this machine
          </Button>
          <Button variant="quiet" href="/" disabled={pending}>
            cancel
          </Button>
        </div>

        {error !== null && (
          <div className="mt-5">
            <Alert tone="danger" title="Not approved">
              <p className="wrap-value">{error}</p>
            </Alert>
          </div>
        )}
      </Panel>
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

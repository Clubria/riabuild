import { useEffect, useState } from "react";
import { useData } from "../data/context";
import { DeviceRequest } from "../data/types";
import { readError } from "../lib/errors";
import { Alert, Button, Field, KeyValue, Panel } from "../ui";

/**
 * Device authorisation — where a developer approves the machine that is asking.
 *
 * The CLI never listens on a socket, so nothing about this page depends on the
 * browser and the terminal being on the same computer. That is the whole point:
 * over SSH the terminal is on a server and the browser is on a laptop.
 *
 * The typed code is what binds the two. Approving is a deliberate act on a
 * screen that names the machine asking, because a link that approved on sight
 * would let anyone who can get a developer to click it sign their own terminal
 * in. The click is the security control, not a formality.
 */
export function CliAuthorize() {
  const data = useData();
  const [typed, setTyped] = useState(prefilledCode);
  const [request, setRequest] = useState<DeviceRequest | null>(null);
  const [outcome, setOutcome] = useState<"approved" | "denied" | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const code = normalise(typed);
  const complete = code.length === CODE_LENGTH;

  // Looking the code up as soon as it is complete, rather than behind a
  // "continue" button: the developer has already committed by typing eight
  // characters, and a second click to see what they typed matches is friction
  // with nothing behind it.
  useEffect(() => {
    if (!complete || outcome !== null) return;
    let current = true;
    data
      .lookupDeviceCode({ userCode: code })
      .then((found) => {
        if (!current) return;
        setRequest(found);
        setError(null);
      })
      .catch((cause: unknown) => {
        if (current) setError(readError(cause, "Could not look that code up."));
      });
    return () => {
      current = false;
    };
    // `data` is rebuilt on every provider render; the code is what identifies
    // the request being looked at.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [code, complete, outcome]);

  if (outcome !== null) {
    return (
      <div className="mx-auto max-w-xl py-4">
        <Panel
          title={outcome === "approved" ? "approved" : "denied"}
          tone={outcome === "approved" ? "ok" : "warn"}
          index={outcome === "approved" ? "ok" : "!"}
        >
          <p className="text-fg">Back to your terminal.</p>
          <p className="mt-2 max-w-prose text-fg-dim">
            {outcome === "approved"
              ? "riabuild is finishing up on that machine. You can close this tab."
              : "That machine was not signed in. Nothing was granted."}
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

  return (
    <div className="mx-auto max-w-xl py-4">
      <p className="mb-1 text-fg-faint">
        <span aria-hidden="true">$ </span>riabuild login
      </p>
      <h1 className="mb-5 text-xl text-fg sm:text-2xl">
        Sign a machine in to riabuild?
      </h1>

      <Panel title="code" index="1">
        <Field
          label="code from your terminal"
          value={typed}
          onChange={(next) => {
            setTyped(group(normalise(next)));
            setRequest(null);
          }}
          placeholder="XXXX-XXXX"
          autoComplete="off"
          spellCheck={false}
          hint="Eight characters, shown by riabuild in the terminal you are signing in."
        />
      </Panel>

      {complete && request !== null && (
        <div className="mt-4">
          <Decision
            request={request}
            now={data.now}
            busy={busy}
            onDecide={(decision) => {
              setBusy(true);
              setError(null);
              const act = (p: { userCode: string }) =>
                decision === "approve"
                  ? data.approveDeviceCode(p)
                  : data.denyDeviceCode(p);
              void act({ userCode: code })
                .then((result) => {
                  setBusy(false);
                  // The request can go stale between reading it and acting on
                  // it — a fifteen-minute expiry passes while someone reads.
                  if (result.status !== "ok") {
                    setRequest({ status: result.status });
                    return;
                  }
                  setOutcome(decision === "approve" ? "approved" : "denied");
                })
                .catch((cause: unknown) => {
                  setBusy(false);
                  setError(readError(cause, "That did not go through."));
                });
            }}
          />
        </div>
      )}

      {error !== null && (
        <div className="mt-4">
          <Alert tone="danger" title="Not approved">
            <p className="wrap-value">{error}</p>
          </Alert>
        </div>
      )}
    </div>
  );
}

function Decision({
  request,
  now,
  busy,
  onDecide,
}: {
  request: DeviceRequest;
  /**
   * The clock as a prop, never `Date.now()`.
   *
   * A component that reads the wall clock cannot be shown holding a fixed
   * moment, so every screenshot of this panel would differ from the last and
   * stop being evidence of anything. `data.now` is the ticking clock the
   * provider owns, and fixtures freeze it.
   */
  now: number;
  busy: boolean;
  onDecide: (decision: "approve" | "deny") => void;
}) {
  if (request.status !== "pending") {
    return (
      <Alert tone="warn" title={DEAD_TITLE[request.status]}>
        <p>{DEAD_DETAIL[request.status]}</p>
      </Alert>
    );
  }

  return (
    <Panel title="machine" index="2">
      <KeyValue
        rows={[
          { label: "device", value: request.deviceLabel },
          { label: "riabuild", value: `v${request.cliVersion}` },
          { label: "asked", value: formatAsked(request.requestedAt, now) },
        ]}
      />
      <p className="mt-5 max-w-prose text-fg-dim">
        Check that against the terminal you are signing in. Approving grants that
        machine a token that expires in 90 days; you can revoke it from the
        dashboard at any time.
      </p>

      <div className="mt-5 flex flex-wrap gap-2">
        <Button
          variant="primary"
          pending={busy}
          pendingLabel="approving"
          onClick={() => onDecide("approve")}
        >
          approve this machine
        </Button>
        <Button variant="quiet" disabled={busy} onClick={() => onDecide("deny")}>
          deny
        </Button>
      </div>
    </Panel>
  );
}

const DEAD_TITLE: Record<"unknown" | "expired" | "used", string> = {
  unknown: "No such code",
  expired: "That code has expired",
  used: "That code has already been answered",
};

const DEAD_DETAIL: Record<"unknown" | "expired" | "used", string> = {
  unknown:
    "Check the code against your terminal. If riabuild has moved on, run `riabuild login` again for a fresh one.",
  expired:
    "Codes last fifteen minutes. Run `riabuild login` again to get a new one.",
  used: "Nothing more to do here — if that machine is still waiting, run `riabuild login` again.",
};

/** Alphabet and length mirror `convex/lib/crypto.ts`. */
const ALPHABET = "BCDFGHJKMNPQRSTVWXZ";
const CODE_LENGTH = 8;

function normalise(input: string): string {
  let out = "";
  for (const character of input.toUpperCase()) {
    if (ALPHABET.includes(character)) out += character;
  }
  return out.slice(0, CODE_LENGTH);
}

/** Groups as the developer types so the box matches the terminal's `XXXX-XXXX`. */
function group(code: string): string {
  if (code.length <= CODE_LENGTH / 2) return code;
  return `${code.slice(0, CODE_LENGTH / 2)}-${code.slice(CODE_LENGTH / 2)}`;
}

/**
 * `verificationUriComplete` lands here. It fills the box and stops — the
 * approval is still a click, on a screen naming the machine.
 */
function prefilledCode(): string {
  const raw = new URLSearchParams(window.location.search).get("code") ?? "";
  return group(normalise(raw));
}

function formatAsked(at: number, now: number): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  return minutes === 1 ? "a minute ago" : `${minutes} minutes ago`;
}

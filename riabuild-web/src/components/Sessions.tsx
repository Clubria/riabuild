import { useState } from "react";
import { useData } from "../data/context";
import { Session } from "../data/types";
import { readError } from "../lib/errors";
import { formatTime } from "../lib/time";
import { Alert, Badge, Button, Column, DataTable, Empty, Loading } from "../ui";

/**
 * Every machine that holds a live riabuild token, and the button that takes it
 * away. Revoking is effective on that machine's next request — there is no
 * cached credential to wait out.
 */
export function Sessions() {
  const data = useData();
  const [error, setError] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);

  if (data.sessions.state === "loading") return <Loading label="loading machines" />;
  if (data.sessions.state === "error") {
    return (
      <Alert tone="danger" title="Could not list your machines">
        <p className="wrap-value">{data.sessions.message}</p>
      </Alert>
    );
  }

  const now = data.now;
  const sessions = data.sessions.value;

  const columns: Column<Session>[] = [
    {
      key: "device",
      header: "device",
      grow: true,
      // A delegated session is the one nobody approved by hand, so this line
      // is the only place a developer could catch one they did not expect.
      // Stated as the mechanism rather than the purpose: these are minted for
      // servers today, and a row that said "server" would be guessing.
      render: (s) => (
        <span className="text-fg wrap-value">
          {s.deviceLabel}
          {s.origin === "delegated" && (
            <span className="block text-fg-dim">
              signed in by another machine
            </span>
          )}
        </span>
      ),
    },
    {
      key: "version",
      header: "cli",
      priority: "wide",
      render: (s) => <span className="text-fg-dim">v{s.cliVersion}</span>,
    },
    {
      key: "lastUsed",
      header: "last used",
      priority: "wide",
      render: (s) => (
        <span className="text-fg-dim">{formatTime(s.lastUsedAt)}</span>
      ),
    },
    {
      key: "state",
      header: "state",
      render: (s) => {
        const state = sessionState(s, now);
        return (
          <Badge tone={state === "active" ? "ok" : "muted"}>{state}</Badge>
        );
      },
    },
  ];

  return (
    <>
      <DataTable
        caption="Machines signed in to riabuild"
        columns={columns}
        rows={sessions}
        rowKey={(s) => s._id}
        renderActions={(s) => {
          const dead = sessionState(s, now) !== "active";
          return (
            <Button
              variant="danger"
              disabled={dead}
              pending={revoking === s._id}
              pendingLabel="revoking"
              aria-label={`Revoke ${s.deviceLabel}`}
              onClick={() => {
                setError(null);
                setRevoking(s._id);
                void data
                  .revokeSession({ sessionId: s._id })
                  .catch((cause: unknown) => setError(readError(cause)))
                  .finally(() => setRevoking(null));
              }}
            >
              revoke
            </Button>
          );
        }}
        empty={
          <Empty glyph="⌁" title="No machines signed in yet.">
            Running <span className="text-fg">riabuild</span> for the first time
            will add one.
          </Empty>
        }
      />
      {error !== null && (
        <div className="mt-4">
          <Alert tone="danger" title="Not revoked">
            <p className="wrap-value">{error}</p>
          </Alert>
        </div>
      )}
    </>
  );
}

function sessionState(
  session: Session,
  now: number,
): "active" | "expired" | "revoked" {
  if (session.revokedAt !== null) return "revoked";
  if (session.expiresAt <= now) return "expired";
  return "active";
}

import { useMutation, useQuery } from "convex/react";
import { api } from "../../convex/_generated/api";
import { Chip } from "./primitives";
import { formatTime, useNow } from "../lib/time";

/**
 * Every machine that holds a live riabuild token, and the button that takes it
 * away. Revoking is effective on that machine's next request — there is no
 * cached credential to wait out.
 */
export function Sessions() {
  const sessions = useQuery(api.sessions.listMine);
  const now = useNow();
  const revoke = useMutation(api.sessions.revoke);

  if (sessions === undefined) {
    return <p className="mono text-muted">Loading sessions…</p>;
  }
  if (sessions.length === 0) {
    return (
      <p className="text-muted">
        No machines signed in yet. Running <code className="mono">riabuild</code>{" "}
        for the first time will add one.
      </p>
    );
  }

  return (
    <ul className="divide-y divide-rule border-y border-rule">
      {sessions.map((session) => {
        const expired = session.expiresAt <= now;
        const revoked = session.revokedAt !== null;
        const dead = expired || revoked;
        return (
          <li
            key={session._id}
            className="flex flex-wrap items-baseline gap-x-4 gap-y-1 py-3"
          >
            <span className="mono flex-1 basis-48 text-graphite">
              {session.deviceLabel}
            </span>
            <span className="mono text-muted">v{session.cliVersion}</span>
            <span className="mono text-muted">
              last used {formatTime(session.lastUsedAt)}
            </span>
            {revoked ? (
              <Chip tone="muted">revoked</Chip>
            ) : expired ? (
              <Chip tone="muted">expired</Chip>
            ) : (
              <Chip tone="verified">active</Chip>
            )}
            <button
              className="btn btn-danger"
              disabled={dead}
              onClick={() => {
                void revoke({ sessionId: session._id });
              }}
            >
              Revoke
            </button>
          </li>
        );
      })}
    </ul>
  );
}

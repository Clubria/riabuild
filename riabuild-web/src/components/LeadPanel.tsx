import { useMutation, useQuery } from "convex/react";
import { useState } from "react";
import { api } from "../../convex/_generated/api";
import { Chip, Notice } from "./primitives";
import { formatTime } from "../lib/time";

type Role = "candidate" | "developer" | "lead";

const ROLE_TONE: Record<Role, "ink" | "verified" | "muted"> = {
  candidate: "muted",
  developer: "ink",
  lead: "verified",
};

export function Members({ viewerId }: { viewerId: string }) {
  const members = useQuery(api.members.list);
  const setRole = useMutation(api.members.setRole);
  const setStatus = useMutation(api.members.setStatus);
  const [error, setError] = useState<string | null>(null);

  if (members === undefined) {
    return <p className="mono text-muted">Loading members…</p>;
  }

  return (
    <div>
      <ul className="divide-y divide-rule border-y border-rule">
        {members.map((member) => {
          const suspended = member.status === "suspended";
          return (
            <li
              key={member._id}
              className="flex flex-wrap items-baseline gap-x-4 gap-y-2 py-3"
            >
              <span className="mono flex-1 basis-40 text-graphite">
                @{member.githubLogin}
              </span>
              <span className="flex-1 basis-48 text-muted">
                {member.firstName} {member.lastName}
              </span>
              {suspended && <Chip tone="signal">suspended</Chip>}
              <Chip tone={ROLE_TONE[member.role]}>{member.role}</Chip>
              <select
                className="field w-auto"
                value={member.role}
                onChange={(event) => {
                  setError(null);
                  void setRole({
                    memberId: member._id,
                    role: event.target.value as Role,
                  }).catch((cause: unknown) => setError(readError(cause)));
                }}
              >
                <option value="candidate">candidate</option>
                <option value="developer">developer</option>
                <option value="lead">lead</option>
              </select>
              <button
                className={suspended ? "btn btn-quiet" : "btn btn-danger"}
                disabled={member._id === viewerId}
                onClick={() => {
                  setError(null);
                  void setStatus({
                    memberId: member._id,
                    status: suspended ? "active" : "suspended",
                  }).catch((cause: unknown) => setError(readError(cause)));
                }}
              >
                {suspended ? "Reactivate" : "Suspend"}
              </button>
            </li>
          );
        })}
      </ul>
      <p className="mono mt-3 text-muted">
        Suspending revokes that person&rsquo;s CLI sessions immediately.
      </p>
      {error !== null && (
        <div className="mt-4">
          <Notice tone="signal" title="Nothing changed">
            <p>{error}</p>
          </Notice>
        </div>
      )}
    </div>
  );
}

export function OrgSettings() {
  const config = useQuery(api.org.get);
  const update = useMutation(api.org.update);
  const [draft, setDraft] = useState<string | null>(null);
  const [repoSlug, setRepoSlug] = useState<string | null>(null);
  const [latestCli, setLatestCli] = useState<string | null>(null);
  const [minCli, setMinCli] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  if (config === undefined) {
    return <p className="mono text-muted">Loading org config…</p>;
  }

  const settings = draft ?? config.claudeSettings;
  const slug = repoSlug ?? config.repoSlug;
  const latest = latestCli ?? config.latestCliVersion;
  const floor = minCli ?? config.minCliVersion;
  const floorIsChanging = floor.trim() !== config.minCliVersion;

  return (
    <div className="max-w-2xl">
      <label className="block">
        <span className="eyebrow mb-1 block">Repository</span>
        <input
          className="field"
          value={slug}
          onChange={(event) => setRepoSlug(event.target.value)}
        />
      </label>

      <label className="mt-4 block">
        <span className="eyebrow mb-1 block">
          Claude Code settings — layered over every profile at launch
        </span>
        <textarea
          className="field h-56 resize-y"
          spellCheck={false}
          value={settings}
          onChange={(event) => setDraft(event.target.value)}
        />
      </label>

      <div className="mt-4 grid gap-4 sm:grid-cols-2">
        <label className="block">
          <span className="eyebrow mb-1 block">
            Latest CLI version — offered as an upgrade
          </span>
          <input
            className="field mono"
            value={latest}
            spellCheck={false}
            placeholder="2026.08.04"
            onChange={(event) => setLatestCli(event.target.value)}
          />
        </label>

        <label className="block">
          <span className="eyebrow mb-1 block">
            Minimum CLI version — refuses to run below this
          </span>
          <input
            className="field mono"
            value={floor}
            spellCheck={false}
            placeholder="2026.08.04"
            onChange={(event) => setMinCli(event.target.value)}
          />
        </label>
      </div>

      {floorIsChanging && (
        <div className="mt-4">
          <Notice tone="signal" title="This blocks people mid-workday">
            <p>
              The floor moves from v{config.minCliVersion} to v{floor.trim()}.
              Anyone on an older riabuild is refused by the API until they
              upgrade — the next command they run stops working, whatever they
              were in the middle of.
            </p>
          </Notice>
        </div>
      )}

      <div className="mt-4 flex flex-wrap items-center gap-3">
        <button
          className="btn"
          onClick={() => {
            setError(null);
            void update({
              claudeSettings: settings,
              repoSlug: slug,
              latestCliVersion: latest.trim(),
              minCliVersion: floor.trim(),
            })
              .then(() => {
                setSaved(true);
                setTimeout(() => setSaved(false), 2000);
              })
              .catch((cause: unknown) => setError(readError(cause)));
          }}
        >
          Save org config
        </button>
        <button
          className="btn btn-quiet"
          onClick={() => {
            setError(null);
            void update({ markSecretsRotated: true }).catch((cause: unknown) =>
              setError(readError(cause)),
            );
          }}
        >
          Mark secrets rotated
        </button>
        {saved && <span className="eyebrow text-verified">Saved</span>}
      </div>
      <p className="mono mt-3 text-muted">
        secrets last rotated {formatTime(config.secretsUpdatedAt)} · saved CLI
        floor v{config.minCliVersion} · saved latest v{config.latestCliVersion}
      </p>
      {error !== null && (
        <div className="mt-4">
          <Notice tone="signal" title="Not saved">
            <p>{error}</p>
          </Notice>
        </div>
      )}
    </div>
  );
}

export function AuditLog() {
  const entries = useQuery(api.members.auditLog, { limit: 40 });
  if (entries === undefined) {
    return <p className="mono text-muted">Loading audit log…</p>;
  }
  if (entries.length === 0) {
    return <p className="text-muted">Nothing has changed yet.</p>;
  }
  return (
    <ul className="divide-y divide-rule border-y border-rule">
      {entries.map((entry) => (
        <li key={entry._id} className="mono flex flex-wrap gap-x-3 py-2">
          <span className="text-muted">{formatTime(entry.at)}</span>
          <span className="text-ink">{entry.action}</span>
          {entry.actorLogin !== null && (
            <span className="text-muted">by @{entry.actorLogin}</span>
          )}
          {entry.subjectLogin !== null &&
            entry.subjectLogin !== entry.actorLogin && (
              <span className="text-muted">on @{entry.subjectLogin}</span>
            )}
          <span className="text-graphite">
            {Object.entries(entry.meta)
              .map(([key, value]) => `${key}=${value}`)
              .join(" ")}
          </span>
        </li>
      ))}
    </ul>
  );
}

function readError(cause: unknown): string {
  if (!(cause instanceof Error)) return "Something went wrong.";
  return cause.message.replace(/^.*Uncaught Error:\s*/, "").split("\n")[0];
}

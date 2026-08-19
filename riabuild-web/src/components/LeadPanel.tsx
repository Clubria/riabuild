import { useState } from "react";
import { useData } from "../data/context";
import { AuditEntry, Member, Role } from "../data/types";
import { readError } from "../lib/errors";
import { formatTime } from "../lib/time";
import {
  Alert,
  Badge,
  Button,
  Column,
  Copyable,
  DataTable,
  Empty,
  Field,
  Loading,
  Select,
  TextArea,
  Tone,
} from "../ui";

const ROLE_TONE: Record<Role, Tone> = {
  candidate: "muted",
  developer: "accent",
  lead: "ok",
};

const ROLE_OPTIONS = [
  { value: "candidate", label: "candidate" },
  { value: "developer", label: "developer" },
  { value: "lead", label: "lead" },
];

export function Members({ viewerId }: { viewerId: string }) {
  const data = useData();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  if (data.members.state === "loading") return <Loading label="loading members" />;
  if (data.members.state === "error") {
    return (
      <Alert tone="danger" title="Could not list members">
        <p className="wrap-value">{data.members.message}</p>
      </Alert>
    );
  }

  const columns: Column<Member>[] = [
    {
      key: "login",
      header: "github",
      grow: true,
      render: (m) => <span className="text-fg">@{m.githubLogin}</span>,
    },
    {
      key: "name",
      header: "name",
      priority: "wide",
      render: (m) => (
        <span className="text-fg-dim">
          {`${m.firstName} ${m.lastName}`.trim() === ""
            ? "—"
            : `${m.firstName} ${m.lastName}`}
        </span>
      ),
    },
    {
      key: "id",
      header: "member id",
      priority: "wide",
      render: (m) => (
        <Copyable value={m.memberId} label={`member id for @${m.githubLogin}`} />
      ),
    },
    {
      key: "state",
      header: "state",
      render: (m) => (
        <span className="inline-flex flex-wrap gap-1.5">
          <Badge tone={ROLE_TONE[m.role]}>{m.role}</Badge>
          {/* The role beside it is a decision, not a live account — an invited
              `lead` has nobody signed in as it. Without this badge that row is
              indistinguishable from somebody who has been here for months. */}
          {m.invited && <Badge tone="warn">invited</Badge>}
          {m.status === "suspended" && <Badge tone="danger">suspended</Badge>}
        </span>
      ),
    },
  ];

  return (
    <>
      <DataTable
        caption="Org members and their roles"
        columns={columns}
        rows={data.members.value}
        rowKey={(m) => m._id}
        renderActions={(m) => {
          const suspended = m.status === "suspended";
          const isSelf = m._id === viewerId;
          return (
            <>
              <Select
                compact
                label={`Role for @${m.githubLogin}`}
                value={m.role}
                options={ROLE_OPTIONS}
                disabled={busy === m._id}
                onChange={(value) => {
                  setError(null);
                  setBusy(m._id);
                  void data
                    .setRole({ memberId: m._id, role: value as Role })
                    .catch((cause: unknown) => setError(readError(cause)))
                    .finally(() => setBusy(null));
                }}
              />
              {/* Withdrawing and suspending are not the same act and must not
                  look like one. Nobody has ever signed in as an invited person,
                  so there is no session to revoke and nothing to keep a row
                  for — while deleting somebody who *has* arrived would leave
                  their live sessions pointing at a member that is gone. */}
              {m.invited ? (
                <Button
                  variant="danger"
                  pending={busy === m._id}
                  pendingLabel="…"
                  aria-label={`Withdraw the invitation for @${m.githubLogin}`}
                  onClick={() => {
                    setError(null);
                    setBusy(m._id);
                    void data
                      .withdrawInvite({ memberId: m._id })
                      .catch((cause: unknown) => setError(readError(cause)))
                      .finally(() => setBusy(null));
                  }}
                >
                  withdraw
                </Button>
              ) : (
                <Button
                  variant={suspended ? "quiet" : "danger"}
                  disabled={isSelf}
                  pending={busy === m._id}
                  pendingLabel="…"
                  title={
                    isSelf ? "You cannot suspend your own account." : undefined
                  }
                  aria-label={`${suspended ? "Reactivate" : "Suspend"} @${m.githubLogin}`}
                  onClick={() => {
                    setError(null);
                    setBusy(m._id);
                    void data
                      .setStatus({
                        memberId: m._id,
                        status: suspended ? "active" : "suspended",
                      })
                      .catch((cause: unknown) => setError(readError(cause)))
                      .finally(() => setBusy(null));
                  }}
                >
                  {suspended ? "reactivate" : "suspend"}
                </Button>
              )}
            </>
          );
        }}
        empty={
          <Empty glyph="⌂" title="Nobody here yet.">
            Members appear the first time they sign in with GitHub &mdash; or as
            soon as you invite one above, which is the earlier of the two.
          </Empty>
        }
      />
      <p className="mt-3 max-w-prose text-xs text-fg-faint">
        Suspending revokes that person&rsquo;s CLI sessions immediately.
        Withdrawing takes back an invitation nobody has claimed yet, and takes
        their issued keys with it.
      </p>
      {error !== null && (
        <div className="mt-4">
          <Alert tone="danger" title="Nothing changed">
            <p className="wrap-value">{error}</p>
          </Alert>
        </div>
      )}
    </>
  );
}

export function OrgSettings() {
  const data = useData();
  const [draft, setDraft] = useState<string | null>(null);
  const [repoSlug, setRepoSlug] = useState<string | null>(null);
  const [latestCli, setLatestCli] = useState<string | null>(null);
  const [minCli, setMinCli] = useState<string | null>(null);
  // `null` is untouched, and is not the same as "". The field is blank on every
  // load because the token is write-only, so sending that blank with an
  // ordinary settings save would wipe the team's token every time a lead
  // changed the repo slug.
  const [ngrokToken, setNgrokToken] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [saving, setSaving] = useState(false);

  if (data.orgConfig.state === "loading") {
    return <Loading label="loading org config" />;
  }
  if (data.orgConfig.state === "error") {
    return (
      <Alert tone="danger" title="Could not load org config">
        <p className="wrap-value">{data.orgConfig.message}</p>
      </Alert>
    );
  }

  const config = data.orgConfig.value;
  const settings = draft ?? config.claudeSettings;
  const slug = repoSlug ?? config.repoSlug;
  const latest = latestCli ?? config.latestCliVersion;
  const floor = minCli ?? config.minCliVersion;
  const floorIsChanging = floor.trim() !== config.minCliVersion;

  return (
    <div className="max-w-2xl">
      <Field
        label="default repository"
        hint="What Enter picks when riabuild asks. Developers can work on any repository they can see."
        value={slug}
        onChange={setRepoSlug}
      />

      <div className="mt-4">
        <TextArea
          label="claude code settings"
          hint="Layered over every profile at launch. Must be valid JSON."
          value={settings}
          rows={12}
          onChange={setDraft}
        />
      </div>

      <div className="mt-4 grid gap-4 sm:grid-cols-2">
        <Field
          label="latest cli version"
          hint="Offered as an upgrade."
          value={latest}
          placeholder="2026.08.04"
          spellCheck={false}
          onChange={setLatestCli}
        />
        <Field
          label="minimum cli version"
          hint="Refuses to run below this."
          value={floor}
          placeholder="2026.08.04"
          spellCheck={false}
          onChange={setMinCli}
        />
      </div>

      <div className="mt-4">
        <Field
          label="ngrok authtoken"
          hint={
            config.ngrokAuthTokenUpdatedAt > 0
              ? `Set ${formatTime(config.ngrokAuthTokenUpdatedAt)}, ending ${config.ngrokAuthTokenHint}. Type a new one to replace it — this box is blank because the token is never shown back.`
              : "Not set, so every developer's ngrok runs unauthenticated. Paste the token from the team's ngrok account."
          }
          value={ngrokToken ?? ""}
          placeholder="2abc…"
          spellCheck={false}
          onChange={setNgrokToken}
        />
        <p className="mt-1 text-xs text-fg-faint wrap-value">
          Every developer&rsquo;s CLI fetches this when they run{" "}
          <span className="text-fg-dim">ngrok</span>, and stores it nowhere. One
          account carries the whole team, so who opened which tunnel is answered
          by the audit log below and not by ngrok.
        </p>
      </div>

      {floorIsChanging && (
        <div className="mt-4">
          <Alert tone="danger" title="This blocks people mid-workday">
            <p className="wrap-value">
              The floor moves from v{config.minCliVersion} to v{floor.trim()}.
              Anyone on an older riabuild is refused by the API until they
              upgrade — the next command they run stops working, whatever they
              were in the middle of.
            </p>
          </Alert>
        </div>
      )}

      <div className="mt-5 flex flex-wrap items-center gap-2">
        <Button
          variant="primary"
          pending={saving}
          pendingLabel="saving"
          onClick={() => {
            setError(null);
            setSaving(true);
            void data
              .updateOrg({
                claudeSettings: settings,
                repoSlug: slug,
                latestCliVersion: latest.trim(),
                minCliVersion: floor.trim(),
                ...(ngrokToken === null
                  ? {}
                  : { ngrokAuthToken: ngrokToken.trim() }),
              })
              .then(() => {
                setNgrokToken(null);
                setSaved(true);
                setTimeout(() => setSaved(false), 2000);
              })
              .catch((cause: unknown) => setError(readError(cause)))
              .finally(() => setSaving(false));
          }}
        >
          save org config
        </Button>
        <Button
          variant="quiet"
          onClick={() => {
            setError(null);
            void data
              .updateOrg({ markSecretsRotated: true })
              .catch((cause: unknown) => setError(readError(cause)));
          }}
        >
          mark secrets rotated
        </Button>
        {config.ngrokAuthTokenUpdatedAt > 0 && (
          <Button
            variant="quiet"
            onClick={() => {
              setError(null);
              void data
                .updateOrg({ ngrokAuthToken: "" })
                .then(() => setNgrokToken(null))
                .catch((cause: unknown) => setError(readError(cause)));
            }}
          >
            remove ngrok authtoken
          </Button>
        )}
        {saved && (
          <span className="text-xs tracking-wider text-ok uppercase">saved</span>
        )}
      </div>

      <p className="mt-3 text-xs text-fg-faint wrap-value">
        secrets last rotated {formatTime(config.secretsUpdatedAt)} · saved CLI
        floor v{config.minCliVersion} · saved latest v{config.latestCliVersion}
      </p>

      {error !== null && (
        <div className="mt-4">
          <Alert tone="danger" title="Not saved">
            <p className="wrap-value">{error}</p>
          </Alert>
        </div>
      )}
    </div>
  );
}

export function AuditLog() {
  const data = useData();

  if (data.auditLog.state === "loading") {
    return <Loading label="loading audit log" />;
  }
  if (data.auditLog.state === "error") {
    return (
      <Alert tone="danger" title="Could not load the audit log">
        <p className="wrap-value">{data.auditLog.message}</p>
      </Alert>
    );
  }

  const columns: Column<AuditEntry>[] = [
    {
      key: "at",
      header: "when",
      render: (e) => <span className="text-fg-faint">{formatTime(e.at)}</span>,
    },
    {
      key: "action",
      header: "action",
      render: (e) => <span className="text-accent">{e.action}</span>,
    },
    {
      key: "who",
      header: "who",
      priority: "wide",
      render: (e) => (
        <span className="text-fg-dim">
          {e.actorLogin !== null && <>by @{e.actorLogin}</>}
          {e.subjectLogin !== null && e.subjectLogin !== e.actorLogin && (
            <> on @{e.subjectLogin}</>
          )}
          {e.actorLogin === null && e.subjectLogin === null && "—"}
        </span>
      ),
    },
    {
      key: "meta",
      header: "detail",
      grow: true,
      priority: "wide",
      render: (e) => (
        <span className="text-fg-dim">
          {Object.entries(e.meta)
            .map(([key, value]) => `${key}=${value}`)
            .join(" ") || "—"}
        </span>
      ),
    },
  ];

  return (
    <DataTable
      caption="Changes to access"
      columns={columns}
      rows={data.auditLog.value}
      rowKey={(e) => e._id}
      empty={
        <Empty glyph="∅" title="Nothing has changed yet.">
          Role promotions, suspensions and session revocations are recorded here.
        </Empty>
      }
    />
  );
}

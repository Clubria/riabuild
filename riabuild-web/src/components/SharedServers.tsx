import { FormEvent, useState } from "react";
import { useData } from "../data/context";
import { SharedServer } from "../data/types";
import { readError } from "../lib/errors";
import { formatTime } from "../lib/time";
import {
  Alert,
  Badge,
  Button,
  Column,
  DataTable,
  Empty,
  Field,
  Loading,
} from "../ui";

/** An empty form, and what "cancel" goes back to. */
const BLANK = { name: "", host: "", port: "22", user: "", description: "" };

type Draft = typeof BLANK;

/**
 * The longest description riabuild-web will store, and so the longest one worth
 * typing. `convex/sharedServers.ts` is the authority; this copy is what lets the
 * form say so before a lead loses a paragraph to a refusal.
 */
const DESCRIPTION_MAX = 120;

function draftOf(server: SharedServer): Draft {
  return {
    name: server.name,
    host: server.host,
    port: String(server.port),
    user: server.user,
    description: server.description,
  };
}

/** `user@host`, with the port only when it is not the default one — the same
 * rule the CLI's own server box follows, and for the same reason: a port is
 * part of a server's identity, and printing `:22` everywhere buries the one row
 * where it matters. */
function address(server: SharedServer): string {
  return server.port === 22
    ? `${server.user}@${server.host}`
    : `${server.user}@${server.host}:${server.port}`;
}

export function SharedServers() {
  const data = useData();
  const [draft, setDraft] = useState<Draft>(BLANK);
  const [editing, setEditing] = useState<SharedServer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  if (data.sharedServers.state === "loading") {
    return <Loading label="loading the team's servers" />;
  }
  if (data.sharedServers.state === "error") {
    return (
      <Alert tone="danger" title="Could not list the team's servers">
        <p className="wrap-value">{data.sharedServers.message}</p>
      </Alert>
    );
  }

  const servers = data.sharedServers.value;
  const port = Number(draft.port);
  // How long the description is, when that is too long — `null` otherwise, so
  // the two places that read it (the field's error and the disabled submit)
  // cannot disagree about where the limit is. riabuild-web refuses it anyway;
  // this is so a lead finds out while they are still typing.
  const typed = draft.description.trim().length;
  const overlong = typed > DESCRIPTION_MAX ? typed : null;
  // The address a lead is about to save re-identifies the server for everyone
  // if any part of the login target moves. A rename does not — the name is a
  // label, and `Remote::hash` never covers it.
  const readdressing =
    editing !== null &&
    (draft.host.trim() !== editing.host ||
      port !== editing.port ||
      draft.user.trim() !== editing.user);

  function reset() {
    setDraft(BLANK);
    setEditing(null);
    setError(null);
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    setError(null);
    setSaving(true);
    const address = {
      name: draft.name.trim(),
      host: draft.host.trim(),
      port: Number(draft.port),
      user: draft.user.trim(),
      description: draft.description.trim(),
    };
    const done =
      editing === null
        ? data.addSharedServer(address)
        : data.updateSharedServer({ ...address, id: editing._id });
    void done
      .then(reset)
      .catch((cause: unknown) => setError(readError(cause)))
      .finally(() => setSaving(false));
  }

  const columns: Column<SharedServer>[] = [
    {
      key: "name",
      header: "name",
      render: (server) => (
        <span className="text-fg wrap-value">
          <span className="text-fg-faint" aria-hidden="true">
            shared-
          </span>
          {server.name}
        </span>
      ),
    },
    {
      key: "address",
      header: "address",
      grow: true,
      // Two lines, the way the CLI's own picker draws them: the address, and
      // under it what the server is for. A server nobody has described gets one
      // line rather than a blank second one — the same rule the CLI follows,
      // where a row holding space for a sentence nobody wrote reads as a
      // sentence that failed to load.
      render: (server) => (
        <div className="min-w-0">
          <span className="block text-fg-dim wrap-value">
            {address(server)}
          </span>
          {server.description !== "" && (
            <span className="block text-fg-faint wrap-value">
              {server.description}
            </span>
          )}
        </div>
      ),
    },
    {
      key: "updated",
      header: "changed",
      priority: "wide",
      render: (server) => (
        <span className="text-fg-faint">{formatTime(server.updatedAt)}</span>
      ),
    },
  ];

  return (
    <>
      <p className="mb-4 max-w-prose text-fg-dim">
        Every developer sees these in{" "}
        <span className="text-fg">riabuild remote</span>, named{" "}
        <span className="text-fg">shared-&lt;name&gt;</span> so they cannot be
        confused with a server somebody added themselves. Only the address is
        shared: each laptop keeps its own key, its own saved password and its
        own session.
      </p>

      <DataTable
        caption="Servers the whole team can reach"
        columns={columns}
        rows={servers}
        rowKey={(server) => server._id}
        renderActions={(server) => (
          <>
            <Button
              variant="quiet"
              disabled={busy !== null || saving}
              aria-label={`Edit shared-${server.name}`}
              onClick={() => {
                setError(null);
                setEditing(server);
                setDraft(draftOf(server));
              }}
            >
              edit
            </Button>
            <Button
              variant="danger"
              pending={busy === server._id}
              pendingLabel="…"
              disabled={saving}
              aria-label={`Remove shared-${server.name}`}
              onClick={() => {
                setError(null);
                setBusy(server._id);
                void data
                  .removeSharedServer({ id: server._id })
                  .then(() => {
                    if (editing?._id === server._id) reset();
                  })
                  .catch((cause: unknown) => setError(readError(cause)))
                  .finally(() => setBusy(null));
              }}
            >
              remove
            </Button>
          </>
        )}
        empty={
          <Empty glyph="⌁" title="No shared servers yet.">
            Add one below and it appears in every developer&rsquo;s picker the
            next time they run riabuild remote.
          </Empty>
        }
      />

      {/* A real form, so Enter in "hostname" saves the server the way every
          other address box on the internet does, and the browser refuses an
          empty field before riabuild-web has to. */}
      <form className="mt-6 max-w-2xl" onSubmit={submit}>
        <p className="mb-3 flex flex-wrap items-center gap-2 text-fg-dim">
          <span aria-hidden="true" className="text-accent">
            ▸
          </span>
          {editing === null ? (
            "add a server"
          ) : (
            <>
              editing
              <Badge tone="accent">shared-{editing.name}</Badge>
            </>
          )}
        </p>

        <div className="grid gap-4 sm:grid-cols-2">
          <Field
            label="name"
            hint="Letters, digits, dots, dashes, underscores."
            value={draft.name}
            placeholder="gpu"
            required
            spellCheck={false}
            onChange={(name) => setDraft({ ...draft, name })}
          />
          <Field
            label="username"
            hint="The account riabuild signs in as."
            value={draft.user}
            placeholder="clubria"
            required
            spellCheck={false}
            onChange={(user) => setDraft({ ...draft, user })}
          />
          <Field
            label="hostname"
            hint="No username, no port — they have their own boxes."
            value={draft.host}
            placeholder="gpu.internal"
            required
            spellCheck={false}
            onChange={(host) => setDraft({ ...draft, host })}
          />
          <Field
            label="port"
            hint="22 unless this server says otherwise."
            value={draft.port}
            placeholder="22"
            required
            spellCheck={false}
            onChange={(value) => setDraft({ ...draft, port: value })}
          />
        </div>

        {/* Its own row rather than a third cell in the grid: it is the only
            box here that holds a sentence, and half a column is not where a
            sentence goes. Not `required` either — a server with no description
            is a server nobody has got round to, not a mistake. */}
        <div className="mt-4">
          <Field
            label="what it is for"
            hint="One line, shown under the server's name in every developer's picker."
            error={
              overlong === null
                ? null
                : `One line, up to ${DESCRIPTION_MAX} characters — this one is ${overlong}.`
            }
            value={draft.description}
            placeholder="The 4×A100 box. Ask before starting a long training run."
            onChange={(description) => setDraft({ ...draft, description })}
          />
        </div>

        {readdressing && (
          <div className="mt-4">
            <Alert tone="warn" title="This is a different machine to riabuild">
              <p className="wrap-value">
                A server is identified by {editing.user}@{editing.host}:
                {editing.port}, so changing any part of it makes this a new
                machine. Every developer&rsquo;s riabuild notices on their next
                connect: it revokes the session it minted on the old one, clears
                its key from it, and sets this one up fresh. Rename freely — a
                name is only a label.
              </p>
            </Alert>
          </div>
        )}

        <div className="mt-5 flex flex-wrap items-center gap-2">
          <Button
            type="submit"
            variant="primary"
            pending={saving}
            pendingLabel="saving"
            disabled={busy !== null || overlong !== null}
          >
            {editing === null ? "add server" : "save changes"}
          </Button>
          {editing !== null && (
            <Button variant="quiet" disabled={saving} onClick={reset}>
              cancel
            </Button>
          )}
        </div>

        {error !== null && (
          <div className="mt-4">
            <Alert tone="danger" title="Not saved">
              <p className="wrap-value">{error}</p>
            </Alert>
          </div>
        )}
      </form>
    </>
  );
}

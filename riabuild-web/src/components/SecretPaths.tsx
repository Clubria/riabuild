import { FormEvent, useState } from "react";
import { useData } from "../data/context";
import { RepoSecretPath } from "../data/types";
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
  TextArea,
} from "../ui";

/** An empty form, and what "cancel" goes back to. */
const BLANK = { repoSlug: "", folders: "" };

type Draft = typeof BLANK;

/**
 * The textarea's text, as the ordered list the mutation wants.
 *
 * One folder per line rather than a comma-separated box, because the order is
 * load-bearing and a list read top to bottom shows it. Blank lines are dropped
 * so a trailing newline is not a path — the server refuses an empty one anyway,
 * and being refused for pressing Enter is a bad way to learn that.
 */
function foldersOf(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

function draftOf(row: RepoSecretPath): Draft {
  return { repoSlug: row.repoSlug, folders: row.secretPaths.join("\n") };
}

export function SecretPaths() {
  const data = useData();
  const [draft, setDraft] = useState<Draft>(BLANK);
  const [editing, setEditing] = useState<RepoSecretPath | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  if (data.repoSecretPaths.state === "loading") {
    return <Loading label="loading the secret paths" />;
  }
  if (data.repoSecretPaths.state === "error") {
    return (
      <Alert tone="danger" title="Could not list the secret paths">
        <p className="wrap-value">{data.repoSecretPaths.message}</p>
      </Alert>
    );
  }

  const rows = data.repoSecretPaths.value;
  const folders = foldersOf(draft.folders);
  // Moving a repository to another folder is what makes every developer's
  // `.env.<name>` stale, so it is worth saying before the save rather than
  // after. A save that lands on the same list changes nothing at all, which is
  // why this compares in order — the order decides which folder's value wins.
  const moving =
    editing !== null && folders.join("\n") !== editing.secretPaths.join("\n");

  function reset() {
    setDraft(BLANK);
    setEditing(null);
    setError(null);
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    setError(null);
    setSaving(true);
    void data
      .setRepoSecretPaths({
        repoSlug: draft.repoSlug.trim(),
        secretPaths: foldersOf(draft.folders),
      })
      .then(reset)
      .catch((cause: unknown) => setError(readError(cause)))
      .finally(() => setSaving(false));
  }

  /**
   * The repository and its folders in **one** column, rather than two.
   *
   * Both values here can be long — a 100-character slug is a real GitHub name,
   * and a nested Infisical folder is longer still — and `DataTable` is shaped
   * for exactly one of those per table: the `grow` column wraps and every other
   * column is `whitespace-nowrap`. Two of them means the nowrap one sets the
   * table's width, the growing one collapses to its 14ch floor, and at 380px a
   * folder path stacks two characters per line down a column six wide. That is
   * visible in the shipped shared-servers table on adversarial data, and it is
   * not something to reproduce on purpose.
   *
   * Nesting them says what the row means anyway. A folder list is not a
   * property of a repository the way a port is of a server — it is what the
   * repository *resolves to*, and reading it as an indented list under the name
   * is how anybody would write it down.
   */
  const columns: Column<RepoSecretPath>[] = [
    {
      key: "repo",
      header: "repository, and its infisical folders",
      grow: true,
      render: (row) => (
        <div className="min-w-0">
          <span className="text-fg wrap-value">{row.repoSlug}</span>
          <ol className="mt-0.5 min-w-0">
            {row.secretPaths.map((path, index) => (
              <li key={path} className="flex items-baseline gap-2">
                {/* The ordinal is the whole point of showing these as a list:
                    a key two folders hold takes the value of the later one, so
                    which line a folder is on is a fact about the file that
                    lands. Decorative to a screen reader — `<ol>` already
                    announces the position. */}
                <span aria-hidden="true" className="shrink-0 text-fg-faint">
                  {index + 1}
                </span>
                <span className="min-w-0 text-fg-dim wrap-value">{path}</span>
              </li>
            ))}
          </ol>
        </div>
      ),
    },
    {
      key: "updated",
      header: "changed",
      priority: "wide",
      render: (row) => (
        <span className="text-fg-faint">{formatTime(row.updatedAt)}</span>
      ),
    },
  ];

  return (
    <>
      <p className="mb-3 max-w-prose text-fg-dim">
        Every developer&rsquo;s <span className="text-fg">env_local</span> reads
        the row for the repository their run is about, and writes one{" "}
        <span className="text-fg">.env.&lt;environment&gt;</span> for each
        environment those folders exist in. riabuild-web never sees a value:
        these name where the secrets live, and the CLI fetches them itself with
        a token brokered for that one run.
      </p>
      <p className="mb-4 max-w-prose text-fg-dim">
        <strong className="text-fg">
          A repository that is not listed gets no environment files.
        </strong>{" "}
        That is the way to say a repository has no environment variables — not
        an oversight, and not a repository that falls back to another
        one&rsquo;s secrets.
      </p>

      <DataTable
        caption="Where each repository's secrets come from"
        columns={columns}
        rows={rows}
        rowKey={(row) => row._id}
        renderActions={(row) => (
          <>
            <Button
              variant="quiet"
              disabled={busy !== null || saving}
              aria-label={`Edit the folders for ${row.repoSlug}`}
              onClick={() => {
                setError(null);
                setEditing(row);
                setDraft(draftOf(row));
              }}
            >
              edit
            </Button>
            <Button
              variant="danger"
              pending={busy === row._id}
              pendingLabel="…"
              disabled={saving}
              aria-label={`Give ${row.repoSlug} no environment files`}
              onClick={() => {
                setError(null);
                setBusy(row._id);
                void data
                  .removeRepoSecretPaths({ id: row._id })
                  .then(() => {
                    if (editing?._id === row._id) reset();
                  })
                  .catch((cause: unknown) => setError(readError(cause)))
                  .finally(() => setBusy(null));
              }}
            >
              unmap
            </Button>
          </>
        )}
        empty={
          <Empty glyph="⌁" title="No repository takes secrets from Infisical.">
            Until one is mapped below, every run skips environment variables
            entirely and reports env_local satisfied.
          </Empty>
        }
      />

      {/* A real form, so Enter in "repository" saves the mapping the way every
          other box on the internet does, and the browser refuses an empty field
          before riabuild-web has to. */}
      <form className="mt-6 max-w-2xl" onSubmit={submit}>
        <p className="mb-3 flex flex-wrap items-center gap-2 text-fg-dim">
          <span aria-hidden="true" className="text-accent">
            ▸
          </span>
          {editing === null ? (
            "map a repository"
          ) : (
            <>
              editing
              <Badge tone="accent">{editing.repoSlug}</Badge>
            </>
          )}
        </p>

        <div className="grid gap-4">
          <Field
            label="repository"
            hint="owner/name, exactly as GitHub spells it."
            value={draft.repoSlug}
            placeholder="Clubria/ai-builders-hub"
            required
            spellCheck={false}
            onChange={(repoSlug) => setDraft({ ...draft, repoSlug })}
          />
          <TextArea
            label="infisical folders"
            rows={4}
            hint="One per line, starting at the root. Where two of them hold the same key, the lower line wins."
            value={draft.folders}
            placeholder={"/\n/apps/payments"}
            onChange={(value) => setDraft({ ...draft, folders: value })}
          />
        </div>

        {moving && (
          <div className="mt-4">
            <Alert tone="warn" title="Every developer's files are rewritten">
              <p className="wrap-value">
                A <span className="text-fg">.env</span> file written from the
                folders {editing.repoSlug} used to name is as wrong as one
                written before the team rotated, and the file cannot tell the
                difference. Saving this marks all of them stale, so the next run
                on every laptop fetches them again.
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
            disabled={busy !== null}
          >
            {editing === null ? "map repository" : "save folders"}
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

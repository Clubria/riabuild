import { useEffect, useState } from "react";
import { useData } from "../data/context";
import { IssuedKey, Member } from "../data/types";
import { readError } from "../lib/errors";
import { KeyParseError, ParsedKey, parseOpenSshPrivateKey } from "../lib/opensshKey";
import {
  Alert,
  Badge,
  Button,
  Column,
  DataTable,
  Empty,
  Field,
  KeyValue,
  Loading,
  TextArea,
} from "../ui";

const BLANK = { label: "", privateKey: "" };

type Draft = typeof BLANK;

/**
 * What the paste box has produced so far.
 *
 * Three states rather than two, because "nothing pasted yet" and "pasted
 * something that will not parse" want different things on screen: the first is
 * silent, the second is the reason, and only the third fills the preview in.
 */
type Preview =
  | { state: "blank" }
  | { state: "bad"; message: string }
  | { state: "parsed"; key: ParsedKey };

/**
 * Parses the paste box's contents, off the render path.
 *
 * `parseOpenSshPrivateKey` is async — `crypto.subtle.digest` is — so this
 * cannot be a `useMemo`, and the result has to be stored.
 *
 * What is stored is the *paste it came from*, not just the verdict, and both
 * the empty case and the still-parsing case are derived below rather than
 * written back into state. That is what keeps a slow parse of an earlier paste
 * from being shown beside a later one — the stale result simply stops matching
 * — and it means nothing here calls `setState` in an effect body.
 */
function usePreview(privateKey: string): Preview {
  const [result, setResult] = useState<{
    source: string;
    preview: Preview;
  } | null>(null);

  useEffect(() => {
    if (privateKey.trim() === "") return;
    let cancelled = false;
    void parseOpenSshPrivateKey(privateKey)
      .then((key) => {
        if (!cancelled) {
          setResult({ source: privateKey, preview: { state: "parsed", key } });
        }
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setResult({
          source: privateKey,
          preview: {
            state: "bad",
            message:
              cause instanceof KeyParseError
                ? cause.message
                : "That key could not be read.",
          },
        });
      });
    return () => {
      cancelled = true;
    };
  }, [privateKey]);

  if (privateKey.trim() === "") return { state: "blank" };
  // Still parsing this paste, or holding a verdict for an earlier one. Either
  // way there is nothing true to show yet.
  if (result === null || result.source !== privateKey) return { state: "blank" };
  return result.preview;
}

function nameOf(member: Member): string {
  return member.githubLogin;
}

/**
 * The lead-only panel for SSH keys the org issues.
 *
 * The rule this component exists to hold: **a private key goes in and never
 * comes back out.** There is no reveal control and no edit-in-place for a key,
 * because no route serves one to a browser. Changing a key is a fresh paste —
 * which is also what makes the fingerprint worth showing, since it is how a
 * lead confirms which key a row holds.
 */
export function IssuedKeys() {
  const data = useData();
  const [draft, setDraft] = useState<Draft>(BLANK);
  const [replacing, setReplacing] = useState<IssuedKey | null>(null);
  const [issuing, setIssuing] = useState<IssuedKey | null>(null);
  const [picked, setPicked] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const preview = usePreview(draft.privateKey);

  if (data.issuedKeys.state === "loading") {
    return <Loading label="loading the keys the org issues" />;
  }
  if (data.issuedKeys.state === "error") {
    return (
      <Alert tone="danger" title="Could not list the issued keys">
        <p className="wrap-value">{data.issuedKeys.message}</p>
      </Alert>
    );
  }

  const keys = data.issuedKeys.value;
  const everyone = data.members.state === "ready" ? data.members.value : [];
  /**
   * Who may be *given* a key: active members only. Issuing to a suspended
   * account would hand out a credential riabuild-web will refuse to serve.
   */
  const pickable = everyone.filter((member) => member.status === "active");
  /**
   * Who may be *named* on a row: everyone, including the suspended.
   *
   * Built from the full list rather than `pickable`, which is the bug this
   * split exists to fix — a suspended member rendered as "(removed)", so the
   * one row a lead most needs to notice, a key still issued to somebody who
   * has been suspended, looked like stale data instead.
   */
  const loginOf = new Map(everyone.map((member) => [member._id, nameOf(member)]));

  function reset() {
    setDraft(BLANK);
    setReplacing(null);
    setError(null);
  }

  function submit() {
    setError(null);
    setSaving(true);
    const done =
      replacing === null
        ? data.addIssuedKey({
            label: draft.label.trim(),
            privateKey: draft.privateKey,
          })
        : data.replaceIssuedKey({
            id: replacing._id,
            privateKey: draft.privateKey,
          });
    void done
      .then(reset)
      .catch((cause: unknown) => setError(readError(cause)))
      .finally(() => setSaving(false));
  }

  function saveGrants(key: IssuedKey) {
    setError(null);
    setBusy(key._id);
    void data
      .setIssuedKeyMembers({ id: key._id, issuedTo: picked })
      .then(() => setIssuing(null))
      .catch((cause: unknown) => setError(readError(cause)))
      .finally(() => setBusy(null));
  }

  /**
   * Four columns and three actions did not fit at 1440px — the fingerprint got
   * a column narrow enough to wrap it across five lines, which defeats the one
   * thing it is for. So the type rides under the name, where it is a label
   * rather than a value, and `changed` is gone: a key that has not moved in a
   * month reads the same as one that changed this morning, and neither tells a
   * lead anything they act on.
   */
  const columns: Column<IssuedKey>[] = [
    {
      key: "label",
      header: "name",
      render: (key) => (
        <span className="block min-w-0">
          <span className="block text-fg wrap-value">{key.label}</span>
          <span className="block text-fg-faint">{key.keyType}</span>
        </span>
      ),
    },
    {
      key: "fingerprint",
      header: "fingerprint",
      grow: true,
      render: (key) => (
        <span className="text-fg-dim wrap-value">{key.fingerprint}</span>
      ),
    },
    {
      key: "issuedTo",
      header: "issued to",
      render: (key) =>
        key.issuedTo.length === 0 ? (
          <span className="text-fg-faint">nobody yet</span>
        ) : (
          <span className="flex flex-wrap gap-1">
            {key.issuedTo.map((id) => (
              <Badge key={id} tone="accent">
                {loginOf.get(id) ?? "(removed)"}
              </Badge>
            ))}
          </span>
        ),
    },
  ];

  return (
    <>
      <p className="mb-4 max-w-prose text-fg-dim">
        A key pasted here reaches the developers you issue it to, and lets their{" "}
        <span className="text-fg">riabuild remote</span> onto a server riabuild&rsquo;s
        own key cannot sign in to yet &mdash; a managed bastion, or any box with{" "}
        <span className="text-fg">PasswordAuthentication no</span>. riabuild uses
        it once, to install that laptop&rsquo;s own key, and every connection
        after that uses the laptop&rsquo;s.
      </p>
      <p className="mb-4 max-w-prose text-fg-dim">
        The private half is never shown again, here or anywhere. Compare the
        fingerprint to tell two keys apart, and paste a new key to rotate one.
      </p>

      <DataTable
        caption="SSH keys the org issues"
        columns={columns}
        rows={keys}
        rowKey={(key) => key._id}
        renderActions={(key) => (
          <>
            <Button
              variant="quiet"
              disabled={busy !== null || saving}
              aria-label={`Choose who gets ${key.label}`}
              onClick={() => {
                setError(null);
                setIssuing(key);
                setPicked(key.issuedTo);
              }}
            >
              issue
            </Button>
            <Button
              variant="quiet"
              disabled={busy !== null || saving}
              aria-label={`Replace the key for ${key.label}`}
              onClick={() => {
                setError(null);
                setReplacing(key);
                setDraft({ label: key.label, privateKey: "" });
              }}
            >
              replace
            </Button>
            <Button
              variant="danger"
              pending={busy === key._id && issuing === null}
              pendingLabel="…"
              disabled={saving}
              aria-label={`Remove ${key.label}`}
              onClick={() => {
                setError(null);
                setBusy(key._id);
                void data
                  .removeIssuedKey({ id: key._id })
                  .then(() => {
                    if (replacing?._id === key._id) reset();
                    if (issuing?._id === key._id) setIssuing(null);
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
          <Empty glyph="⚿" title="No issued keys yet.">
            Paste one below to let developers reach a server that authenticates
            with a key somebody else handed out.
          </Empty>
        }
      />

      {issuing !== null && (
        <div className="mt-6 max-w-2xl">
          <p className="mb-3 flex flex-wrap items-center gap-2 text-fg-dim">
            <span aria-hidden="true" className="text-accent">
              ▸
            </span>
            who gets
            <Badge tone="accent">{issuing.label}</Badge>
          </p>
          {pickable.length === 0 ? (
            <Empty glyph="⌁" title="No active members to issue this to.">
              Everyone on the team is suspended or the member list has not
              loaded.
            </Empty>
          ) : (
            <div className="flex flex-wrap gap-2">
              {pickable.map((member) => {
                const on = picked.includes(member._id);
                return (
                  <Button
                    key={member._id}
                    variant={on ? "primary" : "quiet"}
                    pressed={on}
                    disabled={busy !== null}
                    onClick={() =>
                      setPicked(
                        on
                          ? picked.filter((id) => id !== member._id)
                          : [...picked, member._id],
                      )
                    }
                  >
                    {nameOf(member)}
                    {/* Issuing to somebody who has not arrived is the point of
                        an invitation, not a mistake — but a lead should be able
                        to see which of these names is a person and which is a
                        plan. */}
                    {member.invited && (
                      <span className="ml-1.5 text-fg-faint">· invited</span>
                    )}
                  </Button>
                );
              })}
            </div>
          )}
          <div className="mt-5 flex flex-wrap items-center gap-2">
            <Button
              variant="primary"
              pending={busy === issuing._id}
              pendingLabel="saving"
              onClick={() => saveGrants(issuing)}
            >
              save who gets it
            </Button>
            <Button
              variant="quiet"
              disabled={busy !== null}
              onClick={() => setIssuing(null)}
            >
              cancel
            </Button>
          </div>
        </div>
      )}

      <div className="mt-6 max-w-2xl">
        <p className="mb-3 flex flex-wrap items-center gap-2 text-fg-dim">
          <span aria-hidden="true" className="text-accent">
            ▸
          </span>
          {replacing === null ? (
            "add a key"
          ) : (
            <>
              replacing the key for
              <Badge tone="accent">{replacing.label}</Badge>
            </>
          )}
        </p>

        {replacing !== null && (
          <div className="mb-4">
            <Alert tone="warn" title="This replaces the secret, and nothing else">
              <p className="wrap-value">
                {replacing.label} keeps its name and stays issued to the same
                people. Nothing on a laptop stores an issued key, so everyone
                picks the new one up on their next run without doing anything
                &mdash; but any server that still trusts only the old key stops
                being reachable this way.
              </p>
            </Alert>
          </div>
        )}

        <div className="grid gap-4">
          {replacing === null && (
            <Field
              // "key name", not "name". The team's servers section on this same
              // page already has a field labelled "name", and two controls with
              // one accessible name is ambiguous to anyone navigating by label —
              // which is exactly how the shared-servers test found it.
              label="key name"
              hint="Letters, digits, dots, dashes, underscores. What developers see in their terminal."
              value={draft.label}
              placeholder="prod-bastion"
              spellCheck={false}
              onChange={(label) => setDraft({ ...draft, label })}
            />
          )}
          <TextArea
            label="private key"
            rows={8}
            hint="The whole OpenSSH file, BEGIN and END lines included. It cannot have a passphrase — nothing could answer the prompt on a developer's machine."
            value={draft.privateKey}
            placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
            error={preview.state === "bad" ? preview.message : null}
            onChange={(privateKey) => setDraft({ ...draft, privateKey })}
          />
        </div>

        {preview.state === "parsed" && (
          <div className="mt-4">
            <p className="mb-2 text-fg-faint">
              read back from the key itself, and what will be stored:
            </p>
            <KeyValue
              rows={[
                { label: "type", value: preview.key.keyType },
                { label: "fingerprint", value: preview.key.fingerprint, tone: "ok" },
                { label: "public key", value: preview.key.publicKey },
              ]}
            />
          </div>
        )}

        <div className="mt-5 flex flex-wrap items-center gap-2">
          <Button
            variant="primary"
            pending={saving}
            pendingLabel="saving"
            disabled={busy !== null || preview.state !== "parsed"}
            onClick={submit}
          >
            {replacing === null ? "add key" : "replace key"}
          </Button>
          {replacing !== null && (
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
      </div>
    </>
  );
}

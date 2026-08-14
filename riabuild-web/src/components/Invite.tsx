import { useState } from "react";
import { useData } from "../data/context";
import { OrgCandidate, Role } from "../data/types";
import { readError } from "../lib/errors";
import { Alert, Badge, Button, Empty, Loading, Select } from "../ui";

/**
 * The org's member list, which costs a call to GitHub and so is not fetched
 * until a lead asks for it.
 *
 * Four states rather than a `Loadable`, because `idle` is a real one here: a
 * lead who never invites anybody should never spend that call, and "we have not
 * asked yet" is different from "we asked and are waiting".
 */
type Catalogue =
  | { state: "idle" }
  | { state: "loading" }
  | { state: "error"; message: string }
  | { state: "ready"; candidates: OrgCandidate[] };

/**
 * A role can be recorded before the person arrives, so `candidate` is offered
 * too — not because pre-assigning the default is useful on its own, but because
 * a lead may want somebody to hold a key while their role is still undecided,
 * and the alternative is a picker that silently disagrees with the one in the
 * member list below it.
 */
const ROLE_OPTIONS = [
  { value: "developer", label: "developer" },
  { value: "lead", label: "lead" },
  { value: "candidate", label: "candidate" },
];

/**
 * Inviting somebody before their first sign-in.
 *
 * What this writes is a real member row with nobody behind it yet, which is why
 * the keys picked here can be granted at the same moment: an issued key is
 * held by a member id, and the id written now is the one the row keeps when the
 * developer's sign-in claims it.
 *
 * It grants no access. The row cannot authenticate anything — an invited
 * `lead` is a decision recorded in advance, not access handed out in advance —
 * and the copy on screen says so, because "lead" appearing beside a name nobody
 * has verified is exactly the thing a reader would otherwise assume the worst
 * about.
 */
export function Invite() {
  const data = useData();
  const [catalogue, setCatalogue] = useState<Catalogue>({ state: "idle" });
  const [pickedId, setPickedId] = useState<string | null>(null);
  const [role, setRole] = useState<Role>("developer");
  const [keys, setKeys] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [invited, setInvited] = useState<string | null>(null);

  const members = data.members.state === "ready" ? data.members.value : [];
  const issuedKeys = data.issuedKeys.state === "ready" ? data.issuedKeys.value : [];

  /**
   * Everyone GitHub reported who has no riabuild row yet — invited or arrived,
   * both count. Matched on the numeric id first, since a rename would otherwise
   * offer somebody who is already here under a name we no longer hold.
   */
  const taken = new Set(
    members.flatMap((member) => [
      member.githubId,
      member.githubLogin.toLowerCase(),
    ]),
  );
  const available =
    catalogue.state === "ready"
      ? catalogue.candidates.filter(
          (person) =>
            !taken.has(person.githubId) && !taken.has(person.login.toLowerCase()),
        )
      : [];
  /**
   * Derived rather than stored, so the selection survives the list changing
   * underneath it: the person just invited leaves `available` as soon as Convex
   * reports the new row, and this falls to the next one instead of pointing at
   * somebody who is no longer offered.
   */
  const picked =
    available.find((person) => person.githubId === pickedId) ?? available[0];

  function load() {
    setError(null);
    setCatalogue({ state: "loading" });
    void data.listOrgMembers().then(
      (candidates) => setCatalogue({ state: "ready", candidates }),
      (cause: unknown) =>
        setCatalogue({ state: "error", message: readError(cause) }),
    );
  }

  function submit() {
    if (picked === undefined) return;
    setError(null);
    setInvited(null);
    setSaving(true);
    const login = picked.login;
    void data
      .inviteMember({
        githubLogin: login,
        githubId: picked.githubId,
        role,
        issuedKeys: keys,
      })
      .then(() => {
        setInvited(login);
        setKeys([]);
        setRole("developer");
        setPickedId(null);
      })
      .catch((cause: unknown) => setError(readError(cause)))
      .finally(() => setSaving(false));
  }

  return (
    <>
      <p className="mb-4 max-w-prose text-fg-dim">
        Pick somebody out of the Clubria GitHub org and set their role now,
        before they have ever run riabuild. Their first sign-in claims the row
        you make here, so they arrive already provisioned instead of arriving as
        a candidate and waiting for you to notice.
      </p>
      <p className="mb-4 max-w-prose text-fg-dim">
        Nobody is signed in as an invited person, so this hands out nothing on
        its own: an invited <span className="text-fg">lead</span> is a decision
        written down, not access given away.
      </p>

      {catalogue.state === "idle" && (
        <Empty
          glyph="✧"
          title="Nobody invited from here yet."
          action={
            <Button variant="primary" onClick={load}>
              list the org&rsquo;s members
            </Button>
          }
        >
          riabuild asks GitHub who is in the org when you press this, rather than
          on every visit to this page.
        </Empty>
      )}

      {catalogue.state === "loading" && (
        <Loading label="asking GitHub who is in the org" />
      )}

      {catalogue.state === "error" && (
        <Alert tone="danger" title="Could not list the org's members">
          <p className="wrap-value">{catalogue.message}</p>
          <p className="mt-3">
            <Button variant="quiet" onClick={load}>
              try again
            </Button>
          </p>
        </Alert>
      )}

      {catalogue.state === "ready" && picked === undefined && (
        <Empty glyph="✓" title="Everyone in the org is already here.">
          Every GitHub account in the org has a riabuild row, invited or arrived.
          Somebody missing? Send them the GitHub org invite first &mdash;
          riabuild can only offer people GitHub already knows about.
        </Empty>
      )}

      {catalogue.state === "ready" && picked !== undefined && (
        <div className="max-w-2xl">
          <div className="grid gap-4 sm:grid-cols-2">
            <Select
              label="person"
              hint="Members of the Clubria GitHub org with no riabuild row yet."
              value={picked.githubId}
              options={available.map((person) => ({
                value: person.githubId,
                label: person.login,
              }))}
              disabled={saving}
              onChange={setPickedId}
            />
            <Select
              label="role"
              hint="What they are the moment they sign in."
              value={role}
              options={ROLE_OPTIONS}
              disabled={saving}
              onChange={(value) => setRole(value as Role)}
            />
          </div>

          <div className="mt-5">
            {/* The same label treatment `FieldShell` gives person and role, so
                the three read as one form rather than two controls and a
                heading. It cannot *be* a `FieldShell`: a `<label for>` needs one
                control to point at, and this labels a group of them. */}
            <p
              id="invite-keys-label"
              className="mb-1 block text-xs tracking-wider text-fg-dim uppercase"
            >
              keys to issue them
            </p>
            {issuedKeys.length === 0 ? (
              <p className="max-w-prose text-xs text-fg-faint">
                The org issues no SSH keys yet. Add one in the issued keys
                section below and it can be handed out here.
              </p>
            ) : (
              <div
                role="group"
                aria-labelledby="invite-keys-label"
                className="mt-2 flex flex-wrap gap-2"
              >
                {issuedKeys.map((key) => {
                  const on = keys.includes(key._id);
                  return (
                    <Button
                      key={key._id}
                      variant={on ? "primary" : "quiet"}
                      pressed={on}
                      disabled={saving}
                      onClick={() =>
                        setKeys(
                          on
                            ? keys.filter((id) => id !== key._id)
                            : [...keys, key._id],
                        )
                      }
                    >
                      {key.label}
                    </Button>
                  );
                })}
              </div>
            )}
            <p className="mt-2 max-w-prose text-xs text-fg-faint">
              An issued key reaches their laptop on their first run and lets it
              onto a server riabuild&rsquo;s own key cannot sign in to yet.
            </p>
          </div>

          <div className="mt-5 flex flex-wrap items-center gap-2">
            <Button
              variant="primary"
              pending={saving}
              pendingLabel="inviting"
              onClick={submit}
            >
              invite @{picked.login} as {role}
            </Button>
            <Button variant="quiet" disabled={saving} onClick={load}>
              refresh the org list
            </Button>
            {invited !== null && (
              <span className="flex flex-wrap items-center gap-2 text-xs tracking-wider text-ok uppercase">
                invited <Badge tone="ok">@{invited}</Badge>
              </span>
            )}
          </div>
        </div>
      )}

      {error !== null && (
        <div className="mt-4">
          <Alert tone="danger" title="Not invited">
            <p className="wrap-value">{error}</p>
          </Alert>
        </div>
      )}
    </>
  );
}

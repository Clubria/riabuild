# Inviting members ahead of their first sign-in

2026-08-14

## The problem

A `members` row is created by `auth.ts:upsertMember`, on the developer's first GitHub
sign-in, with `role: "candidate"`. Everything a lead can decide about a person —
their role, and which issued SSH keys they hold — can therefore only be decided
*after* they have already arrived and found themselves a candidate with nothing.

That inverts the order the work actually happens in. A lead knows who is joining, and
what they will need, days before that person opens a terminal. Today they must wait for
the sign-in, notice it, and then go and fix it — and until they do, the developer's
first `riabuild` run is the broken one.

Two things are wanted, and they are the same feature:

1. Promote someone to `developer` or `lead` before they sign in.
2. Issue them an SSH key before they sign in.

## The decision: invited rows are member rows

`members.userId` becomes optional. **Absent means invited and not yet arrived.**
`upsertMember` looks for an unclaimed row before inserting a new one, and adopts it.

The alternative was a separate `pendingMembers` table. It is rejected, and the reason is
`issuedKeys.issuedTo`, which is `v.array(v.id("members"))`. A second table would force:

- a parallel `pendingIssuedTo: v.array(v.id("pendingMembers"))` on every key row,
- a second branch in every reader of that array, including `serveForApi`,
- and a migration of ids from one array into the other at sign-in — the one moment
  where a mistake silently drops somebody's access.

With adoption, an invited person has a real `Id<"members">` from the moment a lead picks
them. Pre-assigning a key needs **no new mechanism**: `issuedKeys.setIssuedTo` already
writes the right id, and that id stays valid across adoption. The second half of this
feature costs one line of UI.

What it costs instead: `members` no longer means "someone who has signed in". Every
reader must be checked. §"What an invited row must never do" is that audit.

### Adoption matches on `githubId`, not `githubLogin`

The org listing API returns GitHub's numeric id alongside the login, so an invited row
can carry the real `githubId` from the start. A developer can rename their GitHub
account between the lead's click and their first sign-in; the numeric id cannot change.
`upsertMember` already relies on exactly this for renames of existing members.

Login is kept as a fallback match for rows invited before a `by_githubId` index existed
— of which there are none, but the fallback costs nothing and the alternative is a
silent duplicate row.

## Picking from the org

A lead does not type a username. `github.listOrgMembers` — a lead-only action — calls
`GET /orgs/{org}/members` with the server-held `GITHUB_ORG_TOKEN` that
`checkOrgMembership` already uses, paginating to a bounded 500, and returns
`{ login, githubId }`.

Typing a username is what this replaces, and the reason is not convenience. A typo in a
hand-typed login produces an invited row that nobody will ever adopt: it sits in the
member list looking like a provisioned developer, holding an SSH key grant, and the
person it was meant for signs in beside it as a fresh candidate with nothing. Picking
from the org's own list cannot produce that row.

Under `RIABUILD_DEV_AUTH=1` it returns a canned list instead of calling GitHub, exactly
as `viewerOrgMembership` already does — a dev deployment has no real org to ask, and
without this the whole invite flow would be unreachable locally and in Playwright.

It is an action, not a query, so it is exposed on the data contract as
`listOrgMembers(): Promise<OrgCandidate[]>` — a promise method like `lookupDeviceCode`,
called when a lead opens the invite form rather than on every dashboard render. A lead
who never invites anyone costs GitHub nothing.

People who already have a `members` row are filtered out client-side, by `githubId`.
Inviting someone who is already here is not an error worth a message; it is a row that
should not have been offered.

## What an invited row must never do

An invited row has no `userId`, and that is what keeps it inert:

| Path | Why an invited row cannot reach it |
|---|---|
| `viewerMember` | queries `by_userId` with a real user id; `undefined` never matches |
| `requireLead` | goes through `viewerMember` |
| every `/api/v1` route | requires a `cliSessions` row, which requires a sign-in |
| `issuedKeys.serveForApi` | takes its `memberId` from that session |

So an invited `lead` is not a lead until they arrive. The role is a *decision recorded
in advance*, not access granted in advance, and nothing changes about
"identity is GitHub, authorization is Convex" — an invited row still cannot get past the
GitHub org check that gates every secret.

`members.list` returns invited rows, and must: the lead who invited them has to see
them, change their mind, and withdraw. They are marked, not hidden.

## Withdrawing an invitation

`members.removeInvite` deletes an unclaimed row, and **refuses a claimed one** — once
someone has signed in, the way to remove them is `setStatus("suspended")`, which also
revokes their sessions. A delete would leave live sessions pointing at a row that is
gone.

It also strips the id out of every `issuedKeys.issuedTo` it appears in. Skipping that
leaves a dangling id, which `setIssuedTo` rejects with "one of the people you picked is
no longer a member" — a lead would be locked out of editing that key's grants by a
person who no longer exists.

## Audit

- `member.invited` — actor, subject, `{ githubLogin, role }`
- `member.joined` — subject, `{ githubLogin, role, source: "invite" }`, written on
  adoption in place of `member.created`
- `member.invite_withdrawn` — actor, `{ githubLogin, role }`

An invited row whose role is later changed goes through the existing `setRole`, and gets
the existing `member.role_changed`. Nothing new is needed for that.

## UI

One new panel, `Invite`, in the lead section of the dashboard, above the member list —
the order a lead works in: invite, then manage.

- a `Select` of org members not yet in riabuild, loaded on demand
- a role `Select`, defaulting to `developer`
- a row of toggles for the issued keys to grant, reusing the shape already in
  `IssuedKeys`

Invited people appear in the member list with a `pending` badge beside their role, and
an `withdraw` action in place of `suspend`. They appear in the issued-key picker like
anyone else, because they are like anyone else — that is the point of adoption.

Scenarios: `invite-empty` (everyone in the org is already here), `invite-available`,
`members-invited` (the list holding an invited row), `invite-refused` (the mutation
rejected), and the invited row joins the `overflow` fixture with a 60-character login.

## Out of scope

No email. No invitation link, no token, no expiry. riabuild does not send mail and an
invited row is not a credential — the developer's route in is unchanged, which is to
accept the GitHub org invite and run `riabuild`. This feature shortens what happens
*after* that, and adding a second channel to tell them about it would be a new thing to
keep working.

No CLI surface. Nothing an invited row holds is readable before sign-in, so there is
nothing for the CLI to read.

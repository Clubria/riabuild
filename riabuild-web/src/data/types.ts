/**
 * The contract between the pages and whatever is feeding them.
 *
 * Before this existed every list component called `useQuery` itself, which meant
 * no UI state could be rendered without a database in that state — a suspended
 * member with an expired session and a 300-character device label was simply
 * unreachable in a test. Two implementations satisfy this contract: the real
 * Convex one, and a dev-only fixture one. Pages cannot tell them apart.
 */

import type { Id } from "../../convex/_generated/dataModel";

/**
 * Row ids, kept as the branded types Convex hands out rather than flattened to
 * `string`.
 *
 * A type-only import, so nothing here drags the backend into a bundle — and
 * `Id<T>` is a string at runtime, so a fixture is still written as one.
 *
 * What the brand buys is the boundary in `convexProvider`. Declared as `string`
 * these arrived at a mutation that wanted `Id<"members">` and were laundered
 * through `as never` on the way in: eleven casts, each of which would equally
 * have accepted a session id, a member id or yesterday's date. Now the table an
 * id belongs to travels with it from the query that produced it to the mutation
 * that consumes it, and the casts are gone.
 */
export type MemberId = Id<"members">;
export type SessionId = Id<"cliSessions">;
export type SharedServerId = Id<"sharedServers">;
export type IssuedKeyId = Id<"issuedKeys">;
export type AuditEntryId = Id<"auditLog">;

export type Role = "candidate" | "developer" | "lead";
export type MemberStatus = "active" | "suspended";

export type Loadable<T> =
  | { state: "loading" }
  | { state: "ready"; value: T }
  | { state: "error"; message: string };

export type Member = {
  _id: MemberId;
  /** The UUID a developer copies into their terminal. Not a row id. */
  memberId: string;
  githubLogin: string;
  githubId: string;
  firstName: string;
  lastName: string;
  email: string;
  role: Role;
  status: MemberStatus;
  joinedAt: number;
  /**
   * Nobody has signed in as this person yet — a lead picked them out of the
   * GitHub org and recorded a role, and possibly a key, in advance.
   *
   * They are a real member row with a real id, which is why they can hold an
   * issued key before they arrive, and why they appear in every picker beside
   * everybody else. What they cannot do is sign anything in: the row has no
   * user behind it until a real GitHub sign-in claims it.
   *
   * For an invited person `joinedAt` is when they were invited.
   */
  invited: boolean;
};

/**
 * Somebody in the Clubria GitHub org, as offered to a lead who is inviting.
 *
 * Not a `Member` and deliberately not shaped like one: this person may have no
 * riabuild row at all, and everything a `Member` carries — role, status, member
 * id — is exactly what has not been decided about them yet.
 */
export type OrgCandidate = {
  login: string;
  githubId: string;
};

export type Session = {
  _id: SessionId;
  deviceLabel: string;
  cliVersion: string;
  createdAt: number;
  lastUsedAt: number;
  expiresAt: number;
  revokedAt: number | null;
  /**
   * How this session came to exist. `device` is the usual one — a person typed
   * a code into this dashboard and approved it. `delegated` means another of
   * their machines asked for it on a server's behalf, which is what
   * `riabuild remote` does and the only sign-in nobody approved by hand.
   *
   * Always one of the two here: `sessions.ts` resolves the absent case before
   * this reaches the page, so nothing in `src/` has to know that older rows
   * predate the field.
   */
  origin: "device" | "delegated";
};

export type AuditEntry = {
  _id: AuditEntryId;
  at: number;
  action: string;
  actorLogin: string | null;
  subjectLogin: string | null;
  meta: Record<string, string>;
};

export type OrgConfig = {
  repoSlug: string;
  claudeSettings: string;
  claudeSettingsUpdatedAt: number;
  minCliVersion: string;
  latestCliVersion: string;
  secretsUpdatedAt: number;
  /**
   * The last four characters of the team's ngrok authtoken, or `""` when none
   * is set. Never the token: a lead has to recognise the one they pasted, and
   * has no reason to read it back. The value leaves riabuild-web only through
   * `GET /api/v1/org/ngrok-token`, to a signed-in CLI.
   */
  ngrokAuthTokenHint: string;
  /** When a lead last set it. Zero means no token is set. */
  ngrokAuthTokenUpdatedAt: number;
};

/**
 * One of the team's servers, as a lead sees it.
 *
 * `name` is bare. Every developer's CLI shows it as `shared-<name>` so it
 * cannot be confused with a server they added themselves, and neither end
 * stores that prefix — which is why nothing here does either.
 */
export type SharedServer = {
  _id: SharedServerId;
  name: string;
  host: string;
  port: number;
  user: string;
  updatedAt: number;
};

export type SharedServerAddress = {
  name: string;
  host: string;
  port: number;
  user: string;
};

/**
 * An SSH key the org issues, as a lead sees it.
 *
 * Note what is not here and never will be: the private key. Nothing returns one
 * to a browser — not on edit, not behind a reveal — which is why `fingerprint`
 * is stored and shown at all. It is how a lead tells two rows apart, and how
 * they confirm which key a row holds, without the row ever handing it back.
 */
export type IssuedKey = {
  _id: IssuedKeyId;
  label: string;
  keyType: string;
  publicKey: string;
  fingerprint: string;
  /** Member row ids. Resolved against `members` for display. */
  issuedTo: MemberId[];
  updatedAt: number;
};

export type MembershipStatus =
  "member" | "not_member" | "unavailable" | "signed_out" | "checking";

export type Membership = {
  org: string;
  status: MembershipStatus;
  detail?: string;
};

export type OrgUpdate = {
  claudeSettings?: string;
  repoSlug?: string;
  minCliVersion?: string;
  latestCliVersion?: string;
  markSecretsRotated?: boolean;
  /**
   * Absent leaves the team's token alone, which is what an ordinary settings
   * save must do — the field on screen is blank because it is write-only, not
   * because anybody cleared it. An empty string is the deliberate removal.
   */
  ngrokAuthToken?: string;
};

export type Data = {
  auth: "loading" | "signed-in" | "signed-out";
  viewer: Loadable<Member | null>;
  membership: Membership;
  sessions: Loadable<Session[]>;
  /** Lead-only. Stays `loading` for everyone else, who never renders it. */
  members: Loadable<Member[]>;
  /**
   * Lead-only, like `members` — but only the *section* is. Every developer
   * reads the same servers through their CLI's picker, which is where they can
   * act on them.
   */
  sharedServers: Loadable<SharedServer[]>;
  /** Lead-only, like `sharedServers`. A developer receives these through their CLI. */
  issuedKeys: Loadable<IssuedKey[]>;
  auditLog: Loadable<AuditEntry[]>;
  orgConfig: Loadable<OrgConfig>;
  /** Ticking clock, so "expired" is computed rather than frozen at mount. */
  now: number;

  /**
   * Every action below is declared as a property holding a function, not as a
   * method.
   *
   * A page is expected to pull one out and keep it — `CliAuthorize` holds
   * `lookupDeviceCode` in an effect's dependency list, which is what stopped
   * that effect re-running on every clock tick. A method shorthand says the
   * opposite: that it wants to be called with a receiver, which is what
   * `@typescript-eslint/unbound-method` objects to when it is detached. None of
   * these reads `this`, and both providers build them as closures.
   */
  updateProfile: (p: {
    firstName: string;
    lastName: string;
    email: string;
  }) => Promise<void>;
  setRole: (p: { memberId: MemberId; role: Role }) => Promise<void>;
  /**
   * Everyone in the GitHub org, so a lead picks a name instead of typing one.
   *
   * A promise rather than a `Loadable` field, like `lookupDeviceCode`: it costs
   * a call to GitHub, and a lead who never invites anybody should never spend
   * it. Called when the invite form opens.
   */
  listOrgMembers: () => Promise<OrgCandidate[]>;
  /**
   * Records a role — and the keys they hold — before this person has ever
   * signed in. Grants nothing on its own; the row it writes cannot authenticate
   * anything until a real GitHub sign-in claims it.
   */
  inviteMember: (p: {
    githubLogin: string;
    githubId: string;
    role: Role;
    issuedKeys: IssuedKeyId[];
  }) => Promise<void>;
  /**
   * Withdraws an invitation, and refuses anyone who has already arrived — for
   * them the action is `setStatus`, which also revokes their sessions.
   */
  withdrawInvite: (p: { memberId: MemberId }) => Promise<void>;
  setStatus: (p: { memberId: MemberId; status: MemberStatus }) => Promise<void>;
  revokeSession: (p: { sessionId: SessionId }) => Promise<void>;
  updateOrg: (p: OrgUpdate) => Promise<void>;
  addSharedServer: (p: SharedServerAddress) => Promise<void>;
  /**
   * Editing an address is editing an identity: riabuild keys a server's SSH
   * key, its saved password and its session off `user@host:port`. Every
   * developer's CLI retires the old one on its next connect, but until it runs
   * their credentials point at the machine that name used to mean.
   */
  updateSharedServer: (
    p: SharedServerAddress & { id: SharedServerId },
  ) => Promise<void>;
  removeSharedServer: (p: { id: SharedServerId }) => Promise<void>;
  addIssuedKey: (p: { label: string; privateKey: string }) => Promise<void>;
  /**
   * Rotation. The row, its name and the people it is issued to all survive;
   * only the secret changes. Nothing on a laptop stores an issued key, so a
   * developer's next run picks the new one up with no action from them.
   */
  replaceIssuedKey: (p: {
    id: IssuedKeyId;
    privateKey: string;
  }) => Promise<void>;
  setIssuedKeyMembers: (p: {
    id: IssuedKeyId;
    issuedTo: MemberId[];
  }) => Promise<void>;
  removeIssuedKey: (p: { id: IssuedKeyId }) => Promise<void>;
  signIn: (p?: { redirectTo?: string }) => Promise<void>;
  /**
   * Present only in dev builds, and only works against a deployment that sets
   * `RIABUILD_DEV_AUTH=1`. Optional on the type so a production build has no
   * expression referring to it at all.
   */
  devSignIn?: (login: string) => Promise<void>;
  signOut: () => Promise<void>;
  /**
   * Looks up the code a developer read off their terminal.
   *
   * A promise rather than a `Loadable` field because the argument comes from a
   * text box: there is nothing to load until someone has typed something.
   */
  lookupDeviceCode: (p: { userCode: string }) => Promise<DeviceRequest>;
  approveDeviceCode: (p: { userCode: string }) => Promise<DeviceDecision>;
  denyDeviceCode: (p: { userCode: string }) => Promise<DeviceDecision>;
};

/** A pending `riabuild login`, as shown to whoever is asked to approve it. */
export type DeviceRequest =
  | {
      status: "pending";
      deviceLabel: string;
      cliVersion: string;
      requestedAt: number;
      expiresAt: number;
    }
  | { status: "unknown" | "expired" | "used" };

export type DeviceDecision = {
  status: "ok" | "unknown" | "expired" | "used";
};

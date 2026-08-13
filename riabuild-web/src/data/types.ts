/**
 * The contract between the pages and whatever is feeding them.
 *
 * Before this existed every list component called `useQuery` itself, which meant
 * no UI state could be rendered without a database in that state — a suspended
 * member with an expired session and a 300-character device label was simply
 * unreachable in a test. Two implementations satisfy this contract: the real
 * Convex one, and a dev-only fixture one. Pages cannot tell them apart.
 */

export type Role = "candidate" | "developer" | "lead";
export type MemberStatus = "active" | "suspended";

export type Loadable<T> =
  | { state: "loading" }
  | { state: "ready"; value: T }
  | { state: "error"; message: string };

export type Member = {
  _id: string;
  memberId: string;
  githubLogin: string;
  githubId: string;
  firstName: string;
  lastName: string;
  email: string;
  role: Role;
  status: MemberStatus;
  joinedAt: number;
};

export type Session = {
  _id: string;
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
  _id: string;
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
};

/**
 * One of the team's servers, as a lead sees it.
 *
 * `name` is bare. Every developer's CLI shows it as `shared-<name>` so it
 * cannot be confused with a server they added themselves, and neither end
 * stores that prefix — which is why nothing here does either.
 */
export type SharedServer = {
  _id: string;
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

export type MembershipStatus =
  | "member"
  | "not_member"
  | "unavailable"
  | "signed_out"
  | "checking";

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
  auditLog: Loadable<AuditEntry[]>;
  orgConfig: Loadable<OrgConfig>;
  /** Ticking clock, so "expired" is computed rather than frozen at mount. */
  now: number;

  updateProfile(p: {
    firstName: string;
    lastName: string;
    email: string;
  }): Promise<void>;
  setRole(p: { memberId: string; role: Role }): Promise<void>;
  setStatus(p: { memberId: string; status: MemberStatus }): Promise<void>;
  revokeSession(p: { sessionId: string }): Promise<void>;
  updateOrg(p: OrgUpdate): Promise<void>;
  addSharedServer(p: SharedServerAddress): Promise<void>;
  /**
   * Editing an address is editing an identity: riabuild keys a server's SSH
   * key, its saved password and its session off `user@host:port`. Every
   * developer's CLI retires the old one on its next connect, but until it runs
   * their credentials point at the machine that name used to mean.
   */
  updateSharedServer(p: SharedServerAddress & { id: string }): Promise<void>;
  removeSharedServer(p: { id: string }): Promise<void>;
  signIn(p?: { redirectTo?: string }): Promise<void>;
  /**
   * Present only in dev builds, and only works against a deployment that sets
   * `RIABUILD_DEV_AUTH=1`. Optional on the type so a production build has no
   * expression referring to it at all.
   */
  devSignIn?(login: string): Promise<void>;
  signOut(): Promise<void>;
  /**
   * Looks up the code a developer read off their terminal.
   *
   * A promise rather than a `Loadable` field because the argument comes from a
   * text box: there is nothing to load until someone has typed something.
   */
  lookupDeviceCode(p: { userCode: string }): Promise<DeviceRequest>;
  approveDeviceCode(p: { userCode: string }): Promise<DeviceDecision>;
  denyDeviceCode(p: { userCode: string }): Promise<DeviceDecision>;
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

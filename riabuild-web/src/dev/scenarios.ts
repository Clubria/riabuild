import {
  AuditEntry,
  Data,
  DeviceRequest,
  IssuedKey,
  Member,
  OrgCandidate,
  OrgConfig,
  Session,
  SharedServer,
} from "../data/types";

/**
 * Fixture data for every UI state worth looking at.
 *
 * Every timestamp is derived from a frozen `NOW`. Nothing here calls
 * `Date.now()` — a screenshot suite whose fixtures move with the wall clock
 * produces a different image every run and stops being evidence of anything.
 *
 * Adding a state to the UI means adding a scenario here. That is the rule that
 * keeps the visual suite honest: a state with no scenario is a state nobody has
 * ever looked at.
 */
export const NOW = 1_785_000_000_000;

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;

const LEAD: Member = {
  _id: "m_lead",
  memberId: "4a1e9c2d-6b3f-4a17-9d2e-8c5f1a3b7e60",
  githubLogin: "ilya",
  githubId: "1",
  firstName: "Ilya",
  lastName: "Konstantinov",
  email: "ilya@clubria.test",
  role: "lead",
  status: "active",
  joinedAt: NOW - 200 * DAY,
  invited: false,
};

const DEVELOPER: Member = {
  _id: "m_dev",
  memberId: "7f2b3d5a-9c1e-4b26-8a4f-2d6e9b1c5f83",
  githubLogin: "dana",
  githubId: "2",
  firstName: "Dana",
  lastName: "Ruiz",
  email: "dana@clubria.test",
  role: "developer",
  status: "active",
  joinedAt: NOW - 40 * DAY,
  invited: false,
};

const CANDIDATE: Member = {
  _id: "m_cand",
  memberId: "1c8e4f6b-2a9d-4e37-b1c8-5f3a7d2e9b41",
  githubLogin: "sam",
  githubId: "3",
  firstName: "Sam",
  lastName: "Tran",
  email: "sam@clubria.test",
  role: "candidate",
  status: "active",
  joinedAt: NOW - 2 * DAY,
  invited: false,
};

const SUSPENDED: Member = {
  _id: "m_susp",
  memberId: "9d3a7c1e-5b8f-4c92-a6d1-3e7b9c2f5a84",
  githubLogin: "rowan",
  githubId: "4",
  firstName: "Rowan",
  lastName: "Fitzgerald-Whitmore",
  email: "rowan@clubria.test",
  role: "developer",
  status: "suspended",
  joinedAt: NOW - 90 * DAY,
  invited: false,
};

/**
 * The adversarial member. Everything here is a real failure mode: a name with no
 * spaces to break on, a right-to-left script mixed into a left-to-right row, an
 * email longer than any column, and empty strings where the UI expects text.
 */
const HOSTILE: Member = {
  _id: "m_hostile",
  // A full, unbroken 36-character UUID — the `overflow` scenario exists to
  // catch exactly this shape. A shorter stand-in here would not exercise it.
  memberId: "2e6b9d4a-8c1f-4a53-9b7e-6d1a3c8f5b92",
  githubLogin: "a".repeat(60),
  githubId: "5",
  firstName: "",
  lastName: "",
  email: `${"x".repeat(120)}@example.test`,
  role: "candidate",
  status: "suspended",
  joinedAt: NOW - DAY,
  invited: false,
};

const UNICODE: Member = {
  _id: "m_unicode",
  memberId: "5c1f8a3d-9b6e-4d74-8c2a-7e9b1d5f3a60",
  githubLogin: "田中さん",
  githubId: "6",
  firstName: "محمد",
  lastName: "الفارسي 🚀🚀🚀",
  email: "unicode@clubria.test",
  role: "developer",
  status: "active",
  joinedAt: NOW - 10 * DAY,
  invited: false,
};

/**
 * Somebody a lead recorded before they ever signed in.
 *
 * Everything a sign-in would have filled is empty — no name, no email — which
 * is the state the member list has to stay readable in, and the reason the row
 * is marked rather than left to look like a developer who never filled their
 * profile in. `joinedAt` is when they were invited.
 */
const INVITED: Member = {
  _id: "m_invited",
  memberId: "8b4d2f7a-3c6e-4915-b8d2-1f7a4c9e6b35",
  githubLogin: "priya",
  githubId: "7",
  firstName: "",
  lastName: "",
  email: "",
  role: "developer",
  status: "active",
  joinedAt: NOW - 3 * DAY,
  invited: true,
};

/**
 * An invited *lead*, which is the row most likely to be misread: it says lead
 * beside a name nobody has ever authenticated. Worth looking at precisely
 * because it is the one a reader would assume the worst about.
 */
const INVITED_LEAD: Member = {
  _id: "m_invited_lead",
  memberId: "3f9c1e5b-7d2a-4864-9c1e-5b8d3a7f2c94",
  githubLogin: "morgan",
  githubId: "8",
  firstName: "",
  lastName: "",
  email: "",
  role: "lead",
  status: "active",
  joinedAt: NOW - 6 * 60 * 60_000,
  invited: true,
};

/** What GitHub reports when a lead asks who is in the org. */
const ORG_CANDIDATES: OrgCandidate[] = [
  { login: "ilya", githubId: "1" },
  { login: "dana", githubId: "2" },
  { login: "priya", githubId: "7" },
  { login: "morgan", githubId: "8" },
  { login: "wren", githubId: "9" },
  { login: "kofi", githubId: "10" },
];

const ACTIVE_SESSION: Session = {
  _id: "s_active",
  deviceLabel: "dana-mbp-16",
  cliVersion: "2026.08.04",
  createdAt: NOW - 20 * DAY,
  lastUsedAt: NOW - 12 * MINUTE,
  expiresAt: NOW + 70 * DAY,
  revokedAt: null,
  origin: "device",
};

/**
 * A server signed in by the laptop above, which is what `riabuild remote`
 * produces. The label is a hostname rather than a person's machine, and the
 * row carries the extra line saying nobody approved this one in a browser.
 */
const DELEGATED_SESSION: Session = {
  _id: "s_delegated",
  deviceLabel: "build-01.fly.dev",
  cliVersion: "2026.08.04",
  createdAt: NOW - 3 * DAY,
  lastUsedAt: NOW - 40 * MINUTE,
  expiresAt: NOW + 87 * DAY,
  revokedAt: null,
  origin: "delegated",
};

const EXPIRED_SESSION: Session = {
  _id: "s_expired",
  deviceLabel: "old-thinkpad",
  cliVersion: "2026.06.11",
  createdAt: NOW - 120 * DAY,
  lastUsedAt: NOW - 95 * DAY,
  expiresAt: NOW - 30 * DAY,
  revokedAt: null,
  origin: "device",
};

const REVOKED_SESSION: Session = {
  _id: "s_revoked",
  deviceLabel: "borrowed-laptop",
  cliVersion: "2026.07.20",
  createdAt: NOW - 40 * DAY,
  lastUsedAt: NOW - 8 * DAY,
  expiresAt: NOW + 50 * DAY,
  revokedAt: NOW - 7 * DAY,
  origin: "device",
};

const HOSTILE_SESSION: Session = {
  _id: "s_hostile",
  deviceLabel:
    "MacBook-Pro-de-" + "Wolfeschlegelsteinhausenbergerdorff".repeat(9),
  cliVersion: "0.0.0-nightly+build.20260804.deadbeefcafebabe.longsuffix",
  createdAt: NOW - DAY,
  lastUsedAt: NOW - MINUTE,
  expiresAt: NOW + 89 * DAY,
  revokedAt: null,
  origin: "device",
};

const AUDIT: AuditEntry[] = [
  {
    _id: "a1",
    at: NOW - 30 * MINUTE,
    action: "role.set",
    actorLogin: "ilya",
    subjectLogin: "dana",
    meta: { from: "candidate", to: "developer" },
  },
  {
    _id: "a2",
    at: NOW - 3 * DAY,
    action: "member.suspend",
    actorLogin: "ilya",
    subjectLogin: "rowan",
    meta: { reason: "left the company" },
  },
  {
    _id: "a3",
    at: NOW - 5 * DAY,
    action: "session.revoke",
    actorLogin: "dana",
    subjectLogin: "dana",
    meta: { device: "borrowed-laptop" },
  },
  {
    _id: "a4",
    at: NOW - 9 * DAY,
    action: "org.secretsRotated",
    actorLogin: "ilya",
    subjectLogin: null,
    meta: {},
  },
  {
    _id: "a5",
    at: 0,
    action: "org.cliFloorRaised",
    actorLogin: null,
    subjectLogin: null,
    meta: { from: "2026.06.01", to: "2026.08.04" },
  },
];

const ORG: OrgConfig = {
  repoSlug: "Clubria/ai-builders-hub",
  claudeSettings: JSON.stringify(
    { permissions: { allow: ["Bash(pnpm *)"] }, model: "claude-opus-5" },
    null,
    2,
  ),
  claudeSettingsUpdatedAt: NOW - 14 * DAY,
  minCliVersion: "2026.08.04",
  latestCliVersion: "2026.08.04",
  secretsUpdatedAt: NOW - 9 * DAY,
  ngrokAuthTokenHint: "…tok3",
  ngrokAuthTokenUpdatedAt: NOW - 3 * DAY,
};

/**
 * The team's servers. The third one is the adversarial row: a name and a
 * hostname both at their limit, which is where the table runs out of room at
 * 380px and where a row would widen the page if `wrap-value` were forgotten.
 */
const SHARED_SERVERS: SharedServer[] = [
  {
    _id: "s_build",
    name: "build",
    host: "build-01.fly.dev",
    port: 22,
    user: "clubria",
    updatedAt: NOW - 30 * DAY,
  },
  {
    _id: "s_gpu",
    name: "gpu",
    host: "gpu.internal",
    port: 2222,
    user: "ada",
    updatedAt: NOW - 2 * DAY,
  },
  {
    _id: "s_long",
    name: "a".repeat(32),
    host: `${"long-hostname-segment.".repeat(4)}example.test`,
    port: 65535,
    user: "s".repeat(32),
    updatedAt: NOW - 5 * MINUTE,
  },
];

/**
 * The keys the org issues, in the three shapes that look different on screen:
 * one issued to several people, one issued to nobody yet, and one whose name
 * and fingerprint are both at their limit — which is where the table runs out
 * of room at 380px, the same reason `SHARED_SERVERS` carries a long row.
 *
 * An RSA fingerprint is the same 43 characters as an ed25519 one; what varies
 * is the type chip beside it, so both are here.
 */
const ISSUED_KEYS: IssuedKey[] = [
  {
    _id: "k_bastion",
    label: "prod-bastion",
    keyType: "ssh-ed25519",
    // Deliberately *not* the key `opensshKey.fixtures` holds. The interaction
    // test pastes that one and asserts its fingerprint appears in the preview;
    // a fixture row carrying the same fingerprint makes that assertion match
    // two elements and prove nothing.
    publicKey:
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPphI59nx1X/yP8/S7vZh9OrQ0JejkDp2YET7IoQTjJE",
    fingerprint: "SHA256:XK4vR3e9a+3qNQ+nfSt+o4oy7aE8e26VUu8HvxZNZbc",
    issuedTo: [DEVELOPER._id, LEAD._id],
    updatedAt: NOW - 9 * DAY,
  },
  {
    _id: "k_gpu",
    label: "gpu-box",
    keyType: "ssh-rsa",
    publicKey: `ssh-rsa AAAAB3NzaC1yc2E${"A".repeat(340)}`,
    fingerprint: "SHA256:MgxOF2TqJxTgu35QWHCJUOETjhUKOTGIgtmxvr0q+Hs",
    issuedTo: [],
    updatedAt: NOW - 180 * MINUTE,
  },
  {
    _id: "k_long",
    label: "a".repeat(32),
    keyType: "ecdsa-sha2-nistp521",
    publicKey: `ecdsa-sha2-nistp521 AAAAE2V${"B".repeat(200)}`,
    fingerprint: "SHA256:9Zq7vK3mN8pR2sT5wX1yA4bC6dE0fG7hJ9kL2mN4pQ8",
    issuedTo: [DEVELOPER._id, LEAD._id, CANDIDATE._id, SUSPENDED._id],
    updatedAt: NOW - 5 * MINUTE,
  },
];

const NOOP = async () => {};
const REJECT = async (): Promise<never> => {
  throw new Error(
    "[CONVEX M(members:setRole)] Uncaught Error: Only team leads can do that.",
  );
};

function base(viewer: Member | null): Data {
  return {
    auth: viewer === null ? "signed-out" : "signed-in",
    viewer: { state: "ready", value: viewer },
    membership: { org: "Clubria", status: viewer === null ? "signed_out" : "member" },
    sessions: { state: "ready", value: [ACTIVE_SESSION] },
    members: {
      state: "ready",
      value: [LEAD, DEVELOPER, CANDIDATE, SUSPENDED],
    },
    sharedServers: { state: "ready", value: SHARED_SERVERS },
    issuedKeys: { state: "ready", value: ISSUED_KEYS },
    auditLog: { state: "ready", value: AUDIT },
    orgConfig: { state: "ready", value: ORG },
    now: NOW,
    updateProfile: NOOP,
    setRole: NOOP,
    setStatus: NOOP,
    listOrgMembers: async () => ORG_CANDIDATES,
    inviteMember: NOOP,
    withdrawInvite: NOOP,
    revokeSession: NOOP,
    updateOrg: NOOP,
    addSharedServer: NOOP,
    updateSharedServer: NOOP,
    removeSharedServer: NOOP,
    addIssuedKey: NOOP,
    replaceIssuedKey: NOOP,
    setIssuedKeyMembers: NOOP,
    removeIssuedKey: NOOP,
    signIn: NOOP,
    signOut: NOOP,
    lookupDeviceCode: async () => PENDING_REQUEST,
    approveDeviceCode: async () => ({ status: "ok" }),
    denyDeviceCode: async () => ({ status: "ok" }),
  };
}

/** The machine a fixture developer is being asked to approve. */
const PENDING_REQUEST: DeviceRequest = {
  status: "pending",
  deviceLabel: "build-01.fly.dev",
  cliVersion: "2026.08.07",
  requestedAt: NOW - 40 * 1000,
  expiresAt: NOW + 14 * MINUTE,
};

export const SCENARIOS: Record<string, () => Data> = {
  loading: () => ({
    ...base(DEVELOPER),
    auth: "loading",
    viewer: { state: "loading" },
    sessions: { state: "loading" },
    orgConfig: { state: "loading" },
  }),

  "signed-out": () => base(null),

  candidate: () => base(CANDIDATE),
  developer: () => base(DEVELOPER),
  lead: () => base(LEAD),

  suspended: () => base({ ...DEVELOPER, status: "suspended" }),

  /**
   * A team whose lead has not set an ngrok authtoken. Ordinary, not broken —
   * riabuild still installs ngrok, and it runs unauthenticated until somebody
   * fills this in, which is what the settings screen has to say out loud.
   */
  "ngrok-unset": () => ({
    ...base(LEAD),
    orgConfig: {
      state: "ready" as const,
      value: { ...ORG, ngrokAuthTokenHint: "", ngrokAuthTokenUpdatedAt: 0 },
    },
  }),

  "not-member": () => ({
    ...base(DEVELOPER),
    membership: { org: "Clubria", status: "not_member" },
  }),

  "org-unavailable": () => ({
    ...base(DEVELOPER),
    membership: {
      org: "Clubria",
      status: "unavailable",
      detail: "GitHub returned 502",
    },
  }),

  "viewer-missing": () => ({
    ...base(DEVELOPER),
    viewer: { state: "ready", value: null },
  }),

  "sessions-empty": () => ({
    ...base(DEVELOPER),
    sessions: { state: "ready", value: [] },
  }),

  "sessions-one": () => ({
    ...base(DEVELOPER),
    sessions: { state: "ready", value: [ACTIVE_SESSION] },
  }),

  /**
   * The shape a developer sees after their first `riabuild remote`: their own
   * laptop, and the server that laptop signed in. Both rows together, because
   * the extra line only means anything next to a row that does not have it.
   */
  "sessions-delegated": () => ({
    ...base(DEVELOPER),
    sessions: {
      state: "ready",
      value: [ACTIVE_SESSION, DELEGATED_SESSION],
    },
  }),

  "sessions-many": () => ({
    ...base(DEVELOPER),
    sessions: {
      state: "ready",
      value: Array.from({ length: 24 }, (_, i) => ({
        ...ACTIVE_SESSION,
        _id: `s_${i}`,
        deviceLabel: `machine-${String(i).padStart(2, "0")}`,
        lastUsedAt: NOW - i * 3 * DAY,
      })),
    },
  }),

  "session-expired": () => ({
    ...base(DEVELOPER),
    sessions: { state: "ready", value: [EXPIRED_SESSION] },
  }),

  "session-revoked": () => ({
    ...base(DEVELOPER),
    sessions: { state: "ready", value: [REVOKED_SESSION] },
  }),

  "sessions-error": () => ({
    ...base(DEVELOPER),
    sessions: { state: "error", message: "Server Error: could not reach Convex" },
  }),

  "audit-empty": () => ({
    ...base(LEAD),
    auditLog: { state: "ready", value: [] },
  }),

  "audit-full": () => ({
    ...base(LEAD),
    auditLog: {
      state: "ready",
      value: Array.from({ length: 40 }, (_, i) => ({
        ...AUDIT[i % AUDIT.length],
        _id: `a_${i}`,
        at: NOW - i * 90 * MINUTE,
      })),
    },
  }),

  "members-empty": () => ({
    ...base(LEAD),
    members: { state: "ready", value: [] },
  }),

  /**
   * The member list holding people nobody has signed in as. Both an invited
   * developer and an invited lead, because the second is the row a reader would
   * otherwise misread as a live administrator.
   */
  "members-invited": () => ({
    ...base(LEAD),
    members: {
      state: "ready",
      value: [LEAD, INVITED_LEAD, DEVELOPER, INVITED, CANDIDATE],
    },
  }),

  /**
   * Everyone GitHub reports is already here. Reached by pressing the button, so
   * the fixture supplies only logins the base member list already holds.
   */
  "invite-nobody-left": () => ({
    ...base(LEAD),
    listOrgMembers: async () => [
      { login: "ilya", githubId: "1" },
      { login: "dana", githubId: "2" },
    ],
  }),

  /**
   * The org list could not be fetched. Its own state rather than a member-list
   * failure: the member list is fine, and what is missing is the one thing that
   * turns typing a name into picking one.
   */
  "invite-org-unreachable": () => ({
    ...base(LEAD),
    listOrgMembers: async (): Promise<never> => {
      throw new Error(
        "[CONVEX A(github:listOrgMembers)] Uncaught Error: GITHUB_ORG_TOKEN is not set " +
          "on the riabuild deployment, so the org's members cannot be listed.",
      );
    },
  }),

  /** The invitation came back refused — the person is already here. */
  "invite-refused": () => ({
    ...base(LEAD),
    inviteMember: async (): Promise<never> => {
      throw new Error(
        "[CONVEX M(members:invite)] Uncaught Error: @priya has already been invited.",
      );
    },
  }),

  /**
   * Inviting with no keys to give. The panel has to say why the row of key
   * toggles is missing rather than silently not being there — a lead who came
   * here to hand somebody a key would otherwise think the feature was broken.
   */
  "invite-no-keys": () => ({
    ...base(LEAD),
    issuedKeys: { state: "ready", value: [] },
  }),

  /** No shared servers yet — what a lead sees before they add the first one. */
  "shared-servers-empty": () => ({
    ...base(LEAD),
    sharedServers: { state: "ready", value: [] },
  }),

  /**
   * The address a lead typed came back refused. The message is the real one
   * riabuild-web sends for the rule that matters — a hostname `ssh` would read
   * as an option — because that is the sentence a lead has to be able to act on.
   */
  "shared-server-refused": () => ({
    ...base(LEAD),
    addSharedServer: async (): Promise<never> => {
      throw new Error(
        "[CONVEX M(sharedServers:add)] Uncaught Error: A hostname cannot start with a dash.",
      );
    },
  }),

  /** No issued keys yet — what a lead sees before they paste the first one. */
  "issued-keys-empty": () => ({
    ...base(LEAD),
    issuedKeys: { state: "ready", value: [] },
  }),

  /**
   * A key riabuild-web refused. The message is the real one, for the rule most
   * likely to be hit: a lead exporting a key from somewhere that protects it
   * with a passphrase. It has to say what to run, because "refused" alone
   * leaves them with a box that will not take their file and no way forward.
   */
  "issued-key-refused": () => ({
    ...base(LEAD),
    addIssuedKey: async (): Promise<never> => {
      throw new Error(
        "[CONVEX M(issuedKeys:create)] Uncaught Error: That key is protected by a " +
          "passphrase, and riabuild cannot use it: nothing would be able to answer the " +
          "prompt on a developer's machine. Remove the passphrase with " +
          "`ssh-keygen -p -f <file>` — leaving the new one empty — and paste it again.",
      );
    },
  }),

  "mutation-error": () => ({
    ...base(LEAD),
    updateProfile: REJECT,
    setRole: REJECT,
    setStatus: REJECT,
    inviteMember: REJECT,
    withdrawInvite: REJECT,
    revokeSession: REJECT,
    updateOrg: REJECT,
    addSharedServer: REJECT,
    updateSharedServer: REJECT,
    removeSharedServer: REJECT,
    addIssuedKey: REJECT,
    replaceIssuedKey: REJECT,
    setIssuedKeyMembers: REJECT,
    removeIssuedKey: REJECT,
  }),

  overflow: () => ({
    ...base({ ...LEAD, ...HOSTILE, role: "lead", status: "active" }),
    members: {
      state: "ready",
      value: [
        HOSTILE,
        UNICODE,
        SUSPENDED,
        // An invited row carrying the hostile login: three badges in the state
        // column and an unbroken 60-character name in the one beside it, which
        // is where a table with an extra badge runs out of room first.
        {
          ...HOSTILE,
          _id: "m_invited_hostile",
          githubLogin: `invited-${"b".repeat(52)}`,
          githubId: "99",
          role: "lead",
          status: "active",
          invited: true,
        },
      ],
    },
    // A 39-character login is what GitHub actually permits, and an option that
    // long is what widens a `<select>` past the column it sits in.
    listOrgMembers: async () => [
      { login: "z".repeat(39), githubId: "1001" },
      { login: "田中さんの非常に長い名前", githubId: "1002" },
    ],
    sessions: { state: "ready", value: [HOSTILE_SESSION, EXPIRED_SESSION] },
    orgConfig: {
      state: "ready",
      value: {
        ...ORG,
        repoSlug: `Clubria/${"very-long-repository-name".repeat(6)}`,
        claudeSettings: `{"note":"${"z".repeat(400)}"}`,
      },
    },
    auditLog: {
      state: "ready",
      value: [
        {
          _id: "a_overflow",
          at: NOW - MINUTE,
          action: "org.claudeSettingsUpdated",
          actorLogin: "a".repeat(60),
          subjectLogin: "田中さん",
          meta: { diff: "y".repeat(300), reason: "🚀".repeat(30) },
        },
        ...AUDIT,
      ],
    },
  }),

  /** Code prefilled from the terminal, machine found, waiting on a decision. */
  authorize: () => base(DEVELOPER),

  /** Landing on /cli with nothing typed yet — the empty code box. */
  "authorize-empty": () => base(DEVELOPER),

  "authorize-signed-out": () => base(null),

  "authorize-unknown": () => ({
    ...base(DEVELOPER),
    lookupDeviceCode: async () => ({ status: "unknown" as const }),
  }),

  "authorize-expired": () => ({
    ...base(DEVELOPER),
    lookupDeviceCode: async () => ({ status: "expired" as const }),
  }),

  "authorize-used": () => ({
    ...base(DEVELOPER),
    lookupDeviceCode: async () => ({ status: "used" as const }),
  }),

  /**
   * A device label long enough to break the panel if anything stopped wrapping.
   * Hostnames really do get this long once a cloud provider generates them.
   */
  "authorize-overflow": () => ({
    ...base(DEVELOPER),
    lookupDeviceCode: async () => ({
      ...PENDING_REQUEST,
      deviceLabel: `${"build-01.".repeat(9)}fly.dev`,
      cliVersion: "9".repeat(32),
    }),
  }),

  "authorize-error": () => ({
    ...base(DEVELOPER),
    lookupDeviceCode: async (): Promise<never> => {
      throw new Error(
        "[CONVEX Q(cliAuth:deviceRequest)] Uncaught Error: Not signed in.",
      );
    },
  }),

  boom: () => {
    throw new Error("Fixture scenario `boom` throws on purpose.");
  },
};

/** What `verificationUriComplete` puts in the address bar. */
const PREFILLED_CODE = "code=WXZB-CDFG";

/**
 * Scenarios that belong on `/cli` rather than `/`, and the query string each one
 * needs. The visual suite reads this to know which path to open.
 *
 * A scenario with no entry here still gets screenshotted — on `/`. Only the
 * ones that need a prefilled code appear.
 */
export const AUTHORIZE_QUERY: Record<string, string> = {
  authorize: PREFILLED_CODE,
  "authorize-empty": "",
  "authorize-signed-out": PREFILLED_CODE,
  "authorize-unknown": PREFILLED_CODE,
  "authorize-expired": PREFILLED_CODE,
  "authorize-used": PREFILLED_CODE,
  "authorize-overflow": PREFILLED_CODE,
  "authorize-error": PREFILLED_CODE,
};

export const SCENARIO_NAMES = Object.keys(SCENARIOS);

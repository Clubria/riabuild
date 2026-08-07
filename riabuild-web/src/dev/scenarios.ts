import {
  AuditEntry,
  Data,
  DeviceRequest,
  Member,
  OrgConfig,
  Session,
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
  githubLogin: "ilya",
  githubId: "1",
  firstName: "Ilya",
  lastName: "Konstantinov",
  email: "ilya@clubria.test",
  role: "lead",
  status: "active",
  joinedAt: NOW - 200 * DAY,
};

const DEVELOPER: Member = {
  _id: "m_dev",
  githubLogin: "dana",
  githubId: "2",
  firstName: "Dana",
  lastName: "Ruiz",
  email: "dana@clubria.test",
  role: "developer",
  status: "active",
  joinedAt: NOW - 40 * DAY,
};

const CANDIDATE: Member = {
  _id: "m_cand",
  githubLogin: "sam",
  githubId: "3",
  firstName: "Sam",
  lastName: "Tran",
  email: "sam@clubria.test",
  role: "candidate",
  status: "active",
  joinedAt: NOW - 2 * DAY,
};

const SUSPENDED: Member = {
  _id: "m_susp",
  githubLogin: "rowan",
  githubId: "4",
  firstName: "Rowan",
  lastName: "Fitzgerald-Whitmore",
  email: "rowan@clubria.test",
  role: "developer",
  status: "suspended",
  joinedAt: NOW - 90 * DAY,
};

/**
 * The adversarial member. Everything here is a real failure mode: a name with no
 * spaces to break on, a right-to-left script mixed into a left-to-right row, an
 * email longer than any column, and empty strings where the UI expects text.
 */
const HOSTILE: Member = {
  _id: "m_hostile",
  githubLogin: "a".repeat(60),
  githubId: "5",
  firstName: "",
  lastName: "",
  email: `${"x".repeat(120)}@example.test`,
  role: "candidate",
  status: "suspended",
  joinedAt: NOW - DAY,
};

const UNICODE: Member = {
  _id: "m_unicode",
  githubLogin: "田中さん",
  githubId: "6",
  firstName: "محمد",
  lastName: "الفارسي 🚀🚀🚀",
  email: "unicode@clubria.test",
  role: "developer",
  status: "active",
  joinedAt: NOW - 10 * DAY,
};

const ACTIVE_SESSION: Session = {
  _id: "s_active",
  deviceLabel: "dana-mbp-16",
  cliVersion: "2026.08.04",
  createdAt: NOW - 20 * DAY,
  lastUsedAt: NOW - 12 * MINUTE,
  expiresAt: NOW + 70 * DAY,
  revokedAt: null,
};

const EXPIRED_SESSION: Session = {
  _id: "s_expired",
  deviceLabel: "old-thinkpad",
  cliVersion: "2026.06.11",
  createdAt: NOW - 120 * DAY,
  lastUsedAt: NOW - 95 * DAY,
  expiresAt: NOW - 30 * DAY,
  revokedAt: null,
};

const REVOKED_SESSION: Session = {
  _id: "s_revoked",
  deviceLabel: "borrowed-laptop",
  cliVersion: "2026.07.20",
  createdAt: NOW - 40 * DAY,
  lastUsedAt: NOW - 8 * DAY,
  expiresAt: NOW + 50 * DAY,
  revokedAt: NOW - 7 * DAY,
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
};

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
    auditLog: { state: "ready", value: AUDIT },
    orgConfig: { state: "ready", value: ORG },
    now: NOW,
    updateProfile: NOOP,
    setRole: NOOP,
    setStatus: NOOP,
    revokeSession: NOOP,
    updateOrg: NOOP,
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

  "mutation-error": () => ({
    ...base(LEAD),
    updateProfile: REJECT,
    setRole: REJECT,
    setStatus: REJECT,
    revokeSession: REJECT,
    updateOrg: REJECT,
  }),

  overflow: () => ({
    ...base({ ...LEAD, ...HOSTILE, role: "lead", status: "active" }),
    members: { state: "ready", value: [HOSTILE, UNICODE, SUSPENDED] },
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

import { Data } from "./types";

/**
 * What the app runs on when there is no backend to talk to — a missing or
 * malformed `VITE_CONVEX_URL`.
 *
 * Without this, constructing the Convex client throws at module scope and the
 * visitor gets a blank white page. That is the worst possible failure for the
 * two screens that exist precisely for when things are broken: the 404 and the
 * error boundary. Everything degrades to a stated error instead.
 */
export function offlineData(message: string): Data {
  const failed = { state: "error", message } as const;
  const reject = async (): Promise<never> => {
    throw new Error(message);
  };

  return {
    auth: "signed-out",
    viewer: failed,
    membership: { org: "Clubria", status: "unavailable", detail: message },
    sessions: failed,
    members: failed,
    sharedServers: failed,
    issuedKeys: failed,
    auditLog: failed,
    orgConfig: failed,
    now: 0,
    updateProfile: reject,
    setRole: reject,
    setStatus: reject,
    listOrgMembers: reject,
    inviteMember: reject,
    withdrawInvite: reject,
    revokeSession: reject,
    updateOrg: reject,
    addSharedServer: reject,
    updateSharedServer: reject,
    removeSharedServer: reject,
    addIssuedKey: reject,
    replaceIssuedKey: reject,
    setIssuedKeyMembers: reject,
    removeIssuedKey: reject,
    signIn: reject,
    signOut: reject,
    lookupDeviceCode: reject,
    approveDeviceCode: reject,
    denyDeviceCode: reject,
  };
}

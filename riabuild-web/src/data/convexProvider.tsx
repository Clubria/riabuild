import {
  useAction,
  useConvex,
  useConvexAuth,
  useMutation,
  useQuery,
} from "convex/react";
import { useAuthActions } from "@convex-dev/auth/react";
import { ReactNode, useEffect, useRef, useState } from "react";
import { api } from "../../convex/_generated/api";
import { useNow } from "../lib/time";
import { DataContext } from "./context";
import { Data, Loadable, Membership, OrgUpdate } from "./types";

/**
 * The only file in `src/` that may import from `convex/react`.
 *
 * Keeping every query, mutation and action behind this one boundary is what
 * lets the fixture provider stand in for it. `pnpm lint` does not enforce that;
 * the check is:
 *
 *   grep -rn "convex/react" src/ --include=*.tsx | grep -v data/convexProvider
 *
 * which must return nothing.
 */
export function ConvexDataProvider({ children }: { children: ReactNode }) {
  const { isLoading, isAuthenticated } = useConvexAuth();
  const { signIn, signOut } = useAuthActions();
  const now = useNow();

  const viewer = useQuery(api.members.viewer, isAuthenticated ? {} : "skip");
  const isLead = viewer?.role === "lead";

  const sessions = useQuery(api.sessions.listMine, isAuthenticated ? {} : "skip");
  const orgConfig = useQuery(api.org.get, isAuthenticated ? {} : "skip");

  // Lead-only queries throw for everyone else, so they are not issued at all
  // rather than issued and allowed to fail.
  const members = useQuery(api.members.list, isLead ? {} : "skip");
  const auditLog = useQuery(
    api.members.auditLog,
    isLead ? { limit: 40 } : "skip",
  );
  const sharedServers = useQuery(api.sharedServers.list, isLead ? {} : "skip");
  const issuedKeys = useQuery(api.issuedKeys.list, isLead ? {} : "skip");

  const updateProfile = useMutation(api.members.updateProfile);
  const setRole = useMutation(api.members.setRole);
  const setStatus = useMutation(api.members.setStatus);
  const revoke = useMutation(api.sessions.revoke);
  const updateOrg = useMutation(api.org.update);
  const addSharedServer = useMutation(api.sharedServers.add);
  const updateSharedServer = useMutation(api.sharedServers.update);
  const removeSharedServer = useMutation(api.sharedServers.remove);
  const addIssuedKey = useMutation(api.issuedKeys.create);
  const replaceIssuedKey = useMutation(api.issuedKeys.replaceKey);
  const setIssuedKeyMembers = useMutation(api.issuedKeys.setIssuedTo);
  const removeIssuedKey = useMutation(api.issuedKeys.remove);
  const approveDevice = useMutation(api.cliAuth.approve);
  const denyDevice = useMutation(api.cliAuth.deny);
  /**
   * Imperative rather than `useQuery`: the code comes from a text box, so there
   * is nothing to subscribe to until a developer has finished typing one.
   */
  const convex = useConvex();

  const membership = useMembership(isAuthenticated);
  useStaleCredentialReset(isLoading, isAuthenticated, signOut);

  const data: Data = {
    auth: isLoading ? "loading" : isAuthenticated ? "signed-in" : "signed-out",
    viewer: loadable(viewer),
    membership,
    sessions: loadable(sessions),
    members: loadable(members),
    sharedServers: loadable(sharedServers),
    issuedKeys: loadable(issuedKeys),
    auditLog: loadable(auditLog),
    orgConfig: loadable(orgConfig),
    now,

    updateProfile: async (p) => {
      await updateProfile(p);
    },
    setRole: async (p) => {
      await setRole({ memberId: p.memberId as never, role: p.role });
    },
    setStatus: async (p) => {
      await setStatus({ memberId: p.memberId as never, status: p.status });
    },
    revokeSession: async (p) => {
      await revoke({ sessionId: p.sessionId as never });
    },
    updateOrg: async (p: OrgUpdate) => {
      await updateOrg(p);
    },
    addSharedServer: async (p) => {
      await addSharedServer(p);
    },
    updateSharedServer: async (p) => {
      await updateSharedServer({ ...p, id: p.id as never });
    },
    removeSharedServer: async (p) => {
      await removeSharedServer({ id: p.id as never });
    },
    addIssuedKey: async (p) => {
      await addIssuedKey(p);
    },
    replaceIssuedKey: async (p) => {
      await replaceIssuedKey({ id: p.id as never, privateKey: p.privateKey });
    },
    setIssuedKeyMembers: async (p) => {
      await setIssuedKeyMembers({
        id: p.id as never,
        issuedTo: p.issuedTo as never[],
      });
    },
    removeIssuedKey: async (p) => {
      await removeIssuedKey({ id: p.id as never });
    },
    signIn: async (p) => {
      await signIn("github", p?.redirectTo !== undefined ? { redirectTo: p.redirectTo } : {});
    },
    devSignIn: import.meta.env.DEV
      ? async (login: string) => {
          await signIn("dev", { login });
        }
      : undefined,
    signOut: async () => {
      await signOut();
    },
    lookupDeviceCode: async (p) =>
      await convex.query(api.cliAuth.deviceRequest, p),
    approveDeviceCode: async (p) => await approveDevice(p),
    denyDeviceCode: async (p) => await denyDevice(p),
  };

  return <DataContext.Provider value={data}>{children}</DataContext.Provider>;
}

/** Convex reports "not loaded yet" as `undefined`; a query that fails throws. */
function loadable<T>(value: T | undefined): Loadable<T> {
  return value === undefined ? { state: "loading" } : { state: "ready", value };
}

/**
 * `@convex-dev/auth` namespaces its storage keys as `${key}_${namespace}`, so
 * the refresh token is found by prefix rather than by an exact name we would
 * have to keep in step with the deployment URL.
 */
const REFRESH_TOKEN_KEY_PREFIX = "__convexAuthRefreshToken";

function hasStoredRefreshToken(): boolean {
  try {
    return Object.keys(window.localStorage).some((key) =>
      key.startsWith(REFRESH_TOKEN_KEY_PREFIX),
    );
  } catch {
    // Storage blocked by the browser holds no stale token either.
    return false;
  }
}

/**
 * Erases sign-in state the browser is holding and the deployment will not honour.
 *
 * `@convex-dev/auth` renews an access token by calling `verifyCode` with the
 * stored refresh token, and `verifyCode` *throws* when the deployment rejects
 * one — it retries network errors and rethrows everything else. Nothing catches
 * that, so a dead refresh token is never erased. The library means to recover
 * and does not: `fetchAccessToken` lists `signOut` in its dependency array and
 * never calls it, in 0.0.91 and in 0.0.95, whose `client.js` is byte-for-byte
 * identical. There is no upgrade to take and no setting to turn on.
 *
 * What that leaves behind is a *torn* pair. `setToken` writes the JWT and the
 * refresh token together and removes them together, so the two are meant to
 * agree; a resolved signed-out state alongside a stored refresh token means
 * they no longer do. The developer gets the sign-in screen, authorises on
 * GitHub, and arrives back at the sign-in screen, with nothing anywhere
 * reporting a problem — which cost us a debugging session before this existed.
 *
 * Clearing cannot race a live refresh: a refresh only runs while the old token
 * is still in hand, which is to say while `isAuthenticated` is true, and this
 * fires only once auth has settled the other way. `signOut()` does the erasing
 * because it is the library's own API — deleting keys whose names and layout
 * the library owns would be a second copy of its internals for us to maintain.
 *
 * Silent on purpose. What the developer wanted was a sign-in button that works,
 * and once this has run they have one; a notice would explain a state they
 * never chose and cannot act on.
 */
function useStaleCredentialReset(
  isLoading: boolean,
  isAuthenticated: boolean,
  signOut: () => Promise<void>,
): void {
  const cleared = useRef(false);

  useEffect(() => {
    if (cleared.current) return;
    if (isLoading || isAuthenticated) return;
    if (!hasStoredRefreshToken()) return;

    cleared.current = true;
    void signOut().catch(() => {
      // `signOut` already swallows the server call, so a rejection here means
      // storage itself refused. There is nothing better left to try.
    });
  }, [isLoading, isAuthenticated, signOut]);
}

const SIGNED_OUT: Membership = { org: "Clubria", status: "signed_out" };
const CHECKING: Membership = { org: "Clubria", status: "checking" };

/**
 * Checked on every page load rather than cached on the member row: losing org
 * membership should take effect on the next page view, not the next sign-in.
 */
function useMembership(isAuthenticated: boolean): Membership {
  const check = useAction(api.github.viewerOrgMembership);
  const [result, setResult] = useState<Membership | null>(null);

  useEffect(() => {
    if (!isAuthenticated) return;
    let live = true;
    void check({}).then(
      (value) => {
        if (live) setResult(value);
      },
      (cause: unknown) => {
        if (live) {
          setResult({
            org: "Clubria",
            status: "unavailable",
            detail: cause instanceof Error ? cause.message : String(cause),
          });
        }
      },
    );
    return () => {
      live = false;
    };
  }, [isAuthenticated, check]);

  if (!isAuthenticated) return SIGNED_OUT;
  return result ?? CHECKING;
}

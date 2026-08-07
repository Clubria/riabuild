import {
  useAction,
  useConvex,
  useConvexAuth,
  useMutation,
  useQuery,
} from "convex/react";
import { useAuthActions } from "@convex-dev/auth/react";
import { ReactNode, useEffect, useState } from "react";
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

  const updateProfile = useMutation(api.members.updateProfile);
  const setRole = useMutation(api.members.setRole);
  const setStatus = useMutation(api.members.setStatus);
  const revoke = useMutation(api.sessions.revoke);
  const updateOrg = useMutation(api.org.update);
  const approveDevice = useMutation(api.cliAuth.approve);
  const denyDevice = useMutation(api.cliAuth.deny);
  /**
   * Imperative rather than `useQuery`: the code comes from a text box, so there
   * is nothing to subscribe to until a developer has finished typing one.
   */
  const convex = useConvex();

  const membership = useMembership(isAuthenticated);

  const data: Data = {
    auth: isLoading ? "loading" : isAuthenticated ? "signed-in" : "signed-out",
    viewer: loadable(viewer),
    membership,
    sessions: loadable(sessions),
    members: loadable(members),
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

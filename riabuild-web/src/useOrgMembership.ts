import { useAction, useConvexAuth } from "convex/react";
import { useEffect, useState } from "react";
import { api } from "../convex/_generated/api";

export type OrgMembership = {
  org: string;
  status: "member" | "not_member" | "unavailable" | "signed_out" | "checking";
  detail?: string;
};

const SIGNED_OUT: OrgMembership = { org: "Clubria", status: "signed_out" };
const CHECKING: OrgMembership = { org: "Clubria", status: "checking" };

/**
 * Checked on every page load rather than cached on the member row: losing org
 * membership should take effect on the next page view, not the next sign-in.
 *
 * The signed-out case is derived rather than stored — it is already knowable
 * from auth state, and storing it would mean writing state inside an effect.
 */
export function useOrgMembership(): OrgMembership {
  const { isAuthenticated } = useConvexAuth();
  const check = useAction(api.github.viewerOrgMembership);
  const [result, setResult] = useState<OrgMembership | null>(null);

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

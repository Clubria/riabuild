import {
  useAction,
  useConvex,
  useConvexAuth,
  useMutation,
  useQuery,
} from "convex/react";
import { useAuthActions } from "@convex-dev/auth/react";
import type { FunctionReference, FunctionReturnType } from "convex/server";
import { ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../convex/_generated/api";
import { readError } from "../lib/errors";
import { useNow } from "../lib/time";
import { DataContext } from "./context";
import { Data, Loadable, Membership, OrgUpdate } from "./types";

/**
 * The only file in `src/` that may *use* `convex/react`.
 *
 * Keeping every query, mutation and action behind this one boundary is what
 * lets the fixture provider stand in for it. `src/main.tsx` is the one other
 * file that imports the module at all — it constructs the `ConvexReactClient`
 * and chooses between this provider and the fixtures — so both are named in
 * the check. `pnpm lint` does not enforce it; run this from `riabuild-web/`:
 *
 *   grep -rn "convex/react" src/ --include=*.tsx \
 *     | grep -Ev '^src/(data/convexProvider|main)\.tsx:'
 *
 * which must return nothing. Anchoring each exception to the start of the line
 * and to a whole filename is the point: `grep -v data/convexProvider` also
 * excused any future file whose *contents* happened to mention it.
 */
export function ConvexDataProvider({ children }: { children: ReactNode }) {
  const { isLoading, isAuthenticated } = useConvexAuth();
  const { signIn, signOut } = useAuthActions();
  const now = useNow();

  const viewer = useLoadable(api.members.viewer, isAuthenticated ? {} : "skip");
  const isLead = viewer.state === "ready" && viewer.value?.role === "lead";

  const sessions = useLoadable(
    api.sessions.listMine,
    isAuthenticated ? {} : "skip",
  );
  const orgConfig = useLoadable(api.org.get, isAuthenticated ? {} : "skip");

  // Lead-only queries throw for everyone else, so they are not issued at all
  // rather than issued and allowed to fail.
  const members = useLoadable(api.members.list, isLead ? {} : "skip");
  const auditLog = useLoadable(
    api.members.auditLog,
    isLead ? { limit: 40 } : "skip",
  );
  const sharedServers = useLoadable(
    api.sharedServers.list,
    isLead ? {} : "skip",
  );
  const repoSecretPaths = useLoadable(
    api.secretPaths.list,
    isLead ? {} : "skip",
  );
  const issuedKeys = useLoadable(api.issuedKeys.list, isLead ? {} : "skip");
  // The window the panel says it is showing. Named here rather than defaulted
  // silently on the server, so the two cannot disagree about what "last 7 days"
  // means.
  const usage = useLoadable(
    api.usage.rollup,
    isLead ? { windowDays: 7 } : "skip",
  );

  const updateProfile = useMutation(api.members.updateProfile);
  const setRole = useMutation(api.members.setRole);
  const inviteMember = useMutation(api.members.invite);
  const withdrawInvite = useMutation(api.members.removeInvite);
  const listOrgMembers = useAction(api.github.listOrgMembers);
  const setStatus = useMutation(api.members.setStatus);
  const revoke = useMutation(api.sessions.revoke);
  const updateOrg = useMutation(api.org.update);
  const addSharedServer = useMutation(api.sharedServers.add);
  const updateSharedServer = useMutation(api.sharedServers.update);
  const removeSharedServer = useMutation(api.sharedServers.remove);
  const setRepoSecretPaths = useMutation(api.secretPaths.set);
  const removeRepoSecretPaths = useMutation(api.secretPaths.remove);
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

  /**
   * Held apart from the data below so each of these keeps its identity across a
   * clock tick.
   *
   * `now` moves every thirty seconds and that has to rebuild `data` — a ticking
   * clock is the point of the field. Rebuilding the *callbacks* with it is what
   * made that tick expensive: every consumer re-rendered, and an effect
   * depending on one of these functions re-ran twice a minute. The way out
   * anybody reaches for is to leave the dependency out, which is the
   * `eslint-disable` in `CliAuthorize` this replaces. convex-js memoises what
   * `useMutation` and `useAction` return, so nothing in the list below moves on
   * its own and this object is built once, then spread into each `data` after
   * it.
   */
  const actions = useMemo(
    () => ({
      updateProfile: async (p: Parameters<Data["updateProfile"]>[0]) => {
        await updateProfile(p);
      },
      setRole: async (p: Parameters<Data["setRole"]>[0]) => {
        await setRole(p);
      },
      setStatus: async (p: Parameters<Data["setStatus"]>[0]) => {
        await setStatus(p);
      },
      listOrgMembers: async () => await listOrgMembers({}),
      inviteMember: async (p: Parameters<Data["inviteMember"]>[0]) => {
        await inviteMember(p);
      },
      withdrawInvite: async (p: Parameters<Data["withdrawInvite"]>[0]) => {
        await withdrawInvite(p);
      },
      revokeSession: async (p: Parameters<Data["revokeSession"]>[0]) => {
        await revoke(p);
      },
      updateOrg: async (p: OrgUpdate) => {
        await updateOrg(p);
      },
      addSharedServer: async (p: Parameters<Data["addSharedServer"]>[0]) => {
        await addSharedServer(p);
      },
      updateSharedServer: async (
        p: Parameters<Data["updateSharedServer"]>[0],
      ) => {
        await updateSharedServer(p);
      },
      removeSharedServer: async (
        p: Parameters<Data["removeSharedServer"]>[0],
      ) => {
        await removeSharedServer(p);
      },
      setRepoSecretPaths: async (
        p: Parameters<Data["setRepoSecretPaths"]>[0],
      ) => {
        await setRepoSecretPaths(p);
      },
      removeRepoSecretPaths: async (
        p: Parameters<Data["removeRepoSecretPaths"]>[0],
      ) => {
        await removeRepoSecretPaths(p);
      },
      addIssuedKey: async (p: Parameters<Data["addIssuedKey"]>[0]) => {
        await addIssuedKey(p);
      },
      replaceIssuedKey: async (p: Parameters<Data["replaceIssuedKey"]>[0]) => {
        await replaceIssuedKey(p);
      },
      setIssuedKeyMembers: async (
        p: Parameters<Data["setIssuedKeyMembers"]>[0],
      ) => {
        await setIssuedKeyMembers(p);
      },
      removeIssuedKey: async (p: Parameters<Data["removeIssuedKey"]>[0]) => {
        await removeIssuedKey(p);
      },
      signIn: async (p?: { redirectTo?: string }) => {
        await signIn(
          "github",
          p?.redirectTo !== undefined ? { redirectTo: p.redirectTo } : {},
        );
      },
      devSignIn: import.meta.env.DEV
        ? async (login: string) => {
            await signIn("dev", { login });
          }
        : undefined,
      signOut: async () => {
        await signOut();
      },
      lookupDeviceCode: async (p: { userCode: string }) =>
        await convex.query(api.cliAuth.deviceRequest, p),
      approveDeviceCode: async (p: { userCode: string }) =>
        await approveDevice(p),
      denyDeviceCode: async (p: { userCode: string }) => await denyDevice(p),
    }),
    [
      addIssuedKey,
      addSharedServer,
      approveDevice,
      convex,
      denyDevice,
      inviteMember,
      listOrgMembers,
      removeIssuedKey,
      removeRepoSecretPaths,
      removeSharedServer,
      replaceIssuedKey,
      revoke,
      setIssuedKeyMembers,
      setRepoSecretPaths,
      setRole,
      setStatus,
      signIn,
      signOut,
      updateOrg,
      updateProfile,
      updateSharedServer,
      withdrawInvite,
    ],
  );

  const data: Data = useMemo(
    () => ({
      auth: isLoading
        ? "loading"
        : isAuthenticated
          ? "signed-in"
          : "signed-out",
      viewer,
      membership,
      sessions,
      members,
      sharedServers,
      repoSecretPaths,
      issuedKeys,
      auditLog,
      usage,
      orgConfig,
      now,
      ...actions,
    }),
    [
      actions,
      auditLog,
      isAuthenticated,
      isLoading,
      issuedKeys,
      members,
      membership,
      now,
      orgConfig,
      repoSecretPaths,
      sessions,
      sharedServers,
      usage,
      viewer,
    ],
  );

  return <DataContext.Provider value={data}>{children}</DataContext.Provider>;
}

const LOADING: Loadable<never> = { state: "loading" };

/**
 * `useQuery`, in the shape a page can render a failure from.
 *
 * Convex reports "not loaded yet" as `undefined`, and reports a *failed* query
 * by throwing during render. Nothing caught that throw, so the six "Could not
 * list…" alerts and the one in `App` were unreachable in the real app: a query
 * that failed took the whole page to the error boundary instead, and the only
 * thing that ever reached those branches was a fixture.
 *
 * Catching it is safe with the rules of hooks, and not by luck. `useQuery` ends
 * with `if (result instanceof Error) throw result` — both hooks it owns, a
 * `useMemo` and `useQueries`, have already run by then, so a render that throws
 * calls exactly the hooks a render that does not. (`useQuery` in convex-js
 * `src/react/client.ts`.) If that ever changes the failure is loud rather than
 * subtle: React refuses the next render outright.
 *
 * The message is the sentence the backend wrote, which is what those alerts are
 * built to show. A production Convex deployment redacts a thrown `Error` to
 * "Server Error" before it leaves the server, so the detail arriving here is
 * detail somebody chose to send.
 */
function useLoadable<Query extends FunctionReference<"query">>(
  query: Query,
  args: Query["_args"] | "skip",
): Loadable<FunctionReturnType<Query>> {
  let value: FunctionReturnType<Query> | undefined;
  let failure: unknown;
  try {
    // The rule reads a `try` as a conditional call, and here it is not one:
    // `useQuery` is called on every render, and every hook it owns has run
    // before it can throw. The paragraph above is the argument; this is the one
    // place in `src/` allowed to make it.
    // eslint-disable-next-line react-hooks/rules-of-hooks
    value = useQuery(query, args as Query["_args"]);
  } catch (cause) {
    failure = cause;
  }

  // Read down to a string before it reaches the dependency list below: the
  // identity of whatever Convex threw is its own business, and this is the only
  // part of it anything renders.
  const message =
    failure === undefined
      ? null
      : readError(failure, "That could not be loaded.");

  return useMemo(
    () =>
      message !== null
        ? { state: "error", message }
        : value === undefined
          ? LOADING
          : { state: "ready", value },
    [message, value],
  );
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

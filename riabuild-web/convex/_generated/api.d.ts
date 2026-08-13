/* eslint-disable */
/**
 * Generated `api` utility.
 *
 * THIS CODE IS AUTOMATICALLY GENERATED.
 *
 * To regenerate, run `npx convex dev`.
 * @module
 */

import type * as audit from "../audit.js";
import type * as auth from "../auth.js";
import type * as cliAuth from "../cliAuth.js";
import type * as crons from "../crons.js";
import type * as devSeed from "../devSeed.js";
import type * as github from "../github.js";
import type * as http from "../http.js";
import type * as infisical from "../infisical.js";
import type * as lib_crypto from "../lib/crypto.js";
import type * as lib_responses from "../lib/responses.js";
import type * as lib_version from "../lib/version.js";
import type * as members from "../members.js";
import type * as org from "../org.js";
import type * as release from "../release.js";
import type * as sessions from "../sessions.js";
import type * as sharedServers from "../sharedServers.js";

import type {
  ApiFromModules,
  FilterApi,
  FunctionReference,
} from "convex/server";

declare const fullApi: ApiFromModules<{
  audit: typeof audit;
  auth: typeof auth;
  cliAuth: typeof cliAuth;
  crons: typeof crons;
  devSeed: typeof devSeed;
  github: typeof github;
  http: typeof http;
  infisical: typeof infisical;
  "lib/crypto": typeof lib_crypto;
  "lib/responses": typeof lib_responses;
  "lib/version": typeof lib_version;
  members: typeof members;
  org: typeof org;
  release: typeof release;
  sessions: typeof sessions;
  sharedServers: typeof sharedServers;
}>;

/**
 * A utility for referencing Convex functions in your app's public API.
 *
 * Usage:
 * ```js
 * const myFunctionReference = api.myModule.myFunction;
 * ```
 */
export declare const api: FilterApi<
  typeof fullApi,
  FunctionReference<any, "public">
>;

/**
 * A utility for referencing Convex functions in your app's internal API.
 *
 * Usage:
 * ```js
 * const myFunctionReference = internal.myModule.myFunction;
 * ```
 */
export declare const internal: FilterApi<
  typeof fullApi,
  FunctionReference<any, "internal">
>;

export declare const components: {};

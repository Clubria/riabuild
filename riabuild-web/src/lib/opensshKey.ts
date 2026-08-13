/**
 * The OpenSSH key parser, for the browser.
 *
 * One module, re-exported rather than reimplemented. The dashboard shows a
 * lead the public key and fingerprint as they paste, and `issuedKeys.create`
 * derives the values it actually stores — if those two ever disagreed, a lead
 * would confirm one fingerprint and the CLI would be handed another.
 *
 * The re-export exists so components import from `../lib/` like they do for
 * `errors` and `time`, rather than reaching two directories up into `convex/`.
 */

export {
  KeyParseError,
  parseOpenSshPrivateKey,
} from "../../convex/lib/opensshKey";
export type { ParsedKey } from "../../convex/lib/opensshKey";

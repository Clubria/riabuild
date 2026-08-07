/**
 * Token minting and hashing.
 *
 * Convex's runtime is V8, not Node: `crypto.subtle` and `crypto.getRandomValues`
 * are available, `node:crypto` is not. Every hash here is therefore async.
 */

const encoder = new TextEncoder();

/** base64url, no padding — safe in URLs, query params, and shell arguments. */
export function base64url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

export function randomToken(byteLength = 32): string {
  const bytes = new Uint8Array(byteLength);
  crypto.getRandomValues(bytes);
  return base64url(bytes);
}

export async function sha256Hex(input: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(input));
  return Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * The alphabet for the short code a developer reads off their terminal and
 * types into the dashboard.
 *
 * Consonants only, per RFC 8628 §6.1: with no vowels a code can never spell a
 * word by accident, which matters when it is displayed to a stranger over a
 * shared screen. `O`, `0`, `I`, `1` and `L` are absent because they are what
 * gets mistyped, and every mistype is a support question.
 */
export const USER_CODE_ALPHABET = "BCDFGHJKMNPQRSTVWXZ";

const USER_CODE_LENGTH = 8;

/**
 * 20^8 ≈ 2.6e10 codes against a fifteen-minute window.
 *
 * Rejection sampling rather than `byte % 20`: 256 is not a multiple of 20, so
 * the modulo would quietly favour the first sixteen letters. The bias would be
 * small and completely invisible, which is exactly the kind of thing that is
 * never found later.
 */
export function randomUserCode(): string {
  const limit = 256 - (256 % USER_CODE_ALPHABET.length);
  let code = "";
  while (code.length < USER_CODE_LENGTH) {
    const bytes = new Uint8Array(USER_CODE_LENGTH);
    crypto.getRandomValues(bytes);
    for (const byte of bytes) {
      if (byte >= limit) continue;
      code += USER_CODE_ALPHABET[byte % USER_CODE_ALPHABET.length];
      if (code.length === USER_CODE_LENGTH) break;
    }
  }
  return code;
}

/** `WXYZBCDF` → `WXYZ-BCDF`. Storage stays undashed; only display is grouped. */
export function formatUserCode(code: string): string {
  const half = Math.ceil(code.length / 2);
  return `${code.slice(0, half)}-${code.slice(half)}`;
}

/**
 * Whatever the developer typed, reduced to the form stored in `by_userCode`.
 *
 * Anything outside the alphabet is dropped rather than passed through — the
 * result is an index key, and a lookup should not depend on whether someone
 * pasted the dash, held shift, or picked up an en dash from a rendered page.
 */
export function normaliseUserCode(input: string): string {
  const upper = input.toUpperCase();
  let out = "";
  for (const character of upper) {
    if (USER_CODE_ALPHABET.includes(character)) out += character;
  }
  return out;
}

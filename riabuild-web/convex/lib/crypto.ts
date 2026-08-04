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
 * PKCE S256: base64url(SHA-256(verifier)).
 *
 * The CLI sends the challenge when it opens the browser and the verifier when it
 * redeems the code, so a code intercepted in the browser redirect is useless to
 * anyone who did not generate the verifier.
 */
export async function pkceChallenge(verifier: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    encoder.encode(verifier),
  );
  return base64url(new Uint8Array(digest));
}

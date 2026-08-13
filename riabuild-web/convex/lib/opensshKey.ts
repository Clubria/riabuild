/**
 * Deriving an SSH public key from a private one, with no key mathematics.
 *
 * An OpenSSH private key file *contains* its own public key, in the clear, as a
 * length-prefixed field sitting before the encrypted section. So there is
 * nothing here to compute: this is a container walk and one digest, which is
 * why the same logic runs in Convex's V8 runtime, in the browser as the lead
 * pastes, and — ported — in `crates/api/src/openssh.rs`, with no crypto library
 * at any of the three.
 *
 * Layout, after base64-decoding the body between the PEM markers:
 *
 *     "openssh-key-v1\0"
 *     string  ciphername        "none" when the key has no passphrase
 *     string  kdfname
 *     string  kdfoptions
 *     uint32  number of keys    1 in anything ssh-keygen writes
 *     string  publickey         <-- the whole public blob, in the clear
 *     string  encrypted section
 *
 * A `string` is a uint32 length followed by that many bytes, and every one of
 * those lengths comes from a file a human pasted into a box. [`Reader`] checks
 * each against the buffer it is reading, because a parser that trusted them
 * would take a developer's run down over a row a lead typed wrong — and the
 * "not an SSH key at all" case is by far the most likely thing to arrive here.
 */

const MAGIC = "openssh-key-v1";
const BEGIN = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END = "-----END OPENSSH PRIVATE KEY-----";

/**
 * A key riabuild will not take, and why, in words a lead reads in a browser.
 *
 * A distinct class rather than a bare `Error` so `issuedKeys.ts` can pass the
 * message through untouched while turning anything else into a generic
 * failure: these sentences are written for the person pasting, and reworded
 * they stop telling them what to do next.
 */
export class KeyParseError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "KeyParseError";
  }
}

export type ParsedKey = {
  /** `ssh-ed25519`, `ssh-rsa`, `ecdsa-sha2-nistp256`, … */
  keyType: string;
  /**
   * An ordinary `authorized_keys` line: `<keyType> <base64 blob>`.
   *
   * Deliberately without a comment. `ssh-keygen` writes one, it is free text a
   * lead never chose, and carrying it would make it part of the value two rows
   * are compared by.
   */
  publicKey: string;
  /** `SHA256:…`, unpadded — byte for byte what `ssh-keygen -lf` prints. */
  fingerprint: string;
};

/** A cursor over the length-prefixed fields OpenSSH serialises with. */
class Reader {
  private offset = 0;

  constructor(private readonly bytes: Uint8Array) {}

  uint32(): number {
    if (this.offset + 4 > this.bytes.length) {
      throw new KeyParseError("That key is truncated — it ends mid-field.");
    }
    const view = new DataView(
      this.bytes.buffer,
      this.bytes.byteOffset + this.offset,
      4,
    );
    this.offset += 4;
    return view.getUint32(0, false);
  }

  /**
   * `slice`, not `subarray`, so the bytes returned own their buffer.
   *
   * A view would keep the whole decoded private key alive behind every field
   * read out of it, and `crypto.subtle.digest` will not take a view whose
   * buffer TypeScript cannot prove is an `ArrayBuffer` rather than a
   * `SharedArrayBuffer`. Both problems go away with a copy, and the largest
   * thing copied here is one public key.
   */
  string(): Uint8Array<ArrayBuffer> {
    const length = this.uint32();
    if (this.offset + length > this.bytes.length) {
      throw new KeyParseError("That key is truncated — it ends mid-field.");
    }
    const slice = this.bytes.slice(this.offset, this.offset + length);
    this.offset += length;
    return slice;
  }
}

function decodeBase64(body: string): Uint8Array {
  let binary: string;
  try {
    binary = atob(body);
  } catch {
    throw new KeyParseError("That key's body is not valid base64.");
  }
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function encodeBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

const decoder = new TextDecoder();

export async function parseOpenSshPrivateKey(pem: string): Promise<ParsedKey> {
  const text = pem.trim();
  if (!text.startsWith(BEGIN) || !text.endsWith(END)) {
    throw new KeyParseError(
      `That is not an OpenSSH private key — it should begin with "${BEGIN}". ` +
        'If yours begins with "-----BEGIN RSA PRIVATE KEY-----" or ' +
        '"-----BEGIN PRIVATE KEY-----", convert it in place with ' +
        "`ssh-keygen -p -f <file>` and paste it again.",
    );
  }

  const body = text
    .slice(BEGIN.length, text.length - END.length)
    .replace(/\s+/g, "");
  const bytes = decodeBase64(body);

  if (
    bytes.length <= MAGIC.length ||
    decoder.decode(bytes.subarray(0, MAGIC.length)) !== MAGIC ||
    bytes[MAGIC.length] !== 0
  ) {
    throw new KeyParseError(
      "That file has the right header but is not in the openssh-key-v1 format.",
    );
  }

  const reader = new Reader(bytes.subarray(MAGIC.length + 1));
  const cipherName = decoder.decode(reader.string());
  reader.string(); // kdfname
  reader.string(); // kdfoptions

  if (cipherName !== "none") {
    // Refused here rather than on a laptop. `ssh-add` would prompt for this
    // passphrase mid-run, in a process riabuild is driving, with nobody who
    // knows the answer — a hang with no output and nothing to report.
    throw new KeyParseError(
      "That key is protected by a passphrase, and riabuild cannot use it: " +
        "nothing would be able to answer the prompt on a developer's machine. " +
        "Remove the passphrase with `ssh-keygen -p -f <file>` — leaving the new " +
        "one empty — and paste it again.",
    );
  }

  const count = reader.uint32();
  if (count !== 1) {
    throw new KeyParseError(
      `That file holds ${count} keys, and riabuild issues one key per entry.`,
    );
  }

  const publicBlob = reader.string();
  // The blob is itself a sequence of length-prefixed fields, and its first one
  // names the algorithm — the same string that opens an `authorized_keys` line.
  const keyType = decoder.decode(new Reader(publicBlob).string());
  if (!/^[a-z0-9@.-]{4,64}$/i.test(keyType)) {
    throw new KeyParseError(
      "That key does not name a key type riabuild recognises.",
    );
  }

  const digest = await crypto.subtle.digest("SHA-256", publicBlob);
  const fingerprint = `SHA256:${encodeBase64(new Uint8Array(digest)).replace(/=+$/, "")}`;

  return {
    keyType,
    publicKey: `${keyType} ${encodeBase64(publicBlob)}`,
    fingerprint,
  };
}

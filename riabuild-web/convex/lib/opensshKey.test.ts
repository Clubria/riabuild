import { describe, expect, it } from "vitest";
import { KeyParseError, parseOpenSshPrivateKey } from "./opensshKey";
import {
  ED25519_FINGERPRINT,
  ED25519_PRIVATE,
  ED25519_PUBLIC,
  ENCRYPTED_PRIVATE,
  RSA_FINGERPRINT,
  RSA_PRIVATE,
  RSA_PUBLIC,
} from "./opensshKey.fixtures";

describe("parseOpenSshPrivateKey", () => {
  it("derives the public half of an ed25519 key out of the private key itself", async () => {
    const parsed = await parseOpenSshPrivateKey(ED25519_PRIVATE);
    expect(parsed.keyType).toBe("ssh-ed25519");
    expect(parsed.publicKey).toBe(ED25519_PUBLIC);
    expect(parsed.fingerprint).toBe(ED25519_FINGERPRINT);
  });

  it("derives an rsa key the same way", async () => {
    // Nothing in the parser is per-algorithm — the public blob is a field in
    // the container, not something computed from the private scalar — and this
    // is the test that keeps it that way.
    const parsed = await parseOpenSshPrivateKey(RSA_PRIVATE);
    expect(parsed.keyType).toBe("ssh-rsa");
    expect(parsed.publicKey).toBe(RSA_PUBLIC);
    expect(parsed.fingerprint).toBe(RSA_FINGERPRINT);
  });

  it("refuses a passphrase-protected key, because nothing could answer the prompt", async () => {
    // `ssh-add` would ask for the passphrase on a developer's laptop, in the
    // middle of a run riabuild is driving, with nobody who knows the answer.
    // That is a hang with no output, so it is refused at the paste box.
    await expect(parseOpenSshPrivateKey(ENCRYPTED_PRIVATE)).rejects.toThrow(
      KeyParseError,
    );
    await expect(parseOpenSshPrivateKey(ENCRYPTED_PRIVATE)).rejects.toThrow(
      /passphrase/i,
    );
  });

  it("refuses anything that is not an OpenSSH private key", async () => {
    for (const junk of [
      "",
      "hello",
      // The other PEM a developer is likely to have to hand.
      "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----",
      // Correct markers, valid base64, wrong magic.
      "-----BEGIN OPENSSH PRIVATE KEY-----\nbm90LWEta2V5\n-----END OPENSSH PRIVATE KEY-----",
      // Correct markers, not base64 at all.
      "-----BEGIN OPENSSH PRIVATE KEY-----\n!!!!\n-----END OPENSSH PRIVATE KEY-----",
    ]) {
      await expect(parseOpenSshPrivateKey(junk)).rejects.toThrow(KeyParseError);
    }
  });

  it("refuses a truncated container rather than reading past the end of it", async () => {
    // Every length prefix in this format is attacker-supplied as far as the
    // parser is concerned. One that runs off the end must be an error, not a
    // silently short slice and not a crash.
    const lines = ED25519_PRIVATE.trim().split("\n");
    const truncated = [lines[0], lines[1], lines[lines.length - 1]].join("\n");
    await expect(parseOpenSshPrivateKey(truncated)).rejects.toThrow(
      KeyParseError,
    );
  });

  it("tolerates the whitespace a paste box introduces", async () => {
    const parsed = await parseOpenSshPrivateKey(
      `\n  ${ED25519_PRIVATE.trim()}  \n\n`,
    );
    expect(parsed.keyType).toBe("ssh-ed25519");
    expect(parsed.publicKey).toBe(ED25519_PUBLIC);
  });
});

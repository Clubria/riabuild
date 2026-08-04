import { describe, expect, test } from "vitest";
import { compareVersions, meetsMinimum, parseVersion } from "./version";
import { base64url, pkceChallenge, randomToken, sha256Hex } from "./crypto";

describe("version comparison", () => {
  test("orders by numeric component, not lexically", () => {
    expect(compareVersions("0.10.0", "0.9.0")).toBe(1);
    expect(compareVersions("1.0.0", "1.0.0")).toBe(0);
    expect(compareVersions("1.2.3", "1.2.4")).toBe(-1);
  });

  test("treats missing components as zero", () => {
    expect(compareVersions("1.2", "1.2.0")).toBe(0);
    expect(compareVersions("2", "1.9.9")).toBe(1);
  });

  test("ignores a leading v and prerelease suffix", () => {
    expect(parseVersion("v1.4.0-beta.2")).toEqual([1, 4, 0]);
    expect(meetsMinimum("v1.4.0-beta.2", "1.4.0")).toBe(true);
  });

  test("a malformed version never wedges a developer out", () => {
    // Garbage parses to zeroes rather than throwing: the CLI would otherwise
    // fail on a field it does not control.
    expect(() => meetsMinimum("not-a-version", "0.1.0")).not.toThrow();
    expect(meetsMinimum("not-a-version", "0.1.0")).toBe(false);
  });

  test("enforces the floor inclusively", () => {
    expect(meetsMinimum("0.1.0", "0.1.0")).toBe(true);
    expect(meetsMinimum("0.0.9", "0.1.0")).toBe(false);
  });
});

describe("token minting", () => {
  test("base64url output is URL and shell safe", () => {
    const token = randomToken(32);
    expect(token).toMatch(/^[A-Za-z0-9_-]+$/);
  });

  test("tokens do not repeat", () => {
    const tokens = new Set(Array.from({ length: 64 }, () => randomToken(32)));
    expect(tokens.size).toBe(64);
  });

  test("base64url drops padding", () => {
    expect(base64url(new Uint8Array([255, 255, 255]))).toBe("____");
    expect(base64url(new Uint8Array([1]))).toBe("AQ");
  });

  test("sha256 is stable and does not leak the input", async () => {
    const hash = await sha256Hex("hello");
    expect(hash).toBe(
      "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    );
    expect(await sha256Hex("hello")).toBe(hash);
    expect(await sha256Hex("hellp")).not.toBe(hash);
  });

  test("PKCE challenge matches the S256 definition", async () => {
    // The canonical example from RFC 7636 appendix B.
    expect(
      await pkceChallenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
    ).toBe("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
  });
});

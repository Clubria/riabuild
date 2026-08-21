import { describe, expect, test } from "vitest";
import { compareVersions, meetsMinimum, parseVersion } from "./version";
import {
  base64url,
  formatUserCode,
  normaliseUserCode,
  randomToken,
  randomUserCode,
  sha256Hex,
  USER_CODE_ALPHABET,
} from "./crypto";

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
});

describe("user codes", () => {
  test("the alphabet cannot spell a word or hide a typo", () => {
    // RFC 8628 §6.1: no vowels, so no code ever reads as a word by accident.
    expect(USER_CODE_ALPHABET).not.toMatch(/[AEIOU]/);
    // The characters a developer would mistype copying off a terminal.
    expect(USER_CODE_ALPHABET).not.toMatch(/[O0I1L]/);
    // Duplicates would quietly bias the distribution.
    expect(new Set(USER_CODE_ALPHABET).size).toBe(USER_CODE_ALPHABET.length);
  });

  test("a fresh code is eight characters from that alphabet", () => {
    const code = randomUserCode();
    expect(code).toHaveLength(8);
    for (const character of code) {
      expect(USER_CODE_ALPHABET).toContain(character);
    }
  });

  test("codes do not repeat", () => {
    const codes = new Set(Array.from({ length: 64 }, () => randomUserCode()));
    expect(codes.size).toBe(64);
  });

  test("display groups the code into halves", () => {
    expect(formatUserCode("WXZBCDFG")).toBe("WXZB-CDFG");
  });

  test("typing it back accepts whatever shape the developer used", () => {
    // Lowercase off a phone, the dash they read on screen, a stray space from a
    // paste: all the same code. Refusing any of them would be a support ticket.
    expect(normaliseUserCode("wxzb-cdfg")).toBe("WXZBCDFG");
    expect(normaliseUserCode("WXZBCDFG")).toBe("WXZBCDFG");
    expect(normaliseUserCode("  wxzb cdfg  ")).toBe("WXZBCDFG");
    expect(normaliseUserCode("WXZB–CDFG")).toBe("WXZBCDFG");
  });

  test("normalising leaves nothing that could widen a lookup", () => {
    // The result is used as an index key, so anything that is not alphabet is
    // dropped rather than passed through.
    expect(normaliseUserCode("WX*ZB/CD?FG")).toBe("WXZBCDFG");
  });
});

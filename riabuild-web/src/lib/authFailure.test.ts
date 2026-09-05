import { describe, expect, it } from "vitest";
import { hasAuthFailureMark, withoutAuthFailureMark } from "./authFailure";
import { CALLBACK_FAILED_PARAM } from "./authCallbackParam";

const ORIGIN = "https://riabuild.clubria.com";

describe("hasAuthFailureMark", () => {
  it("reads the mark the proxy writes", () => {
    expect(hasAuthFailureMark(`?${CALLBACK_FAILED_PARAM}=1`)).toBe(true);
  });

  it("is false for an ordinary visit", () => {
    expect(hasAuthFailureMark("")).toBe(false);
    expect(hasAuthFailureMark("?user_code=WXYZ-1234")).toBe(false);
  });

  it("does not need the value to be 1", () => {
    // The proxy writes `1` today. Nothing should turn a message off because a
    // future value spells the same fact differently.
    expect(hasAuthFailureMark(`?${CALLBACK_FAILED_PARAM}=`)).toBe(true);
  });
});

describe("withoutAuthFailureMark", () => {
  it("removes only the mark", () => {
    const url = new URL(
      `${ORIGIN}/cli?user_code=WXYZ-1234&${CALLBACK_FAILED_PARAM}=1`,
    );
    expect(withoutAuthFailureMark(url)).toBe("/cli?user_code=WXYZ-1234");
  });

  it("leaves a bare dashboard URL with nothing hanging off it", () => {
    const url = new URL(`${ORIGIN}/?${CALLBACK_FAILED_PARAM}=1`);
    expect(withoutAuthFailureMark(url)).toBe("/");
  });

  it("keeps the fragment", () => {
    const url = new URL(`${ORIGIN}/?${CALLBACK_FAILED_PARAM}=1#profile`);
    expect(withoutAuthFailureMark(url)).toBe("/#profile");
  });
});

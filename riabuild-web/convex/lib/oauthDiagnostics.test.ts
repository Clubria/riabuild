import { describe, expect, test } from "vitest";
import { describeOAuthFailure, loggingOAuthFetch } from "./oauthDiagnostics";

const TOKEN_URL = "https://github.com/login/oauth/access_token";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

describe("describing an OAuth response", () => {
  test("says nothing about a successful exchange", async () => {
    const response = jsonResponse(200, {
      access_token: "gho_a_real_looking_token",
      token_type: "bearer",
      scope: "read:user",
    });
    expect(await describeOAuthFailure(TOKEN_URL, response)).toBeNull();
  });

  // GitHub answers a stale or reused authorization code with HTTP *200* and an
  // error in the body. Without this, that arrives as a complaint about a
  // missing `access_token` and names no cause at all.
  test("reports an error carried by a 200", async () => {
    const described = await describeOAuthFailure(
      TOKEN_URL,
      jsonResponse(200, {
        error: "bad_verification_code",
        error_description: "The code passed is incorrect or expired.",
      }),
    );
    expect(described).toContain("bad_verification_code");
    expect(described).toContain("The code passed is incorrect or expired.");
    expect(described).toContain("HTTP 200");
  });

  test("reports a 4xx, which is what reaches the library's body-error branch", async () => {
    const described = await describeOAuthFailure(
      TOKEN_URL,
      jsonResponse(404, { error: "Not Found" }),
    );
    expect(described).toContain("HTTP 404");
    expect(described).toContain('error="Not Found"');
  });

  test("names the endpoint without its query string", async () => {
    const described = await describeOAuthFailure(
      `${TOKEN_URL}?client_secret=should_never_be_logged`,
      jsonResponse(400, { error: "invalid_request" }),
    );
    expect(described).toContain("github.com/login/oauth/access_token");
    expect(described).not.toContain("should_never_be_logged");
  });

  // A body that both failed and carried a token is the case worth being sure
  // about: the key may be named, the value may never appear.
  test("never repeats a token value", async () => {
    const described = await describeOAuthFailure(
      TOKEN_URL,
      jsonResponse(400, {
        error: "invalid_grant",
        access_token: "gho_this_must_not_be_logged",
        refresh_token: "ghr_this_must_not_be_logged",
      }),
    );
    expect(described).not.toContain("gho_this_must_not_be_logged");
    expect(described).not.toContain("ghr_this_must_not_be_logged");
    expect(described).toContain("access_token (redacted)");
    expect(described).toContain("refresh_token (redacted)");
  });

  test("copes with a body that is not JSON", async () => {
    const described = await describeOAuthFailure(
      TOKEN_URL,
      new Response("<html>gateway timeout</html>", {
        status: 504,
        headers: { "content-type": "text/html" },
      }),
    );
    expect(described).toContain("HTTP 504");
    expect(described).toContain("body was not readable JSON");
  });
});

describe("the logging fetch", () => {
  test("hands the response back unread", async () => {
    const upstream = jsonResponse(200, { access_token: "t", token_type: "b" });
    const fetchImpl = (async () => upstream) as unknown as typeof fetch;
    const logged: string[] = [];

    const response = await loggingOAuthFetch(fetchImpl, (m) => logged.push(m))(
      TOKEN_URL,
    );

    // The library still has to be able to read the body itself.
    expect(await response.json()).toEqual({
      access_token: "t",
      token_type: "b",
    });
    expect(logged).toEqual([]);
  });

  test("logs once when the exchange failed", async () => {
    const fetchImpl = (async () =>
      jsonResponse(404, { error: "Not Found" })) as unknown as typeof fetch;
    const logged: string[] = [];

    await loggingOAuthFetch(fetchImpl, (m) => logged.push(m))(TOKEN_URL);

    expect(logged).toHaveLength(1);
    expect(logged[0]).toContain('error="Not Found"');
  });
});

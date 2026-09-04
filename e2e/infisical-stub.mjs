/**
 * Stands in for app.infisical.com for the duration of an end-to-end run.
 *
 * Two different clients talk to it, and it is worth being explicit about which:
 *
 *   riabuild-web  POST /api/v1/auth/universal-auth/login   (convex/infisical.ts)
 *   infisical CLI GET  /api/v4/secrets                     (tasks/env_local.rs)
 *
 * The second one arrives once per environment *per folder* — riabuild pulling
 * `.env.<environment>` — and once more in stage 13, from the other end of the
 * same brokering, because a developer typed `infisical export` in the
 * environment shell and `~/.riabuild/bin/infisical` brokered a credential for
 * it. That last one carries the *primary* folder, which is the last one
 * `INFISICAL_SECRET_PATH` names.
 *
 * Everything between those two calls — brokering, the short-lived token, the
 * environment-not-arguments handoff, writing and git-ignoring one
 * `.env.<environment>` per environment the developer may see — is
 * riabuild's own code and runs unmodified. The only thing faked here is the
 * third-party host, which is exactly where the seam belongs: putting a real
 * Infisical machine identity into GitHub Actions would place the credential
 * that unlocks every dev secret in CI to test code we already own.
 *
 * Anything unrecognised gets a 501 that names the method and path. That matters
 * more than it looks: when the Infisical CLI changes which endpoint it calls —
 * it was /api/v3/secrets/raw before v4 — this fails as "the stub does not
 * implement GET /api/v5/whatever" instead of quietly returning nothing and
 * letting the run pass with an empty `.env.dev`.
 *
 * Usage: node infisical-stub.mjs
 *   Binds an ephemeral port on 127.0.0.1 and prints `listening <port>` as its
 *   first line of stdout, so the caller never has to guess a free port.
 *
 * Environment:
 *   STUB_CLIENT_ID / STUB_CLIENT_SECRET  credentials the login call must present
 */

import { createServer } from "node:http";

const CLIENT_ID = process.env.STUB_CLIENT_ID ?? "e2e-client-id";
const CLIENT_SECRET = process.env.STUB_CLIENT_SECRET ?? "e2e-client-secret";

/**
 * The secrets a successful run must end up with, per environment and per
 * folder.
 *
 * Deliberately recognisable: `run.sh` greps for these exact pairs, so a
 * `.env.<environment>` written from anything other than this stub fails the
 * assertion rather than passing because some file with the right name exists.
 *
 * The marker differs per environment on purpose. Serving one set of secrets for
 * every environment would let riabuild write staging's export into `.env.dev`
 * — or pull `dev` twice and name the second copy `.env.staging` — with every
 * assertion still passing. The value is what proves each file came from the
 * environment it is named after.
 *
 * **Two folders, because one environment's secrets are not always in one
 * folder.** This mirrors the layout AI Builders moved to on 2026-08-29 — the
 * `VITE_*` in one, the credentials in another — and the whole point is that
 * neither half alone is a working env file. `CLUBRIA_E2E_MARKER` is in *both*,
 * with a value the frontend folder must lose: riabuild exports the folders in
 * the order riabuild-web named them and later wins, so a run that merged them
 * the other way round passes every other assertion here and fails that one.
 */
const SECRETS_BY_ENVIRONMENT = {
  dev: {
    "/tenant/e2e/frontend": [
      { secretKey: "CLUBRIA_E2E_VITE_MARKER", secretValue: "baked-into-the-image" },
      { secretKey: "CLUBRIA_E2E_MARKER", secretValue: "overridden-by-the-frontend-folder" },
    ],
    "/tenant/e2e/convex": [
      { secretKey: "CLUBRIA_E2E_MARKER", secretValue: "brokered-through-riabuild" },
      { secretKey: "DATABASE_URL", secretValue: "postgres://e2e.invalid/clubria" },
      { secretKey: "OPENAI_API_KEY", secretValue: "sk-not-a-real-key" },
    ],
  },
  staging: {
    "/tenant/e2e/frontend": [
      {
        secretKey: "CLUBRIA_E2E_VITE_MARKER",
        secretValue: "baked-into-the-image-staging",
      },
      { secretKey: "CLUBRIA_E2E_MARKER", secretValue: "overridden-by-the-frontend-folder" },
    ],
    "/tenant/e2e/convex": [
      {
        secretKey: "CLUBRIA_E2E_MARKER",
        secretValue: "brokered-through-riabuild-staging",
      },
      { secretKey: "DATABASE_URL", secretValue: "postgres://e2e.invalid/clubria-staging" },
      { secretKey: "OPENAI_API_KEY", secretValue: "sk-not-a-real-staging-key" },
    ],
  },
};

/**
 * The Infisical CLI parses `INFISICAL_TOKEN` locally and rejects anything that
 * is not shaped like one of its own credentials *before* it makes a single HTTP
 * request — an opaque string like "stub-token" fails with "invalid service
 * token entered" and the stub never sees a connection. A universal-auth access
 * token is a JWT, so this is three base64url segments. Nothing verifies the
 * signature; the shape is the whole requirement.
 */
const ACCESS_TOKEN = [
  Buffer.from(JSON.stringify({ alg: "HS256", typ: "JWT" })).toString("base64url"),
  Buffer.from(
    JSON.stringify({ identityAccessTokenId: "e2e", authTokenType: "identityAccessToken" }),
  ).toString("base64url"),
  "e2e-signature-not-verified-by-anything",
].join(".");

const TOKEN_TTL_SECONDS = 300;

function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  res.end(payload);
}

function bearer(req) {
  const match = /^Bearer\s+(.+)$/i.exec(req.headers.authorization ?? "");
  return match === null ? null : match[1].trim();
}

const server = createServer((req, res) => {
  let body = "";
  req.on("data", (chunk) => (body += chunk));
  req.on("end", () => {
    const path = (req.url ?? "").split("?")[0];
    console.log(`${req.method} ${req.url}`);

    if (req.method === "POST" && path === "/api/v1/auth/universal-auth/login") {
      let credentials;
      try {
        credentials = JSON.parse(body);
      } catch {
        return json(res, 400, { message: "stub: login body is not JSON" });
      }
      if (
        credentials.clientId !== CLIENT_ID ||
        credentials.clientSecret !== CLIENT_SECRET
      ) {
        // 401 rather than a friendly message: this is the shape riabuild-web
        // turns into `not_configured`, and a run that trips it should fail.
        return json(res, 401, { message: "stub: wrong machine identity credentials" });
      }
      return json(res, 200, {
        accessToken: ACCESS_TOKEN,
        expiresIn: TOKEN_TTL_SECONDS,
        accessTokenMaxTTL: TOKEN_TTL_SECONDS,
        tokenType: "Bearer",
      });
    }

    if (req.method === "GET" && path === "/api/v4/secrets") {
      if (bearer(req) !== ACCESS_TOKEN) {
        // The CLI must be using the brokered token and nothing else. If riabuild
        // ever leaked a stored credential in here, this is what would catch it.
        return json(res, 401, { message: "stub: not the token this stub brokered" });
      }
      const query = new URL(req.url, "http://stub").searchParams;
      const environment = query.get("environment");
      const folders = SECRETS_BY_ENVIRONMENT[environment ?? ""];
      if (folders === undefined) {
        // Loud rather than empty, for the same reason the 501 below is: a
        // riabuild that asked for an environment nobody configured should fail
        // the run, not write a `.env.<name>` with nothing in it.
        return json(res, 404, {
          message:
            `stub: no secrets for environment ${JSON.stringify(environment)}. ` +
            `Known: ${Object.keys(SECRETS_BY_ENVIRONMENT).join(", ")}.`,
        });
      }
      // A folder is answered by name or not at all, which is what the real
      // instance does — a path that has been moved or deleted is a 404 naming
      // it, never an empty export. That is the shape of the failure a stale
      // `INFISICAL_SECRET_PATH` produces on a laptop, so the suite has to be
      // able to produce it too.
      const secretPath = query.get("secretPath");
      const secrets = folders[secretPath ?? ""];
      if (secrets === undefined) {
        return json(res, 404, {
          message:
            `stub: Folder with path ${JSON.stringify(secretPath)} in environment ` +
            `${JSON.stringify(environment)} was not found. ` +
            `Known: ${Object.keys(folders).join(", ")}.`,
        });
      }
      return json(res, 200, { secrets, imports: [] });
    }

    console.log(`  ^ unimplemented`);
    return json(res, 501, {
      message:
        `stub: no handler for ${req.method} ${path}. The Infisical CLI or ` +
        `riabuild-web changed which endpoint it calls; teach e2e/infisical-stub.mjs ` +
        `about it rather than deleting the assertion that failed.`,
    });
  });
});

server.listen(0, "127.0.0.1", () => {
  const { port } = server.address();
  console.log(`listening ${port}`);
});

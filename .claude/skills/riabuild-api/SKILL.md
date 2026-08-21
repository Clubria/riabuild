---
name: riabuild-api
description: Use when adding, changing, or reviewing a /api/v1 endpoint in riabuild-web, brokering Infisical credentials, authenticating a CLI session, or changing anything the Rust CLI consumes over HTTP
---

# The riabuild `/api/v1` contract

Endpoints under `/api/v1` are consumed by a Rust binary that developers install from
Homebrew and upgrade on their own schedule. **Old CLI versions are always in the field.**
Breaking an endpoint strands every developer who has not upgraded.

## Compatibility

Add fields. Never change a field's type or meaning, never remove one. Anything
incompatible gets a new version prefix, and both versions serve until `minCliVersion`
rules the old CLIs out.

`orgConfig.minCliVersion` is the mechanism for forcing an upgrade. Use it deliberately —
it hard-blocks people mid-workday.

## Every endpoint does these in order

```ts
http.route({
  path: "/api/v1/...",
  method: "POST",
  handler: httpAction(async (ctx, req) => {
    // 1. Authenticate the CLI session: hash the bearer token, look it up in
    //    cliSessions, reject revoked or expired, update lastUsedAt.
    // 2. Load the member; reject unless status === "active".
    // 3. Re-verify Clubria GitHub org membership. Not optional — see below.
    // 4. Narrow the body: treat `await req.json()` as unknown, 400 on bad shape.
    // 5. Do the work.
    // 6. Write an auditLog entry if this changed access.
  }),
});
```

## Non-negotiables

**Re-verify GitHub org membership on every secret-brokering request.** Identity lives in
GitHub; only authorization lives in Convex. Someone removed from the org must lose access
immediately, without anyone remembering to update their Convex row. `members.role` is
never the sole gate.

**Store tokens hashed.** `cliSessions.tokenHash`, never a raw token. A Convex data leak
must not hand out live sessions.

**Never return anything executable.** The server ships data — settings JSON, repo slug,
version floors, brokered tokens. A server-driven task manifest would be remote code
execution on every developer's laptop. This boundary is load-bearing.

**Never return an Infisical secret payload.** Broker a short-lived access token and let
the CLI fetch the secrets itself. Path scoping belongs to Infisical's RBAC, not to our
code.

**Two shipped responses do carry a durable credential, and a third would be a decision
rather than a detail.** `GET /api/v1/issued-keys` returns a private SSH key and
`GET /api/v1/org/ngrok-token` returns the team's ngrok authtoken. Say the cost out loud
rather than discovering it later: both are stored in Convex in plaintext, and a dump of
that database hands out working SSH access and the team's ngrok account. They exist
because the alternative is not a brokered credential — it is a key arriving over Slack
and living in someone's `~/.ssh` for ever. Neither expires on its own, so three
conditions do the work an expiry does everywhere else, and an endpoint that carries a
durable credential without all three is a bug:

- **write-only from a browser.** A lead sets the value and gets back a fingerprint or the
  last four characters. `org.get` returns `publicConfigView` and `org.forApi` returns the
  value, and they are two validators on purpose — one validator serving both a browser
  and the CLI is how a secret reaches a browser by omission instead of by decision.
- **the GitHub org re-check is the whole gate.** There is no expiry behind it, so a
  developer who left yesterday must lose the credential today with nobody remembering to
  edit a Convex row.
- **every *fetch* is audited, not merely every change.** For ngrok that row is the only
  attribution that exists, since one account carries the whole team; for an issued key it
  is the record of who took a copy of what.

`riabuild-web/CLAUDE.md` is the authority on both, and
`docs/superpowers/specs/2026-08-13-issued-ssh-keys-design.md` and
`docs/superpowers/specs/2026-08-18-ngrok-design.md` are why they are bounded the way they
are.

**Log access changes.** Role promotion, suspension, session revocation, and token
brokering all write `auditLog`.

## Secret brokering

Infisical service tokens are deprecated (announced April 2024, migration deadline July
2024). Use machine identities with universal auth:

```
POST /api/v1/secrets/token
  → authenticate session, confirm active, re-verify org membership
  → pick the machine identity for members.role:
        candidate          → mi-candidate  (subset of dev paths)
        developer | lead   → mi-developer  (all dev paths)
  → universal-auth login with that identity's client id + secret
  → return { token, expiresAt }
```

The identity credentials are Convex environment variables. They never leave the server.

## Errors

The CLI surfaces these directly to a developer who may not be technical. Return
`{ error: { code, message, action } }` where `message` says what went wrong in their
terms and `action` says what to do about it.

| Status | When |
|---|---|
| 401 | missing, invalid, revoked, or expired session |
| 403 | authenticated but not permitted — includes losing org membership |
| 409 | CLI version below `minCliVersion` |
| 400 | malformed body |

403 for lost org membership rather than 401 matters: 401 makes the CLI try to
re-authenticate, which will succeed and loop. 403 tells it to stop and explain.

## Convex conventions

Follow `convex/_generated/ai/guidelines.md` — it is generated for the pinned version and
is the authority on syntax. Always declare `args` and `returns` validators. Anything not
called from a client is `internalQuery` / `internalMutation` / `internalAction`.

## Common mistakes

**Trusting `members.role` alone.** The most likely path to a departed developer keeping
secret access.

**Adding a required request field.** Every CLI in the field omits it. Add optional fields
and default them.

**Returning raw Infisical errors.** They mention paths and identity names the developer
has no context for. Translate.

**Putting the org membership check only at sign-in.** It has to run at brokering time.
Sign-in was possibly months ago.

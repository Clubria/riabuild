# Device-code login

**Date:** 2026-08-07
**Status:** Implemented
**Supersedes:** the loopback section of `2026-08-04-riabuild-design.md`

## Problem

`riabuild login` binds an ephemeral port on `127.0.0.1`, sends the developer to the
dashboard, and has the dashboard redirect the browser back to that port with a one-time
code.

That works when the terminal and the browser are on the same machine. Over SSH they are
not. The CLI binds a port on the *server*; the browser that opens the link is on the
*laptop*; the redirect to `http://127.0.0.1:51234/callback` resolves on the laptop, where
nothing is listening. The developer sees a browser error, the terminal sits silently, and
three minutes later riabuild reports that the browser never came back.

Nothing in the design detects this, because from the CLI's point of view an SSH session
and a desktop session are indistinguishable.

## Approach

Replace loopback with a polling flow shaped after RFC 8628 (OAuth 2.0 Device
Authorization Grant) — the flow `gh` falls back to on headless machines. The CLI never
listens on a socket; it asks the server for a code, prints it, and polls until the
developer approves in whatever browser they have.

This is a **breaking change**. The loopback endpoint, the PKCE exchange, the
`/cli/authorize` screen, and the `cliAuthCodes` table are removed outright rather than
kept alongside. `orgConfig.minCliVersion` is raised to the release that carries this.

### What loopback was doing that polling must replace

Loopback was not only plumbing. The one-time code travelled back through a socket that
only a process on the developer's own machine could bind, so an attacker who tricked
someone into opening a riabuild authorize URL still could not receive the code.

Polling gives that up, so it has to be bought back the way RFC 8628 buys it: two codes
with different jobs.

| | `deviceCode` | `userCode` |
|---|---|---|
| Held by | the CLI process only | shown in the terminal, typed into the browser |
| Entropy | 256 bits | 8 chars over a 20-letter alphabet (~34 bits) |
| At rest | hashed (`deviceCodeHash`) | plaintext, indexed |
| Exchangeable for a session | yes | no |

The user code **identifies** a pending request; the device code **authenticates** it.
Storing the user code in plaintext is safe precisely because holding one grants nothing:
the worst an attacker who guesses a live code can do is approve someone else's CLI into
that someone else's session.

The alphabet is `BCDFGHJKLMNPQRSTVWXZ` — consonants only, per RFC 8628 §6.1. No vowels
means no code ever spells a word by accident; dropping `O/0/I/1/L` means nobody mistypes
one off a terminal.

PKCE is deleted. The `verifier`/`challenge` pair existed to protect a code travelling
through a browser redirect. No code travels through a browser now, so it protects
nothing, and `pkceChallenge()` goes with it.

## Wire protocol

Shaped after RFC 8628, but written in this codebase's conventions — camelCase fields and
the `{ error: { code, message, action } }` envelope. riabuild is not an OAuth server and
pretending otherwise would mean two error shapes for the CLI to understand.

### `POST /api/v1/cli/device`

Unauthenticated — this is how a machine *becomes* authenticated. Enforces
`minCliVersion`, which makes this the first endpoint on which the version floor reaches a
machine that has never signed in. Until now `minCliVersion` was only readable through
`GET /api/v1/org/config`, which requires a session, so an unsigned machine on an old
build could never learn it had to upgrade.

```
→ { "deviceLabel": "build-01.fly.dev" }

← 200 { "deviceCode": "<43-char base64url>",
        "userCode": "WXYZ-1234",
        "verificationUri": "https://riabuild.clubria.com/cli",
        "verificationUriComplete": "https://riabuild.clubria.com/cli?code=WXYZ-1234",
        "expiresIn": 900,
        "interval": 5 }

← 409 cli_too_old
```

`verificationUri` is built from the deployment's existing `SITE_URL`, not from a new
variable and not from anything the CLI knows. The server is the thing that knows where
the dashboard is deployed; a copy of that answer in the binary could disagree with it,
and the symptom would be a verification link pointing somewhere nobody is signed in.

`expiresIn` and `interval` are **relative seconds**, unlike `expiresAt` everywhere else
in the API. Deliberate: riabuild's first run happens on freshly provisioned machines
where NTP may not have settled, and a skewed clock would make the CLI abandon a live code
or keep polling a dead one. A duration is immune to that; a timestamp is not.

### `POST /api/v1/cli/token`

```
→ { "deviceCode": "…" }

← 200 { "status": "pending", "interval": 5 }
← 200 { "status": "denied" }
← 200 { "status": "ok", "token": "…", "expiresAt": …, "member": { … } }

← 401 unauthenticated   unknown, expired, or already-redeemed device code
← 403 suspended         approved, then the account was suspended before the poll landed
```

Polling states are **200 with a discriminated body**, not RFC 8628's `400
authorization_pending`. "Not yet" is the expected case in a loop that runs a hundred
times per login, and the CLI's `interpret()` in `api/mod.rs` turns every non-2xx into an
`ApiError` that unwinds. Encoding the normal path as an error would mean unwinding on
every tick and reconstructing the happy path from an error code.

The success body is unchanged from the old exchange — `{ token, expiresAt, member }` —
so everything downstream of `auth::login` is untouched.

## Data

`cliAuthCodes` is replaced by `cliDeviceCodes`, with one structural inversion worth
naming: a `cliAuthCodes` row was created **at approval time** and therefore always knew
its `memberId`. A device-code row is created **before anyone is known**, so `memberId`,
`approvedAt` and `deniedAt` are all optional until a human acts.

```ts
cliDeviceCodes: defineTable({
  deviceCodeHash: v.string(),
  userCode: v.string(),            // normalised: uppercase, no dash
  deviceLabel: v.string(),
  cliVersion: v.string(),
  expiresAt: v.number(),
  memberId: v.optional(v.id("members")),
  approvedAt: v.optional(v.number()),
  deniedAt: v.optional(v.number()),
  consumedAt: v.optional(v.number()),
})
  .index("by_deviceCodeHash", ["deviceCodeHash"])
  .index("by_userCode", ["userCode"])
  .index("by_expiresAt", ["expiresAt"])
```

That inversion has a consequence: an **unauthenticated endpoint now writes rows**, and
abandoned logins became the common case. Under loopback, walking away from a login wrote
nothing at all — the row only appeared once someone clicked approve. Now every
`riabuild login` leaves a row whether or not a human ever looks at it.

So `convex/crons.ts` sweeps hourly, deleting anything past `expiresAt + 1h` through the
`by_expiresAt` index. That is the whole mitigation: each row is a couple of hundred bytes
with a fifteen-minute life, and Convex is the backstop for anything pathological. Rate
limiting the endpoint is deliberately not in scope.

Because codes are reaped rather than reserved forever, a `userCode` string can recur
across rows. Every lookup therefore takes the newest row for that code
(`.withIndex(...).order("desc").first()`) rather than `.unique()`, which would throw the
first time a code was reused.

Minting happens in the HTTP action, not a mutation — Convex mutations are deterministic
and their randomness is seeded, so secrets must come from the action runtime's
`crypto.getRandomValues`. `cliAuth.authorize` consequently stops being an `action` and
becomes a plain `mutation`: with minting moved out, nothing in the approval path needs
the action runtime any more.

The action generates a candidate user code and the mutation rejects it if a live row
already holds it; the action retries up to five times. At 20⁸ ≈ 2.6 × 10¹⁰ codes against
a fifteen-minute window this never fires, but a silent collision would hand one
developer's approval screen to another developer's terminal.

## Dashboard

`/cli/authorize` becomes `/cli`. The old path carried the entire loopback flow in its
query string (`state`, `challenge`, `port`), none of which exists any more.

Signed out, the page keeps today's behaviour: sign in with GitHub and come back, with the
typed code preserved across the round trip.

Signed in, it is two steps:

1. **Enter the code.** Uppercased and dash-grouped as it is typed; paste works with or
   without the dash. `verificationUriComplete` prefills the field.
2. **Approve or deny.** The device label, CLI version and expiry countdown are shown
   first.

**`verificationUriComplete` prefills but never auto-approves.** A one-click approval URL
would hand back exactly the phishing hole that loopback was closing — the developer must
see which machine is asking and act. The click is the security control, not a formality.

Convex functions:

| | kind | notes |
|---|---|---|
| `cliAuth.deviceRequest` | `query` | requires a signed-in viewer, so the codespace is not an open oracle |
| `cliAuth.approve` | `mutation` | viewer must be an active member; writes `cli.device_approved` |
| `cliAuth.deny` | `mutation` | writes `cli.device_denied` |
| `cliAuth.startDevice` | `internalMutation` | called by `/api/v1/cli/device` |
| `cliAuth.redeem` | `internalMutation` | called by `/api/v1/cli/token` |

The `Data` contract in `src/data/types.ts` loses `authorizeCli` and `handOffToCli` and
gains `lookupDeviceCode`, `approveDeviceCode`, `denyDeviceCode`. `handOffToCli` existed
only to make the loopback redirect interceptable by the fixture provider; with no
redirect there is nothing to intercept.

## CLI

`api/auth.rs` loses `TcpListener`, `parse_callback`, `urldecode`, `respond`,
`wait_for_code` and `LoginFlow`. It gains `start_device` and a poll loop.

Per `riabuild-cli/CLAUDE.md`, the decisions stay pure and unit-tested while the loop
around them stays thin — the same split that made `parse_callback` testable without a
socket:

- `format_user_code` — grouping and dashes
- `PollResponse` — a serde-tagged enum, so the wire contract is tested by deserialising
  fixtures rather than by mocking a server
- `poll_delay` — clamps a server-supplied interval into `[1s, 60s]`, defaulting to 5s, so
  a malformed or hostile `interval` cannot spin the CLI or park it for an hour
- `browser_available` — takes the environment as data rather than reading it, so the
  headless decision is testable
- `verification_link` — picks `verificationUriComplete` over `verificationUri`, falling
  back when the server sends no complete link

Browser opening becomes best-effort: skipped when `SSH_CONNECTION` is set, or on Linux
when neither `DISPLAY` nor `WAYLAND_DISPLAY` is. The URL and the code are printed
regardless, so the SSH path is plain text with no failed process spawn in it.

**The printed URL is the one that prefills.** It is the same link the local browser is
sent to, and the developer who most needs the code already in the box is the one over
SSH — nothing is opened for them, so they carry the link to a browser on another machine
themselves. Printing the bare `/cli` there made the only person copying by hand copy
twice. The code keeps its own line regardless: it is what the terminal is checked
against, and prefilling still stops short of approving.

### Deliberately not done

**A too-old CLI is told to upgrade, not upgraded.** `POST /api/v1/cli/device` returns
409 `cli_too_old` with "Run `brew upgrade clubria/tap/riabuild`", and login stops there.
Wiring `update::upgrade_and_reexec` into the login path would make it automatic and is
arguably more in riabuild's spirit, but re-execing from inside a task is a control-flow
change that belongs to the update mechanism rather than to this one. First contact with
riabuild is an install command anyway, so the developer is already at a package manager.

## Rollout

This is a breaking change with no compatibility path, so the order matters:

1. **Merge and deploy riabuild-web.** `/api/v1/cli/device` has to exist before any binary
   asks for it, and the old `{ code, verifier }` body stops working the moment this
   deploys — a CLI mid-login when it lands gets a 400 and has to run `riabuild login`
   again. That is the whole cost of the cutover.
2. **Cut a release** (`docs/releasing.md`).
3. **A lead raises `minCliVersion`** to that release in the dashboard's lead panel.

Step 3 is not automatic and must not be: `docs/releasing.md` is explicit that raising the
floor interrupts whatever everyone is doing the moment they next launch riabuild, so it is
a decision rather than a consequence of shipping. Until it happens, an already-signed-in
developer on an older binary keeps working from their existing session and only breaks
when that session expires; a developer who is *not* signed in gets a 400 from
`/api/v1/cli/token` with no useful explanation.

That gap is the argument for doing step 3 promptly rather than for skipping it. Once the
floor is raised, `/api/v1/cli/device` answers an old binary with a 409 naming the upgrade
command — the version floor's first reach onto an unsigned machine.

## Testing

- `convex/api.test.ts`: full device flow; polling before approval; denial; approval by a
  suspended member; expired code; a device code redeemed twice; user code stored
  plaintext while the device code is stored hashed; `minCliVersion` enforced on
  `/api/v1/cli/device`.
- Rust: the four pure functions above, plus the existing `login` task checks.
- `src/dev/scenarios.ts` gains a scenario per new screen state (code entry, found,
  unknown code, expired, denied, approved) — the rule that keeps the visual suite honest
  is that a state with no scenario is a state nobody has looked at.
- e2e `smoke.spec.ts` and `visual.spec.ts` move from `/cli/authorize` to `/cli`.

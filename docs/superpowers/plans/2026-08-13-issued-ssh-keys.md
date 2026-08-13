# Issued SSH Keys Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A lead pastes a private SSH key into riabuild-web and names who it is issued to; those developers' CLIs pull it and use it to reach a server riabuild's own key cannot sign in to.

**Architecture:** The private key is stored in Convex (a third, documented exception to "secrets are brokered, never stored"). Its public half and fingerprint are *derived* by parsing the OpenSSH container — which holds the public key verbatim — so no crypto beyond SHA-256 is needed, and the same parser runs in Convex, the browser, and Rust. On the CLI the key is held only in an `ssh-agent` riabuild owns and never on a filesystem; it authenticates exactly one `ssh-copy-id`, after which this laptop's own key carries the run.

**Tech Stack:** Convex (V8 runtime, `crypto.subtle`), React + the `src/ui` fake-TUI library, Rust (tokio, `ring` for SHA-256, new `base64` crate).

**Spec:** `docs/superpowers/specs/2026-08-13-issued-ssh-keys-design.md`

## Global Constraints

- **Naming.** "Identity" means *this laptop's own* key throughout. The org's are **issued keys**: `issuedKeys` (Convex), `issued.rs` (Rust), "Issued keys" (dashboard).
- **All work goes through a pull request**, and is not finished until PR CI has completed. Do not push to `main`.
- **Every external process goes through `CommandRunner`.** No `std::process::Command` outside `riabuild-runner`.
- **All Rust IO is async** — `tokio::fs`, never `std::fs`.
- **Components never call `useQuery`.** Only `src/data/convexProvider.tsx` may import from `convex/react`; everything else reads `useData()`.
- **Convex functions always declare `args` and `returns` validators.** Anything not called from a client is `internalQuery`/`internalMutation`.
- **Rust production files target ~300 lines.** Split rather than exceed.
- **A private key is never returned to a browser**, by any route, ever.
- Key lifetime in the agent is **900 seconds** (`ssh-add -t 900`).
- The row scan bound is **`.take(200)`**, matching `sharedServers`.
- Audit action names, exactly: `issued_key.created`, `issued_key.replaced`, `issued_key.issued`, `issued_key.removed`, `issued_key.served`.

---

### Task 1: The OpenSSH key parser (TypeScript)

**Files:**
- Create: `riabuild-web/convex/lib/opensshKey.ts`
- Create: `riabuild-web/convex/lib/opensshKey.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```ts
  export type ParsedKey = { keyType: string; publicKey: string; fingerprint: string };
  export class KeyParseError extends Error {}
  export async function parseOpenSshPrivateKey(pem: string): Promise<ParsedKey>;
  ```
  `publicKey` is a full `authorized_keys` line: `"<keyType> <base64 blob>"`, no comment.
  `fingerprint` is `"SHA256:"` + unpadded standard base64 of SHA-256 over the raw public blob.
  Async because `crypto.subtle.digest` is async — see `convex/lib/crypto.ts`, which documents that Convex's runtime is V8 and `node:crypto` is unavailable.

- [ ] **Step 1: Generate the test fixtures**

Real keys, so the test proves the parser against what `ssh-keygen` actually emits.

```bash
cd /tmp && rm -f fx_ed fx_rsa fx_enc
ssh-keygen -t ed25519 -N "" -C "fixture" -f fx_ed
ssh-keygen -t rsa -b 2048 -N "" -C "fixture" -f fx_rsa
ssh-keygen -t ed25519 -N "hunter2" -C "fixture" -f fx_enc
for f in fx_ed fx_rsa fx_enc; do echo "=== $f"; cat $f; echo "--- pub"; cat $f.pub; echo "--- fp"; ssh-keygen -lf $f.pub; done
```

Paste the private keys, the `.pub` lines and the fingerprints into the test file as constants: `ED25519_PRIVATE`, `ED25519_PUBLIC`, `ED25519_FINGERPRINT`, `RSA_PRIVATE`, `RSA_PUBLIC`, `RSA_FINGERPRINT`, `ENCRYPTED_PRIVATE`.

- [ ] **Step 2: Write the failing tests**

```ts
import { describe, expect, it } from "vitest";
import { KeyParseError, parseOpenSshPrivateKey } from "./opensshKey";

describe("parseOpenSshPrivateKey", () => {
  it("derives the public half of an ed25519 key without any key mathematics", async () => {
    const parsed = await parseOpenSshPrivateKey(ED25519_PRIVATE);
    expect(parsed.keyType).toBe("ssh-ed25519");
    // The comment is dropped: `ssh-keygen` writes one, and it is free text
    // that would otherwise become part of the value a lead compares.
    expect(parsed.publicKey).toBe(ED25519_PUBLIC.split(" ").slice(0, 2).join(" "));
    expect(parsed.fingerprint).toBe(ED25519_FINGERPRINT);
  });

  it("derives an rsa key the same way", async () => {
    const parsed = await parseOpenSshPrivateKey(RSA_PRIVATE);
    expect(parsed.keyType).toBe("ssh-rsa");
    expect(parsed.publicKey).toBe(RSA_PUBLIC.split(" ").slice(0, 2).join(" "));
    expect(parsed.fingerprint).toBe(RSA_FINGERPRINT);
  });

  it("refuses a passphrase-protected key, because nobody could answer the prompt", async () => {
    // `ssh-add` would ask for the passphrase on a developer's laptop mid-run,
    // with no one to type it — a hang with no output.
    await expect(parseOpenSshPrivateKey(ENCRYPTED_PRIVATE)).rejects.toThrow(KeyParseError);
    await expect(parseOpenSshPrivateKey(ENCRYPTED_PRIVATE)).rejects.toThrow(/passphrase/i);
  });

  it("refuses anything that is not an OpenSSH private key", async () => {
    for (const junk of [
      "",
      "hello",
      "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----",
      "-----BEGIN OPENSSH PRIVATE KEY-----\nbm90LWEta2V5\n-----END OPENSSH PRIVATE KEY-----",
    ]) {
      await expect(parseOpenSshPrivateKey(junk)).rejects.toThrow(KeyParseError);
    }
  });

  it("tolerates the whitespace a paste box introduces", async () => {
    const parsed = await parseOpenSshPrivateKey(`\n  ${ED25519_PRIVATE.trim()}  \n\n`);
    expect(parsed.keyType).toBe("ssh-ed25519");
  });
});
```

- [ ] **Step 3: Run to verify it fails**

Run: `cd riabuild-web && pnpm vitest run convex/lib/opensshKey.test.ts`
Expected: FAIL — cannot resolve `./opensshKey`.

- [ ] **Step 4: Implement the parser**

```ts
/**
 * Deriving an SSH public key from a private one, with no key mathematics.
 *
 * An OpenSSH private key file *contains* its own public key, in the clear, as a
 * length-prefixed field before the encrypted section. So this is a container
 * walk and one digest — which is why the same logic runs in Convex's V8
 * runtime, in the browser, and (ported) in Rust, without a crypto library at
 * any of the three.
 *
 * Layout, after base64-decoding the body between the PEM markers:
 *
 *   "openssh-key-v1\0"
 *   string  ciphername      "none" when there is no passphrase
 *   string  kdfname
 *   string  kdfoptions
 *   uint32  number of keys
 *   string  publickey       <-- what this file is for
 *   string  encrypted section
 */

const MAGIC = "openssh-key-v1";
const BEGIN = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END = "-----END OPENSSH PRIVATE KEY-----";

export class KeyParseError extends Error {}

export type ParsedKey = {
  keyType: string;
  publicKey: string;
  fingerprint: string;
};

/** A cursor over the length-prefixed fields OpenSSH serialises with. */
class Reader {
  private offset = 0;
  constructor(private readonly bytes: Uint8Array) {}

  uint32(): number {
    if (this.offset + 4 > this.bytes.length) {
      throw new KeyParseError("That key is truncated.");
    }
    const view = new DataView(
      this.bytes.buffer,
      this.bytes.byteOffset + this.offset,
      4,
    );
    this.offset += 4;
    return view.getUint32(0, false);
  }

  /** A `string` in this format is a uint32 length followed by that many bytes. */
  string(): Uint8Array {
    const length = this.uint32();
    if (this.offset + length > this.bytes.length) {
      throw new KeyParseError("That key is truncated.");
    }
    const slice = this.bytes.subarray(this.offset, this.offset + length);
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
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function encodeBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

const decoder = new TextDecoder();

export async function parseOpenSshPrivateKey(pem: string): Promise<ParsedKey> {
  const text = pem.trim();
  if (!text.startsWith(BEGIN) || !text.endsWith(END)) {
    throw new KeyParseError(
      "That is not an OpenSSH private key. It should begin with " +
        `"${BEGIN}" — if yours starts with "-----BEGIN RSA PRIVATE KEY-----" ` +
        "or similar, convert it with `ssh-keygen -p -m RFC4716 -f <file>`.",
    );
  }

  const body = text.slice(BEGIN.length, text.length - END.length).replace(/\s+/g, "");
  const bytes = decodeBase64(body);

  const magic = decoder.decode(bytes.subarray(0, MAGIC.length));
  if (magic !== MAGIC || bytes[MAGIC.length] !== 0) {
    throw new KeyParseError("That key is not in the openssh-key-v1 format.");
  }

  const reader = new Reader(bytes.subarray(MAGIC.length + 1));
  const cipherName = decoder.decode(reader.string());
  reader.string(); // kdfname
  reader.string(); // kdfoptions

  if (cipherName !== "none") {
    // Refused at the paste box rather than on a laptop: `ssh-add` would prompt
    // for the passphrase mid-run, with nobody who could answer it.
    throw new KeyParseError(
      "That key is protected by a passphrase. riabuild cannot use it, because " +
        "nothing would be able to answer the prompt on a developer's machine. " +
        "Remove the passphrase with `ssh-keygen -p -f <file>` and paste it again.",
    );
  }

  const count = reader.uint32();
  if (count !== 1) {
    throw new KeyParseError(`That file holds ${count} keys; it must hold exactly one.`);
  }

  const publicBlob = reader.string();
  // The blob is itself length-prefixed, and its first string names the type.
  const keyType = decoder.decode(new Reader(publicBlob).string());
  if (!/^[a-z0-9@.-]{4,64}$/i.test(keyType)) {
    throw new KeyParseError("That key does not name a key type riabuild recognises.");
  }

  const digest = await crypto.subtle.digest("SHA-256", publicBlob);
  const fingerprint = `SHA256:${encodeBase64(new Uint8Array(digest)).replace(/=+$/, "")}`;

  return { keyType, publicKey: `${keyType} ${encodeBase64(publicBlob)}`, fingerprint };
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cd riabuild-web && pnpm vitest run convex/lib/opensshKey.test.ts`
Expected: PASS, all five.

- [ ] **Step 6: Commit**

```bash
git add riabuild-web/convex/lib/opensshKey.ts riabuild-web/convex/lib/opensshKey.test.ts
git commit -m "Derive an SSH public key by reading the private key's container"
```

---

### Task 2: The `issuedKeys` table, queries and mutations

**Files:**
- Modify: `riabuild-web/convex/schema.ts`
- Create: `riabuild-web/convex/issuedKeys.ts`
- Create: `riabuild-web/convex/issuedKeys.test.ts`

**Interfaces:**
- Consumes: `parseOpenSshPrivateKey`, `KeyParseError` (Task 1); `requireLead`, `writeAudit` from `./members`.
- Produces:
  ```ts
  export const issuedKeyView: Validator      // NO privateKey field
  export const list: Query                   // lead-only
  export const create: Mutation              // { label, privateKey } -> Id<"issuedKeys">
  export const replaceKey: Mutation          // { id, privateKey } -> null
  export const setIssuedTo: Mutation         // { id, issuedTo: Id<"members">[] } -> null
  export const remove: Mutation              // { id } -> null
  export const serveForApi: InternalMutation // { memberId } -> ApiKey[]; audits
  ```
  `ApiKey = { id, label, keyType, publicKey, fingerprint, privateKey }`.

  `serveForApi` is an **internalMutation, not an internalQuery**, because it writes the
  `issued_key.served` audit row. A query cannot write, and splitting the read from the
  audit would let a future caller take the keys without the log.

- [ ] **Step 1: Add the table to `schema.ts`**

Insert after the `sharedServers` block:

```ts
  /**
   * SSH keys the org issues: a private key a lead pastes once, and the members
   * it is issued to.
   *
   * This is the one table here that holds a long-lived secret in plaintext, and
   * `../../CLAUDE.md` names it as a deliberate third exception to "secrets are
   * brokered, never stored" rather than leaving the invariant and this row to
   * contradict each other. A dump of this database hands out working SSH access
   * to whatever these keys open. What bounds that is everywhere else: nothing
   * returns `privateKey` to a browser, every fetch is audited by label, the CLI
   * holds it only in an ssh-agent and never on a filesystem, and it bootstraps
   * one `ssh-copy-id` rather than replacing a developer's own key.
   *
   * `publicKey`, `fingerprint` and `keyType` are derived from `privateKey` by
   * `lib/opensshKey.ts` and never accepted from a client — an OpenSSH container
   * carries its own public half, so this costs one digest and no key
   * mathematics. They exist so a lead can identify a row without the row ever
   * handing the secret back.
   *
   * `issuedTo` is an array rather than a join table: Convex cannot index
   * array-contains, so "keys issued to me" is a bounded scan, the same shape
   * and the same bound `sharedServers` uses.
   *
   * Design: `docs/superpowers/specs/2026-08-13-issued-ssh-keys-design.md`.
   */
  issuedKeys: defineTable({
    label: v.string(),
    privateKey: v.string(),
    publicKey: v.string(),
    fingerprint: v.string(),
    keyType: v.string(),
    issuedTo: v.array(v.id("members")),
    createdBy: v.id("members"),
    createdAt: v.number(),
    updatedAt: v.number(),
  }).index("by_label", ["label"]),
```

- [ ] **Step 2: Write the failing tests**

Follow the setup helpers already in `convex/api.test.ts` (`convexTest`, seeding a lead and a developer). Tests:

```ts
it("never returns a private key to the dashboard", async () => {
  // The single most important test here. `list` returns a projection with no
  // such field, rather than a document with the field stripped at a call site
  // a later caller could forget.
  const rows = await asLead(t).query(api.issuedKeys.list, {});
  expect(rows).toHaveLength(1);
  expect(rows[0]).not.toHaveProperty("privateKey");
  expect(JSON.stringify(rows)).not.toContain("BEGIN OPENSSH");
});

it("derives the public half and ignores whatever the client claims", async () => {
  const id = await asLead(t).mutation(api.issuedKeys.create, {
    label: "prod-bastion",
    privateKey: ED25519_PRIVATE,
  });
  const row = await t.run(async (ctx) => ctx.db.get("issuedKeys", id));
  expect(row!.publicKey).toBe(ED25519_PUBLIC_NO_COMMENT);
  expect(row!.fingerprint).toBe(ED25519_FINGERPRINT);
  expect(row!.keyType).toBe("ssh-ed25519");
});

it("refuses a passphrase-protected key at the door", async () => {
  await expect(
    asLead(t).mutation(api.issuedKeys.create, {
      label: "nope",
      privateKey: ENCRYPTED_PRIVATE,
    }),
  ).rejects.toThrow(/passphrase/i);
});

it("refuses a duplicate label", async () => { /* second create with same label rejects */ });

it("lets only a lead write", async () => {
  await expect(
    asDeveloper(t).mutation(api.issuedKeys.create, { label: "x", privateKey: ED25519_PRIVATE }),
  ).rejects.toThrow(/team leads/i);
  await expect(asDeveloper(t).query(api.issuedKeys.list, {})).rejects.toThrow(/team leads/i);
});

it("serves a member only the keys issued to them, and audits the fetch by label", async () => {
  const served = await t.run(async (ctx) =>
    ctx.runMutation(internal.issuedKeys.serveForApi, { memberId: ada }),
  );
  expect(served.map((k) => k.label)).toEqual(["prod-bastion"]);
  expect(served[0].privateKey).toContain("BEGIN OPENSSH");
  const audit = await t.run(async (ctx) => ctx.db.query("auditLog").collect());
  const served_ = audit.find((a) => a.action === "issued_key.served");
  expect(served_!.meta.keys).toBe("prod-bastion");
  expect(served_!.meta.count).toBe("1");
});

it("serves nothing to a member named on no row", async () => { /* grace gets [] and no audit noise beyond count 0 */ });

it("writes an audit row for every change", async () => {
  // created / replaced / issued / removed, with the label in meta and, for
  // `issued`, the logins added and removed.
});

it("keeps the label and the grants when a key is replaced", async () => {
  // replaceKey is how rotation happens: same row, same issuedTo, new secret.
});
```

- [ ] **Step 3: Run to verify they fail**

Run: `cd riabuild-web && pnpm vitest run convex/issuedKeys.test.ts`
Expected: FAIL — `api.issuedKeys` does not exist.

- [ ] **Step 4: Implement `convex/issuedKeys.ts`**

Mirror `sharedServers.ts` for shape and voice. Key points:

```ts
const LABEL = /^[A-Za-z0-9._-]{1,32}$/;

export const issuedKeyView = v.object({
  _id: v.id("issuedKeys"),
  label: v.string(),
  keyType: v.string(),
  publicKey: v.string(),
  fingerprint: v.string(),
  issuedTo: v.array(v.id("members")),
  updatedAt: v.number(),
});
// Note what is absent, and note that it is absent by construction rather than
// by omission: there is no `privateKey` in this validator, so a handler that
// tried to return one would fail its own `returns` check.

async function derive(privateKey: string) {
  try {
    return await parseOpenSshPrivateKey(privateKey);
  } catch (error) {
    // KeyParseError's messages are written for a lead reading them in a
    // browser, so they are passed through rather than reworded.
    throw new Error(error instanceof KeyParseError ? error.message : String(error));
  }
}
```

`create`: `requireLead`, validate label, reject duplicate label via `by_label`, `derive`, insert, audit `issued_key.created` with `{ label, fingerprint }`.

`replaceKey`: `requireLead`, load, `derive`, patch the four derived+secret fields and `updatedAt`, leaving `label` and `issuedTo` alone, audit `issued_key.replaced` with `{ label, from: old.fingerprint, fingerprint }`.

`setIssuedTo`: `requireLead`, load, verify every id resolves to a member (a dangling id would serve nobody and be invisible), patch, audit `issued_key.issued` with `{ label, added, removed }` as comma-joined `githubLogin`s.

`remove`: `requireLead`, load, delete, audit `issued_key.removed` with `{ label, fingerprint }`.

`serveForApi`:

```ts
export const serveForApi = internalMutation({
  args: { memberId: v.id("members") },
  returns: v.array(
    v.object({
      id: v.string(),
      label: v.string(),
      keyType: v.string(),
      publicKey: v.string(),
      fingerprint: v.string(),
      privateKey: v.string(),
    }),
  ),
  handler: async (ctx, args) => {
    const rows = await ctx.db.query("issuedKeys").take(200);
    const mine = rows
      .filter((row) => row.issuedTo.some((id) => id === args.memberId))
      .sort((a, b) => a.label.localeCompare(b.label));

    // The only endpoint in riabuild that hands out a durable credential, so it
    // is the one whose *reads* are logged. A log of grants answers who was
    // entitled to a key; this answers who took a copy of one, which is the
    // question actually asked after somebody leaves.
    await writeAudit(ctx, {
      actorId: args.memberId,
      action: "issued_key.served",
      meta: {
        keys: mine.map((row) => row.label).join(","),
        count: String(mine.length),
      },
    });

    return mine.map((row) => ({
      id: row._id,
      label: row.label,
      keyType: row.keyType,
      publicKey: row.publicKey,
      fingerprint: row.fingerprint,
      privateKey: row.privateKey,
    }));
  },
});
```

- [ ] **Step 5: Run to verify they pass**

Run: `cd riabuild-web && pnpm vitest run convex/issuedKeys.test.ts && pnpm lint`
Expected: PASS, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add riabuild-web/convex/schema.ts riabuild-web/convex/issuedKeys.ts riabuild-web/convex/issuedKeys.test.ts
git commit -m "Store issued SSH keys, and derive every field a lead reads"
```

---

### Task 3: `GET /api/v1/issued-keys`

**Files:**
- Modify: `riabuild-web/convex/http.ts` (add after the `/api/v1/remotes/shared` block, ~line 540)
- Modify: `riabuild-web/convex/api.test.ts`

**Interfaces:**
- Consumes: `internal.issuedKeys.serveForApi` (Task 2); `endpoint`, `authenticate`, `loadConfig`, `enforceMinVersion`, `requireOrgMembership`, `jsonResponse` (all already in `http.ts`).
- Produces: `GET /api/v1/issued-keys` → `{ keys: ApiKey[] }`, consumed by Task 5.

- [ ] **Step 1: Write the failing tests in `api.test.ts`**

```ts
it("gives a candidate an empty key list and a 200, never a 403", async () => {
  // Same rule as /remotes/shared: `riabuild remote` is also how a candidate
  // reaches the server they set up themselves.
  const res = await fetchApi(t, "/api/v1/issued-keys", candidateToken);
  expect(res.status).toBe(200);
  expect(res.body).toEqual({ keys: [] });
});

it("gives a developer only the keys issued to them", async () => {
  const res = await fetchApi(t, "/api/v1/issued-keys", adaToken);
  expect(res.body.keys.map((k) => k.label)).toEqual(["prod-bastion"]);
  expect(res.body.keys[0].privateKey).toContain("BEGIN OPENSSH");
});

it("gives a departed member nothing at all", async () => {
  // Org membership is re-verified on every secret-brokering request; the
  // Convex row is never the sole gate.
  mockOrgMembership(false);
  const res = await fetchApi(t, "/api/v1/issued-keys", adaToken);
  expect(res.status).toBe(403);
});

it("refuses an unauthenticated request", async () => {
  expect((await fetchApi(t, "/api/v1/issued-keys", undefined)).status).toBe(401);
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd riabuild-web && pnpm vitest run convex/api.test.ts -t "issued-keys"`
Expected: FAIL — 404 from an unrouted path.

- [ ] **Step 3: Implement the route**

```ts
/* -------------------------------------------------------------------------- */
/* GET /api/v1/issued-keys — the SSH keys issued to this developer             */
/* -------------------------------------------------------------------------- */

http.route({
  path: "/api/v1/issued-keys",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      const { member } = await authenticate(ctx, req);
      // The rule this endpoint exists under: this is the only response in
      // riabuild carrying a durable credential, so the org check is the gate
      // that matters and `members.role` is never the sole one.
      await requireOrgMembership(member.githubLogin);

      // A candidate gets an empty list and a 200, for the reason
      // /api/v1/remotes/shared does — and the fetch is not audited, because
      // nothing was served.
      if (member.role === "candidate") {
        return jsonResponse({ keys: [] });
      }
      const keys = await ctx.runMutation(internal.issuedKeys.serveForApi, {
        memberId: member.id as Id<"members">,
      });
      return jsonResponse({ keys });
    }),
  ),
});
```

Check what `MemberView` actually calls the row id before writing `member.id` — read the `memberView` validator in `convex/members.ts` and use its real field name.

- [ ] **Step 4: Run to verify they pass**

Run: `cd riabuild-web && pnpm vitest run convex/api.test.ts && pnpm lint`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/convex/http.ts riabuild-web/convex/api.test.ts
git commit -m "Serve a developer the SSH keys issued to them, and log the fetch"
```

---

### Task 4: The Issued keys dashboard panel

**Files:**
- Create: `riabuild-web/src/lib/opensshKey.ts` (re-export for the browser)
- Create: `riabuild-web/src/components/IssuedKeys.tsx`
- Modify: `riabuild-web/src/data/types.ts`, `riabuild-web/src/data/convexProvider.tsx`, `riabuild-web/src/data/offlineData.ts`, `riabuild-web/src/dev/scenarios.ts`, `riabuild-web/src/components/LeadPanel.tsx`

**Interfaces:**
- Consumes: `api.issuedKeys.{list,create,replaceKey,setIssuedTo,remove}` (Task 2); `parseOpenSshPrivateKey` (Task 1).
- Produces: `Data` gains
  ```ts
  issuedKeys: Loadable<IssuedKey[]>;
  addIssuedKey(p: { label: string; privateKey: string }): Promise<void>;
  replaceIssuedKey(p: { id: string; privateKey: string }): Promise<void>;
  setIssuedKeyMembers(p: { id: string; issuedTo: string[] }): Promise<void>;
  removeIssuedKey(p: { id: string }): Promise<void>;
  ```
  with `type IssuedKey = { id: string; label: string; keyType: string; publicKey: string; fingerprint: string; issuedTo: string[]; updatedAt: number }`.

**REQUIRED READING before this task:** `.claude/skills/riabuild-ui/SKILL.md`. The UI is a fake TUI built entirely from `src/ui/`; nothing here may look as though the operating system drew it.

- [ ] **Step 1: Add the fixtures and the scenarios**

`offlineData.ts` gains an `issuedKeys` list; `scenarios.ts` gains `issued-keys-empty`, `issued-keys-populated`, `issued-keys-unparseable`. Follow the shared-servers entries directly above them.

- [ ] **Step 2: Write the component**

Model it on `src/components/SharedServers.tsx`. Requirements that are not stylistic:

- The add form is a **label field and a paste box**. Below the paste box, three
  **read-only** fields — type, public key, fingerprint — filled in from
  `parseOpenSshPrivateKey` as the lead pastes, debounced. A key that will not parse
  shows the `KeyParseError` message there, before saving.
- The list shows label, type, fingerprint and the members it is issued to.
- **There is no reveal control and no edit-in-place for the key.** Changing a key is
  `replaceIssuedKey`, which takes a fresh paste. This is why the fingerprint is
  displayed at all.
- The section renders only for a lead, like `SharedServers`.

- [ ] **Step 3: Verify the data boundary holds**

Run: `cd riabuild-web && grep -rn "convex/react" src/ --include=*.tsx | grep -v data/convexProvider`
Expected: empty output.

- [ ] **Step 4: Run the visual suite and look at every screenshot**

**REQUIRED READING:** `.claude/skills/visual-testing/SKILL.md`.

Run: `cd riabuild-web && pnpm lint && pnpm test && pnpm ui:check`
Then open every screenshot the run produced and look at it. A passing exit code is not
the check; the screenshots are. If `pnpm dev` is running, stop it first — the visual
suite flakes under CPU contention.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src
git commit -m "Give leads a panel for issued keys that never shows one back"
```

---

### Task 5: The Rust parser and API client

**Files:**
- Modify: `riabuild-cli/Cargo.toml`, `riabuild-cli/crates/api/Cargo.toml`
- Create: `riabuild-cli/crates/api/src/openssh.rs`
- Create: `riabuild-cli/crates/api/src/issued.rs`
- Modify: `riabuild-cli/crates/api/src/lib.rs` (add `pub mod openssh; pub mod issued;`)

**Interfaces:**
- Consumes: `GET /api/v1/issued-keys` (Task 3); `ApiClient::get_json`.
- Produces:
  ```rust
  // openssh.rs
  pub struct PublicHalf { pub key_type: String, pub public_key: String, pub fingerprint: String }
  pub fn public_half(private_key: &str) -> Result<PublicHalf, String>;

  // issued.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct IssuedKey {
      pub id: String,
      pub label: String,
      pub key_type: String,
      pub public_key: String,
      pub fingerprint: String,
      pub private_key: String,
  }
  #[derive(Debug, Clone, Default)]
  pub struct Fetched { pub keys: Vec<IssuedKey>, pub refused: Vec<String> }
  pub async fn fetch_issued(api: &ApiClient) -> Result<Fetched>;
  ```

- [ ] **Step 1: Add the dependency**

In `riabuild-cli/Cargo.toml`'s workspace dependencies, in the house style — every entry
there carries the reason it is present:

```toml
# base64 for the OpenSSH key container, which stores a key's public half as a
# base64 blob inside a base64 body. Nothing else in this binary does base64, and
# the forty lines it would take to avoid this would be forty lines of a codec
# that is wrong in exactly one direction and silently. SHA-256 is already free
# through `ring`, above.
base64 = "0.22"
```

- [ ] **Step 2: Write the failing parser tests**

Use the *same* fixtures as Task 1 — the two parsers must agree byte for byte, and
sharing the fixtures is what proves it.

```rust
#[test]
fn the_public_half_comes_out_of_the_private_key_itself() {
    let half = public_half(ED25519_PRIVATE).expect("parses");
    assert_eq!(half.key_type, "ssh-ed25519");
    assert_eq!(half.public_key, ED25519_PUBLIC_NO_COMMENT);
    assert_eq!(half.fingerprint, ED25519_FINGERPRINT);
}

#[test]
fn an_rsa_key_parses_the_same_way() { /* RSA fixtures */ }

#[test]
fn a_passphrase_protected_key_is_refused() {
    let refused = public_half(ENCRYPTED_PRIVATE).expect_err("must not parse");
    assert!(refused.contains("passphrase"), "{refused}");
}

#[test]
fn junk_is_refused_rather_than_panicking() {
    // Every one of these reaches a length prefix that would index past the
    // end of the buffer. A parser that panicked here would take down a
    // developer's run on a row a lead typed wrong.
    for junk in ["", "hello", "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----"] {
        assert!(public_half(junk).is_err(), "{junk:?}");
    }
}
```

And the client tests, mirroring `remotes.rs`'s table:

```rust
#[test]
fn a_key_whose_halves_disagree_is_refused() {
    // The check `remotes.rs` has no analogue for. If the stored public key and
    // the stored private key are not the same key pair, something has edited
    // the row's fields apart from each other — and riabuild must not probe a
    // server with a credential whose fingerprint it cannot vouch for.
    let wire = WireKey { public_key: OTHER_PUBLIC.into(), ..wire() };
    let refused = usable(&wire).expect_err("must not be usable");
    assert!(refused.contains("does not match"), "{refused}");
}

#[test]
fn one_refused_key_does_not_cost_the_developer_the_rest() { /* as remotes.rs */ }

#[test]
fn a_reply_with_no_keys_field_at_all_is_an_empty_list() {
    // What a riabuild-web that has not been deployed yet answers.
    let reply: Reply = serde_json::from_str("{}").expect("decodes");
    assert!(sort_out(reply.keys).keys.is_empty());
}

#[test]
fn an_unknown_field_is_ignored() { /* forward compatibility, as /api/v1 requires */ }
```

- [ ] **Step 3: Run to verify they fail**

Run: `cd riabuild-cli && cargo test -p riabuild-api`
Expected: FAIL — unresolved module.

- [ ] **Step 4: Implement both modules**

`openssh.rs` is the direct port of Task 1's parser: same layout walk, same rejections,
same message wording where a developer will read it. Use `ring::digest::digest(&SHA256, blob)`
and `base64::engine::general_purpose::STANDARD`. Return `Result<_, String>` rather than
`anyhow` — every caller turns it into one line of a refusal list.

`issued.rs` follows `remotes.rs` exactly: an `i64`-tolerant wire struct with
`#[serde(default)]` on every field so one bad row cannot fail the whole reply, a
`usable()` returning `Result<IssuedKey, String>`, and `sort_out()` collecting
`keys`/`refused`. `usable()` checks the label charset, that `public_key` parses as a key
line, and that `public_half(&wire.private_key)?.public_key == wire.public_key`.

- [ ] **Step 5: Run to verify they pass**

Run: `cd riabuild-cli && cargo test -p riabuild-api && cargo clippy -p riabuild-api --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/Cargo.toml riabuild-cli/crates/api
git commit -m "Fetch issued keys, and check each one against its own private half"
```

---

### Task 6: The private ssh-agent

**Files:**
- Modify: `riabuild-cli/crates/paths/src/lib.rs` (add `agent_dir()`)
- Create: `riabuild-cli/crates/remote/src/issued.rs`
- Create: `riabuild-cli/crates/remote/src/issued/agent.rs`
- Modify: `riabuild-cli/crates/remote/src/lib.rs` (add `mod issued;`)

**Interfaces:**
- Consumes: `riabuild_api::issued::{IssuedKey, fetch_issued}` (Task 5); `CommandRunner::{run, spawn, which}`; `ChildHandle::kill`.
- Produces:
  ```rust
  // issued/agent.rs
  pub struct Agent { /* child, socket, dir */ }
  impl Agent {
      pub async fn start(remote: &Remote, paths: &dyn Paths, runner: Arc<dyn CommandRunner>)
          -> Result<Option<Agent>>;                       // Ok(None) = no ssh-agent on PATH
      pub async fn add(&self, runner: Arc<dyn CommandRunner>, key: &IssuedKey)
          -> Result<PathBuf>;                             // returns the .pub path
      pub async fn probe(&self, remote: &Remote, paths: &dyn Paths,
                         runner: Arc<dyn CommandRunner>, public_key_path: &Path)
          -> Result<bool>;
      pub async fn stop(self, runner: Arc<dyn CommandRunner>);
      pub fn socket(&self) -> &Path;
  }

  // issued.rs
  pub struct Working { pub label: String, pub socket: PathBuf, pub public_key_path: PathBuf }
  pub struct Issued<'a> { /* api, lazily-fetched keys, agent */ }
  impl<'a> Issued<'a> {
      pub fn new(api: &'a ApiClient) -> Self;
      pub async fn working(&mut self, remote: &Remote, paths: &dyn Paths,
                           runner: Arc<dyn CommandRunner>, ui: &Ui) -> Option<&Working>;
      pub async fn stop(&mut self, runner: Arc<dyn CommandRunner>);
  }
  ```
  `Issued::working` fetches on first call and returns `None` for every ordinary
  failure — no keys issued, no `ssh-agent`, a fetch that failed, none that signed in —
  after warning. It never returns `Err`: this whole feature is an *additional* way in,
  and losing it must cost a warning rather than the run.

- [ ] **Step 1: Add `agent_dir` to `Paths`**

```rust
/// Where an issued-key `ssh-agent` keeps its socket and the public halves that
/// address its identities, one directory per server.
///
/// Under `~/.riabuild` and `0700`, but note what does *not* live here and
/// cannot: the private keys themselves never touch a filesystem. A socket and
/// a public key are both inert — see the spec's §7.
fn agent_dir(&self, server_hash: &str) -> PathBuf {
    self.root().join("agent").join(server_hash)
}
```

- [ ] **Step 2: Write the failing agent tests**

```rust
#[tokio::test]
async fn the_private_key_reaches_ssh_add_on_stdin_and_appears_in_no_argument() {
    // `ps` shows an argv to every process on the machine, and on a shared box
    // that includes every other developer. This is the test that keeps it out.
    let fake = Arc::new(FakeRunner::new()
        .spawning_until_killed("ssh-agent -D")
        .with("ssh-add", 0, "", ""));
    let agent = Agent::start(&remote(), &paths, fake.clone()).await.expect("start").expect("some");
    agent.add(fake.clone(), &key()).await.expect("add");

    let piped = fake.stdin_text_of("ssh-add").expect("ssh-add got stdin");
    assert!(piped.contains("BEGIN OPENSSH PRIVATE KEY"), "{piped}");
    for call in fake.calls() {
        assert!(!call.contains("BEGIN OPENSSH"), "key material in an argv: {call}");
        assert!(!call.contains(SECRET_BODY_FRAGMENT), "key material in an argv: {call}");
    }
}

#[tokio::test]
async fn keys_are_added_with_a_lifetime_so_an_orphaned_agent_forgets_them() {
    // A SIGKILLed riabuild orphans its children. Without -t, an orphaned agent
    // would serve the org's keys until the machine rebooted.
    assert!(fake.calls().iter().any(|c| c.contains("ssh-add -t 900")), "{:?}", fake.calls());
}

#[tokio::test]
async fn the_agent_runs_in_the_foreground_so_riabuild_can_kill_it() {
    assert!(fake.spawns().iter().any(|c| c.contains("ssh-agent -D")));
    agent.stop(fake.clone()).await;
    assert_eq!(fake.killed().len(), 1, "the agent must not outlive the run");
}

#[tokio::test]
async fn a_probe_offers_exactly_one_identity() {
    // One connection offering every key would hit sshd's MaxAuthTries (6 by
    // default) and silently stop before a developer's seventh key was tried —
    // and would not say which key got in.
    agent.probe(&remote(), &paths, fake.clone(), &pub_path).await.expect("probe");
    let call = fake.calls().iter().find(|c| c.starts_with("ssh ")).expect("probed").clone();
    assert!(call.contains("IdentityAgent="), "{call}");
    assert!(call.contains("IdentitiesOnly=yes"), "{call}");
    assert!(call.contains("BatchMode=yes"), "{call}");
    assert!(call.contains(&pub_path.to_string_lossy().to_string()), "{call}");
}

#[tokio::test]
async fn no_ssh_agent_on_path_is_a_warning_and_not_a_stop() {
    // riabuild stops when there is no way in, not when the convenient way in
    // failed — the rule `authorise`'s module doc sets out.
    let fake = Arc::new(FakeRunner::new());   // `which` finds nothing
    assert!(Agent::start(&remote(), &paths, fake).await.expect("no error").is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn the_agent_directory_is_private() { /* 0700, as identity::set_private_dir */ }
```

- [ ] **Step 3: Run to verify they fail**

Run: `cd riabuild-cli && cargo test -p riabuild-remote issued`
Expected: FAIL — unresolved module.

- [ ] **Step 4: Implement `agent.rs`, then `issued.rs`**

`Agent::start`: `runner.which("ssh-agent")` → `Ok(None)` when absent. Create
`paths.agent_dir(&remote.hash())`, `set_private_dir` it, remove a stale `sock` left by a
killed run, `runner.spawn("ssh-agent", &["-D", "-a", sock])`. Poll for the socket to
appear (bounded, ~2s) — the child is up before the socket is bound, and a probe against a
socket that does not exist yet fails for the wrong reason.

`Agent::add`: write `<key.id>.pub` (`key.public_key`, `0600` is unnecessary but harmless —
use the directory's `0700` as the boundary), then
`runner.run("ssh-add", &["-t", "900", "-"], RunOptions { stdin: Some(key.private_key.into_bytes()), env: vec![("SSH_AUTH_SOCK", sock)], ..default })`.

`Agent::probe`: `ssh` with `BatchMode=yes`, `IdentityAgent=<sock>`, `IdentitiesOnly=yes`,
`-i <pub path>`, plus the pinned `known_hosts` options from `identity::ssh_options` —
reuse that function rather than restating its flags, and add a note there that it is now
shared. Deliberately **no askpass** in `RunOptions`, for the reason `can_sign_in`
documents: a saved password could otherwise make the answer yes for a key that does not
work.

`Agent::stop`: kill the child, then remove the directory. Both are best-effort — a failure
here must not surface as a failure of the run.

`Issued::working`: on first call, `fetch_issued`; warn each `refused` line; if `keys` is
empty return `None` without starting an agent. Otherwise `Agent::start`, `add` each key,
`probe` each in order, and on the first success store `Working { label, socket, public_key_path }`
and `ui.applied(...)` naming the label. Cache the answer so a second call does not re-probe.

- [ ] **Step 5: Run to verify they pass**

Run: `cd riabuild-cli && cargo test -p riabuild-remote && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/crates/paths riabuild-cli/crates/remote
git commit -m "Hold issued keys in an ssh-agent riabuild owns, never on a disk"
```

---

### Task 7: Wire issued keys into `authorise`

**Files:**
- Modify: `riabuild-cli/crates/remote/src/authorise.rs` (the `authorise` fn, ~line 196, and the module doc)
- Modify: `riabuild-cli/crates/remote/src/authorise/copy.rs` (`install_key` gains the credential)
- Modify: `riabuild-cli/crates/remote/src/flow/connect.rs` (~line 150-168)

**Interfaces:**
- Consumes: `issued::{Issued, Working}` (Task 6).
- Produces:
  ```rust
  pub async fn authorise(remote: &Remote, paths: &dyn Paths, runner: Arc<dyn CommandRunner>,
                         ui: &Ui, issued: &mut Issued<'_>) -> Result<()>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_keys_only_server_with_a_working_issued_key_now_installs_the_key() {
    // The case this whole feature exists for, and a hard failure before it:
    // `PasswordAuthentication no`, so there is no password to ask for, and
    // today riabuild stops and tells the developer to paste a public key into
    // a file they may not be able to edit.
    // can_sign_in fails, the issued probe succeeds, ssh-copy-id runs.
    assert!(fake.calls().iter().any(|c| c.contains("IdentityAgent=")));
    assert!(applied.contains("Authorised"));
}

#[tokio::test]
async fn a_keys_only_server_with_no_issued_key_still_fails_with_the_paste_remedy() {
    // The old behaviour has to survive exactly, for the servers this feature
    // does not reach.
    let error = authorise(...).await.expect_err("must fail");
    assert!(format!("{error:?}").contains("accepts keys only"));
}

#[tokio::test]
async fn a_server_riabuilds_own_key_already_reaches_fetches_nothing_at_all() {
    // The property that makes this feature free for a returning developer: no
    // fetch, no agent, and no org key ever in this process's memory.
    // can_sign_in succeeds.
    assert!(fake.spawns().is_empty(), "an agent was started: {:?}", fake.spawns());
}

#[tokio::test]
async fn an_issued_key_that_does_not_sign_in_falls_through_to_the_password() {
    // Probing is not committing: a key that fails leaves today's path intact.
}

#[tokio::test]
async fn the_run_continues_on_riabuilds_own_key_once_it_is_installed() {
    // The bootstrap rule. The issued key authenticates the copy and nothing
    // after it, so `remote forget` still has exactly one developer's line to
    // remove and the server's auth log still tells developers apart.
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd riabuild-cli && cargo test -p riabuild-remote authorise`
Expected: FAIL — arity mismatch on `authorise`.

- [ ] **Step 3: Implement**

In `authorise`, after the `can_sign_in` early return and after reading the public key,
insert the probe:

```rust
    // Asked only once riabuild's own key has already failed, which is what
    // keeps this free for every run after the first: `can_sign_in` above
    // returns early on a server already set up, so nothing is fetched, no
    // agent starts, and no org key is ever in this process's memory.
    if let Some(working) = issued.working(remote, paths, runner.clone(), ui).await {
        ui.working("Authorised", &format!("installing the key using {}", working.label));
        // Note what is *not* re-derived here: the methods probe below. An
        // identity that has just signed in has already answered the question
        // that probe asks, and asking again would spend a connection to be
        // told what we know.
        return finish_copy(remote, paths, runner, ui, &public_key,
                           Some(working), carry_on, paste).await;
    }
```

The existing `PreferredAuthentications=none` probe, the `!interactive` hard failure and
the `copy::install_key` call all stay exactly as they are on the `None` path.

`copy::install_key` gains an `Option<&Working>` and, when it is `Some`, adds
`-o IdentityAgent=<sock> -o IdentitiesOnly=yes -i <pub>` to its `ssh` invocation. Keep the
server-side `grep`-then-append script untouched — what changes is how the connection
authenticates, not what it does.

In `connect.rs`, construct `let mut issued = issued::Issued::new(&ctx.api);` before the
`request.check` branch, pass `&mut issued` to `authorise`, add the `--check` report, and
`issued.stop(ctx.runner.clone()).await` on both paths out.

- [ ] **Step 4: Run to verify they pass**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Update `authorise`'s module doc**

Its "When this stops, and when it does not" section currently says a server offering
neither `password` nor `keyboard-interactive` is a hard failure. That is no longer
unconditionally true, and the doc is the first thing the next reader trusts. Amend it.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/crates/remote
git commit -m "Let an issued key be the way in when riabuild's own key is not"
```

---

### Task 8: Full verification, PR, CI

- [ ] **Step 1: Both suites, clean**

```bash
cd riabuild-cli && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd ../riabuild-web && pnpm lint && pnpm test && pnpm ui:check
```

If a workspace doctest fails with "can't find crate" in a crate you never touched, re-run
before investigating — it is a shared-`target/` race, not a regression.

- [ ] **Step 2: Open the PR**

```bash
gh pr create --fill
```

- [ ] **Step 3: Watch checks to completion**

```bash
gh pr checks --watch
```

Work is not finished until CI has completed. If CI fails, fixing it is part of this task.

## Self-Review

**Spec coverage.** §1 → Task 2. §2 → Tasks 1 and 5. §3 → Task 2. §4 → Task 3. §5 → Task 4.
§6 → Task 5. §7 → Task 6. §8 → Task 7. §9 is a set of non-changes, each covered by a test
in Task 7 (`fetches_nothing_at_all`, `continues_on_riabuilds_own_key`). The E2E scenario
in the spec's Testing section is **not** in this plan — see below.

**Known gap, carried deliberately.** The spec calls for an e2e container with
`PasswordAuthentication no`. `e2e/` runs on macOS against real containers and is a
separate harness; adding a scenario there is a task of its own, and the unit coverage in
Task 7 pins the same behaviour. Raise it as a follow-up rather than letting it silently
not happen.

**Type consistency.** `IssuedKey` is the Rust struct (Task 5) and the TS `Data` type
(Task 4); the Convex row is `issuedKeys`. `Working` (Task 6) is what Task 7 consumes.
`serveForApi` is named identically in Tasks 2 and 3. The fixture constants
`ED25519_PRIVATE` / `ED25519_PUBLIC_NO_COMMENT` / `ED25519_FINGERPRINT` / `ENCRYPTED_PRIVATE`
are shared by Tasks 1, 2 and 5 on purpose: the two parsers must agree byte for byte.

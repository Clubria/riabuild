# Remote Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `riabuild remote` provisions a Linux or macOS server over SSH and opens a mosh shell on it, with the laptop holding the SSH identity, the server's riabuild session, and the GitHub sign-in the server borrows.

**Architecture:** The laptop drives; the server runs its own riabuild binary, so setup logic is never pushed over SSH. Per-developer state on the server lives in `~/.riabuild-remote/<public-id>/` while tools stay shared in `~/.riabuild/`, which lets several developers share one Unix account. The GitHub credential is seeded from the laptop into a per-session runtime directory and wiped when the last session ends.

**Tech Stack:** Rust 2024 on current-thread tokio, `async-trait`, `reqwest` with rustls, `clap`; Convex + React + Tailwind for riabuild-web; `vitest` + `convex-test` for the backend, Playwright for the dashboard.

**Design:** [`../specs/2026-08-06-remote-mode-design.md`](../specs/2026-08-06-remote-mode-design.md)

## Global Constraints

- **Every external process goes through `CommandRunner`.** No `std::process::Command` or `tokio::process` outside `runner.rs`. This includes `ssh`, `scp`, `ssh-keygen`, `ssh-keyscan`, `ssh-copy-id`, `mosh` and `uname`.
- **All IO is async.** `tokio::fs`, never `std::fs`. Exceptions already documented in `riabuild-cli/CLAUDE.md`: `ui.rs` stdio, `Paths` (pure path computation), `CommandRunner::which`, tarball extraction.
- **`clippy::unwrap_used` is denied outside tests.** Use `?`, `let else`, `ok_or_else`, or `.context(...)`.
- **`apply()` must be safe to run twice**, and is always followed by a re-run of `check()`.
- **`check()` is authoritative.** `version()` is only for drift a check cannot observe.
- **No secrets in `~/.riabuild/` on a laptop.** The one amendment this plan introduces is `<namespace>/session.token`, mode `0600`, on a **server only** — Task 20 also writes that amendment into `riabuild-cli/CLAUDE.md`.
- **Secrets never appear in an argument list.** They go through `RunOptions.stdin` or `RunOptions.env`, the way `env_local` already passes `INFISICAL_TOKEN`.
- **The version comes from the git tag.** Never `CARGO_PKG_VERSION`; local builds report `9999.0.0-dev`.
- **`/api/v1` fields are added, never removed or retyped.** The one break this plan makes — a required `publicId` — is argued in the spec and is safe only because riabuild-web deploys before a CLI release ships.
- **Every file stays under roughly 300 lines.** One responsibility per file.
- **Every PR runs** `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test` for the CLI, and `pnpm test` + `pnpm lint` for the web app. Work is not finished until PR CI has completed.
- **Naming:** the user-facing name is **remote mode**; SSH is the transport, never the feature name.

## Dependency on the Linux support work

Stage C cannot ship before the Linux design's PRs A and B are merged. It needs riabuild to own `gh` and `infisical` (no Homebrew on a server) and it needs published musl tarballs to install. **Stages A and B have no such dependency and can land immediately.** Task 17 names the exact release assets it expects.

## File Structure

**riabuild-web**

| File | Responsibility |
|---|---|
| `convex/schema.ts` | `members.publicId` |
| `convex/auth.ts` | mints a `publicId` when a member row is created |
| `convex/members.ts` | `memberView`/`toView` carry `publicId`; the backfill mutation |
| `convex/http.ts` | `memberPayload` returns `publicId` |
| `convex/devSeed.ts` | fixture members carry one |
| `src/ui/Copyable.tsx` | monospace value, truncated, with a copy button |
| `src/components/Profile.tsx` | the developer's own member id |
| `src/components/LeadPanel.tsx` | the member id column |
| `src/dev/scenarios.ts` | `overflow` gains a member id |

**riabuild-cli — Stage B (plumbing)**

| File | Responsibility |
|---|---|
| `src/paths.rs` | `tools_root()`, the `RIABUILD_ROOT` override, remote layout |
| `src/runner.rs` | `ScopedRunner`, the decorator that injects namespace environment |
| `src/keychain.rs` | `FileKeychain` and remote-aware `for_platform` |
| `src/scope.rs` | **new** — reads `RIABUILD_REMOTE`, decides remote behaviour |
| `src/download.rs` | asset naming for a target other than the running one |

**riabuild-cli — Stage C (remote mode)**

| File | Responsibility |
|---|---|
| `src/remote/mod.rs` | the `Remote` type, its hash, the flow |
| `src/remote/store.rs` | `remotes.json`, name allocation |
| `src/remote/identity.rs` | key pair, host key trust, `ssh-copy-id` |
| `src/remote/install.rs` | `uname`, version comparison, streaming the binary |
| `src/remote/session.rs` | minting the server's session, seeding GitHub |
| `src/remote/shell.rs` | mosh with the ssh fallback |
| `src/gh_session.rs` | server-side: runtime directory, markers, sweep, wipe |
| `src/ui.rs` | `ask`, `confirm` |
| `src/cli.rs` | the `remote` subcommand |

---

# Stage A — member ids

Lands on its own. Ships the dashboard change and the schema the namespace depends on.

### Task 1: `publicId` on members, minted and backfilled

**Files:**
- Modify: `riabuild-web/convex/schema.ts`
- Modify: `riabuild-web/convex/auth.ts:186`
- Modify: `riabuild-web/convex/members.ts`
- Modify: `riabuild-web/convex/devSeed.ts:49,137`
- Test: `riabuild-web/convex/api.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `members.publicId?: string` in the schema; `internal.members.backfillPublicIds` as an `internalMutation` taking no args and returning `v.number()` (how many rows it filled).

- [ ] **Step 1: Write the failing test**

Add to `riabuild-web/convex/api.test.ts`:

```ts
describe("member public ids", () => {
  test("a member created through sign-in gets a public id", async () => {
    const t = setup();
    const { memberId } = await seedMemberWithPublicId(t);
    const member = await t.run(async (ctx) => await ctx.db.get(memberId));
    expect(member?.publicId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );
  });

  test("the backfill fills only the rows that are missing one", async () => {
    const t = setup();
    const withId = await seedMemberWithPublicId(t);
    const withoutId = await t.run(async (ctx) => {
      const userId = await ctx.db.insert("users", { name: "Bob", email: "bob@clubria.dev" });
      return await ctx.db.insert("members", {
        userId,
        githubLogin: "bob",
        githubId: "5678",
        firstName: "Bob",
        lastName: "Stone",
        email: "bob@clubria.dev",
        role: "developer" as const,
        status: "active" as const,
      });
    });

    const before = await t.run(async (ctx) => (await ctx.db.get(withId.memberId))?.publicId);
    const filled = await t.mutation(internal.members.backfillPublicIds, {});

    expect(filled).toBe(1);
    const after = await t.run(async (ctx) => ({
      untouched: (await ctx.db.get(withId.memberId))?.publicId,
      filled: (await ctx.db.get(withoutId))?.publicId,
    }));
    expect(after.untouched).toBe(before);
    expect(after.filled).toBeTruthy();
  });

  test("running the backfill twice changes nothing the second time", async () => {
    const t = setup();
    await seedMember(t);
    await t.mutation(internal.members.backfillPublicIds, {});
    expect(await t.mutation(internal.members.backfillPublicIds, {})).toBe(0);
  });
});
```

Add the helper beside `seedMember`:

```ts
async function seedMemberWithPublicId(
  t: ReturnType<typeof setup>,
  overrides: Parameters<typeof seedMember>[1] = {},
) {
  const seeded = await seedMember(t, overrides);
  await t.mutation(internal.members.backfillPublicIds, {});
  return seeded;
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd riabuild-web && pnpm vitest run convex/api.test.ts -t "public id"`
Expected: FAIL — `internal.members.backfillPublicIds` does not exist.

- [ ] **Step 3: Add the field, the minting and the backfill**

`convex/schema.ts`, inside `members`, directly under `githubId`:

```ts
    /**
     * Immutable, ours, and independent of GitHub. Names a developer's
     * directory on a shared server, so it must outlive a GitHub rename.
     * Optional for exactly one deploy — see the backfill below.
     */
    publicId: v.optional(v.string()),
```

`convex/auth.ts`, in the `ctx.db.insert("members", {...})` at line 186, add:

```ts
      publicId: crypto.randomUUID(),
```

`convex/devSeed.ts`, the same line in both inserts.

`convex/members.ts`, at the end:

```ts
/**
 * One-shot: gives every member row a `publicId` so the field can be made
 * required. Idempotent, and returns how many rows it changed so the deploy
 * step can be checked rather than assumed.
 */
export const backfillPublicIds = internalMutation({
  args: {},
  returns: v.number(),
  handler: async (ctx) => {
    const members = await ctx.db.query("members").collect();
    let filled = 0;
    for (const member of members) {
      if (member.publicId !== undefined) continue;
      await ctx.db.patch(member._id, { publicId: crypto.randomUUID() });
      filled += 1;
    }
    return filled;
  },
});
```

Import `internalMutation` from `./_generated/server` if it is not already imported.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-web && pnpm vitest run convex/api.test.ts -t "public id"`
Expected: PASS, three tests.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/convex
git commit -m "Mint a public id for every member"
```

### Task 2: Serve `publicId`, and make it required

**Files:**
- Modify: `riabuild-web/convex/members.ts` (`memberView`, `toView`)
- Modify: `riabuild-web/convex/http.ts:150` (`memberPayload`)
- Modify: `riabuild-web/convex/schema.ts`
- Test: `riabuild-web/convex/api.test.ts`

**Interfaces:**
- Consumes: `members.publicId` from Task 1.
- Produces: `publicId: string` in `memberView` and in every `/api/v1` member payload — `GET /api/v1/me` and `POST /api/v1/cli/token`.

- [ ] **Step 1: Write the failing test**

```ts
test("every member payload carries the public id", async () => {
  const t = setup();
  const { memberId } = await seedMemberWithPublicId(t);
  const token = await seedSession(t, memberId);

  const response = await t.fetch("/api/v1/me", {
    headers: { authorization: `Bearer ${token}` },
  });
  const body = await response.json();

  expect(response.status).toBe(200);
  expect(body.member.publicId).toBeTruthy();
});
```

Use whichever session helper `api.test.ts` already defines for authenticated requests; if it is named differently from `seedSession`, use that name rather than adding another.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd riabuild-web && pnpm vitest run convex/api.test.ts -t "carries the public id"`
Expected: FAIL — `body.member.publicId` is `undefined`.

- [ ] **Step 3: Serve the field**

`convex/members.ts`, in `memberView`:

```ts
  publicId: v.string(),
```

and in `toView`:

```ts
    publicId: member.publicId ?? "",
```

`convex/http.ts`, in `memberPayload`:

```ts
    publicId: member.publicId,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd riabuild-web && pnpm vitest run convex/api.test.ts -t "carries the public id"`
Expected: PASS.

- [ ] **Step 5: Make the field required and drop the fallback**

`convex/schema.ts`:

```ts
    publicId: v.string(),
```

`convex/members.ts`, in `toView`, now that no row can be missing one:

```ts
    publicId: member.publicId,
```

- [ ] **Step 6: Run the whole backend suite**

Run: `cd riabuild-web && pnpm vitest run convex`
Expected: PASS. A failure here means a fixture inserts a member without a `publicId` — fix the fixture, not the schema.

- [ ] **Step 7: Commit**

```bash
git add riabuild-web/convex
git commit -m "Serve publicId, and require it"
```

> **Deploy note for the reviewer, not a code step.** Production takes this in three moves: deploy with the field optional, run `npx convex run members:backfillPublicIds --prod`, then deploy the required schema. The third deploy fails loudly if the backfill missed a row, which is the point of doing it in that order.

### Task 3: The CLI reads `public_id`

**Files:**
- Modify: `riabuild-cli/src/api/mod.rs:77-87` (`Member`)
- Test: `riabuild-cli/src/api/mod.rs` tests module

**Interfaces:**
- Consumes: `publicId` from Task 2.
- Produces: `Member::public_id: String`, non-optional, used by Stage C to name the server namespace.

- [ ] **Step 1: Write the failing test**

Add to the tests module in `riabuild-cli/src/api/mod.rs`:

```rust
#[test]
fn a_member_payload_carries_the_public_id() {
    let member: Member = serde_json::from_str(
        r#"{"githubLogin":"ada","githubId":"1234","publicId":"550e8400-e29b-41d4-a716-446655440000",
            "firstName":"Ada","lastName":"Lovelace","email":"ada@clubria.dev",
            "role":"developer","status":"active"}"#,
    )
    .expect("payload should parse");
    assert_eq!(member.public_id, "550e8400-e29b-41d4-a716-446655440000");
}

#[test]
fn a_payload_without_a_public_id_is_refused() {
    // A deployment older than this binary. Failing here is correct: the
    // alternative is a namespace directory named after an empty string,
    // silently shared by every developer on a server.
    let parsed = serde_json::from_str::<Member>(
        r#"{"githubLogin":"ada","githubId":"1234","firstName":"Ada","lastName":"Lovelace",
            "email":"ada@clubria.dev","role":"developer","status":"active"}"#,
    );
    assert!(parsed.is_err(), "a missing publicId must not default");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd riabuild-cli && cargo test a_member_payload_carries_the_public_id`
Expected: FAIL — no field `public_id` on `Member`.

- [ ] **Step 3: Add the field**

In `riabuild-cli/src/api/mod.rs`, inside `struct Member`:

```rust
    /// Immutable and ours. Names this developer's directory on a server.
    /// Deliberately not `#[serde(default)]`: an identifier that half the
    /// deployments might not send is not an identifier.
    #[serde(rename = "publicId")]
    pub public_id: String,
```

Every construction of `Member` in tests across the crate now needs the field. Add `public_id: "550e8400-e29b-41d4-a716-446655440000".into()` to each; `cargo test` will list them.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 5: Turn the decode failure into a sentence a developer can act on**

In `riabuild-cli/src/api/mod.rs`, wherever a response body is deserialised into `Member` (the `me()` and token-exchange paths), map the serde error:

```rust
.map_err(|error| {
    Failure::new(
        "reading your riabuild profile",
        "Ask your team lead to deploy the dashboard — this riabuild is newer than it.",
    )
    .detail(error.to_string())
})?
```

- [ ] **Step 6: Run the suite and commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add riabuild-cli/src
git commit -m "Read the member public id, and refuse a payload without one"
```

### Task 4: A `Copyable` component

**Files:**
- Create: `riabuild-web/src/ui/Copyable.tsx`
- Modify: `riabuild-web/src/ui/index.ts`
- Modify: `riabuild-web/src/routes/__ui` gallery source (follow the existing gallery file's structure)
- Test: `riabuild-web/e2e/` visual scenario, per the `visual-testing` skill

**Interfaces:**
- Consumes: nothing.
- Produces: `<Copyable value={string} label={string} />` — monospace, truncated to the first dash-segment, full value as the accessible name, with a copy button.

- [ ] **Step 1: Read the two skills that govern this**

Read `.claude/skills/riabuild-ui/SKILL.md` and `.claude/skills/visual-testing/SKILL.md` before writing any component code. They carry the no-keystrokes rule, the use-extend-generalize rule, and the requirement that a new component gets a gallery entry and a scenario.

- [ ] **Step 2: Write the component**

`riabuild-web/src/ui/Copyable.tsx`:

```tsx
import { useState } from "react";

/**
 * An opaque value a developer needs to copy but never to read aloud — a member
 * id, which names their directory on a shared server.
 *
 * Not a `Command` prop: `Command`'s `$` prompt means *this is a shell command*,
 * and an identifier is not one.
 */
export function Copyable({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const short = value.split("-")[0] || value;

  function copy() {
    void navigator.clipboard.writeText(value).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }

  return (
    <span className="inline-flex items-center gap-2">
      <span className="font-mono text-fg-dim" title={value} aria-label={`${label} ${value}`}>
        {short}…
      </span>
      <button
        type="button"
        onClick={copy}
        className="text-fg-faint hover:text-accent focus-visible:text-accent"
        aria-label={`Copy ${label}`}
      >
        {copied ? "copied" : "copy"}
      </button>
    </span>
  );
}
```

- [ ] **Step 3: Export it and add the gallery entry**

Add `export { Copyable } from "./Copyable";` to `riabuild-web/src/ui/index.ts`, and add a `Copyable` section to the `/__ui` gallery showing: a normal UUID, a value with no dashes, and an empty string.

- [ ] **Step 4: Check it renders in both suites**

Run: `cd riabuild-web && pnpm lint && pnpm test`
Then run the visual suite per `visual-testing` and look at the gallery screenshot. Expected: the value does not overflow its container, and the copy button has a visible focus ring.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src
git commit -m "Add Copyable, for identifiers you copy rather than read"
```

### Task 5: Show member ids in the dashboard

**Files:**
- Modify: `riabuild-web/src/components/Profile.tsx`
- Modify: `riabuild-web/src/components/LeadPanel.tsx:46-70` (the `columns` array)
- Modify: `riabuild-web/src/data/types.ts` (the `Member` type)
- Modify: `riabuild-web/src/dev/scenarios.ts` (every fixture member, and `overflow`)

**Interfaces:**
- Consumes: `Copyable` from Task 4, `publicId` from Task 2.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Add the field to the client-side type and every fixture**

In `src/data/types.ts`, add `publicId: string;` to `Member`. Then add a `publicId` to every fixture member in `src/dev/scenarios.ts`. In the `overflow` scenario use a full 36-character UUID, because an unbroken string with no spaces is exactly what that scenario exists to catch.

- [ ] **Step 2: Add the profile row**

In `Profile.tsx`, inside the panel, above the form:

```tsx
<KeyValue
  rows={[{ label: "member id", value: <Copyable value={member.publicId} label="member id" /> }]}
/>
```

Import `KeyValue` and `Copyable` from `../ui`. If `KeyValue`'s `value` prop is typed as `string` rather than `ReactNode`, widen it — generalizing an existing component with a prop is the rule; forking it is not.

- [ ] **Step 3: Add the member table column**

In `LeadPanel.tsx`, add to `columns` after the `name` column:

```tsx
    {
      key: "id",
      header: "member id",
      priority: "wide",
      render: (m) => <Copyable value={m.publicId} label={`member id for @${m.githubLogin}`} />,
    },
```

`priority: "wide"` so it is the first thing dropped on a narrow viewport — a member id matters less than knowing whose row it is.

- [ ] **Step 4: Run both suites and look at the screenshots**

Run: `cd riabuild-web && pnpm lint && pnpm test`
Then the visual suite at 380, 768 and 1440. Expected: no horizontal document overflow in any scenario, `overflow` included; the id column is absent at 380.

- [ ] **Step 5: Commit**

```bash
git add riabuild-web/src
git commit -m "Show member ids in the profile and the member table"
```

- [ ] **Step 6: Open the pull request for Stage A**

```bash
gh pr create --fill
gh pr checks --watch
```

CI must complete before this stage is done.

---

# Stage B — namespace plumbing

Invisible to a developer on a laptop: every test here asserts a laptop keeps behaving
exactly as it does today. It exists so Stage C has somewhere to stand.

### Task 6: A root that can move, and a tools root that does not

**Files:**
- Modify: `riabuild-cli/src/paths.rs`
- Test: `riabuild-cli/src/paths.rs` tests module

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `Paths::tools_root() -> PathBuf` — shared tools; equals `root()` on a laptop
  - `Paths::session_token_file() -> PathBuf` — `<root>/session.token`
  - `Paths::owner_file() -> PathBuf` — `<root>/owner.json`
  - `Paths::riabuild_dir(&self, version: &str) -> PathBuf` — `<tools_root>/riabuild/<version>`
  - `paths::root_for(home: &Path, override_root: Option<&str>) -> PathBuf`
  - `paths::remote_namespace(home: &Path, public_id: &str) -> PathBuf`
  - `RealPaths::with_root(home, root)`

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `riabuild-cli/src/paths.rs`:

```rust
#[test]
fn a_laptop_keeps_one_root_for_everything() {
    let paths = RealPaths::rooted_at("/Users/ada");
    assert_eq!(paths.root(), PathBuf::from("/Users/ada/.riabuild"));
    assert_eq!(paths.tools_root(), paths.root());
    assert_eq!(
        paths.node_dir("22.23.1"),
        PathBuf::from("/Users/ada/.riabuild/node/22.23.1")
    );
}

#[test]
fn a_server_namespaces_state_but_shares_tools() {
    let home = Path::new("/home/dev");
    let root = remote_namespace(home, "550e8400-e29b-41d4-a716-446655440000");
    let paths = RealPaths::with_root(home, &root);

    // State is one developer's.
    assert_eq!(
        paths.state_file(),
        PathBuf::from(
            "/home/dev/.riabuild-remote/550e8400-e29b-41d4-a716-446655440000/state.json"
        )
    );
    assert!(paths.claude_dir().starts_with(&root));
    assert!(paths.shell_dir("zsh").starts_with(&root));
    assert!(paths.bin_dir().starts_with(&root));
    assert_eq!(paths.session_token_file(), root.join("session.token"));

    // Tools are everybody's.
    assert_eq!(paths.tools_root(), PathBuf::from("/home/dev/.riabuild"));
    assert_eq!(
        paths.node_dir("22.23.1"),
        PathBuf::from("/home/dev/.riabuild/node/22.23.1")
    );
    assert_eq!(
        paths.riabuild_dir("2026.08.06"),
        PathBuf::from("/home/dev/.riabuild/riabuild/2026.08.06")
    );
}

#[test]
fn the_root_override_is_read_without_touching_the_environment() {
    // Pure, so the decision is testable without setting a process-wide variable
    // every other test in this binary would then see.
    let home = Path::new("/home/dev");
    assert_eq!(root_for(home, None), PathBuf::from("/home/dev/.riabuild"));
    assert_eq!(
        root_for(home, Some("/home/dev/.riabuild-remote/abc")),
        PathBuf::from("/home/dev/.riabuild-remote/abc")
    );
    // A relative or empty override is ignored rather than obeyed: it would put a
    // developer's state wherever the process happened to be standing.
    assert_eq!(root_for(home, Some("")), PathBuf::from("/home/dev/.riabuild"));
    assert_eq!(
        root_for(home, Some("relative/path")),
        PathBuf::from("/home/dev/.riabuild")
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test paths::`
Expected: FAIL — `tools_root`, `with_root`, `root_for`, `remote_namespace` do not exist.

- [ ] **Step 3: Implement**

Add to the `Paths` trait in `riabuild-cli/src/paths.rs`:

```rust
    /// Tools everyone on this machine shares: node, pnpm, gh, infisical, and
    /// riabuild itself. Equal to `root()` on a laptop; on a server it stays at
    /// `~/.riabuild` while `root()` moves into a per-developer namespace, so one
    /// Unix account holds one toolchain and several developers.
    fn tools_root(&self) -> PathBuf {
        self.root()
    }

    /// A server's own riabuild session. Never used on a laptop, where the
    /// platform keychain holds it instead.
    fn session_token_file(&self) -> PathBuf {
        self.root().join("session.token")
    }

    /// Who this namespace belongs to, in words, for whoever has a shell on the
    /// box and finds a directory named after a UUID.
    fn owner_file(&self) -> PathBuf {
        self.root().join("owner.json")
    }

    fn riabuild_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("riabuild").join(version)
    }
```

Change `node_dir` and `pnpm_dir` to hang off `tools_root()`:

```rust
    fn node_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("node").join(version)
    }
    fn pnpm_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("pnpm").join(version)
    }
```

Leave `state_file`, `config_file`, `org_settings_file`, `bin_dir`, `claude_dir`, `shell_dir`
and `log_file` on `root()`. Shims stay per-developer: they are regenerated every run and
cost nothing, and two developers rewriting one set of files concurrently is a race with no
upside.

Replace `RealPaths`:

```rust
pub struct RealPaths {
    home: PathBuf,
    root: PathBuf,
}

impl RealPaths {
    pub fn new() -> anyhow::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| {
            anyhow::anyhow!("riabuild could not work out your home directory (is $HOME set?)")
        })?;
        let root = root_for(&home, std::env::var("RIABUILD_ROOT").ok().as_deref());
        Ok(Self { home, root })
    }

    pub fn with_root(home: impl AsRef<Path>, root: impl AsRef<Path>) -> Self {
        Self {
            home: home.as_ref().to_path_buf(),
            root: root.as_ref().to_path_buf(),
        }
    }

    #[cfg(test)]
    pub fn rooted_at(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref().to_path_buf();
        let root = home.join(".riabuild");
        Self { home, root }
    }
}

impl Paths for RealPaths {
    fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn tools_root(&self) -> PathBuf {
        self.home.join(".riabuild")
    }
}
```

And two free functions:

```rust
/// Where riabuild keeps this developer's state.
///
/// Split out and pure so the decision is testable without setting an environment
/// variable every other test in the binary would then see. A relative override is
/// ignored: it would put state wherever the process happened to be standing,
/// which is never what anyone meant.
pub fn root_for(home: &Path, override_root: Option<&str>) -> PathBuf {
    match override_root {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        _ => home.join(".riabuild"),
    }
}

/// One developer's namespace on a shared server.
pub fn remote_namespace(home: &Path, public_id: &str) -> PathBuf {
    home.join(".riabuild-remote").join(public_id)
}
```

- [ ] **Step 4: Run the whole suite**

Run: `cd riabuild-cli && cargo test`
Expected: PASS. `RealPaths::rooted_at` keeps its old meaning, so every existing test is
unaffected — that is the property this step is checking.

- [ ] **Step 5: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src/paths.rs
git commit -m "Let the root move without moving the toolchain"
```

### Task 7: The checkout path on a server

**Files:**
- Modify: `riabuild-cli/src/paths.rs`
- Test: `riabuild-cli/src/paths.rs` tests module

**Interfaces:**
- Consumes: nothing from Task 6.
- Produces: `paths::remote_project_dir(home: &Path, login: &str, repo_name: &str) -> PathBuf`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_server_checkout_is_grouped_by_developer_and_avoids_documents() {
    // Not ~/Documents even on macOS: over SSH that directory is TCC-protected and
    // returns "Operation not permitted" unless sshd has Full Disk Access. One
    // answer on every platform is also one less branch to be wrong in.
    assert_eq!(
        remote_project_dir(Path::new("/home/dev"), "ada", "ai-builders-hub"),
        PathBuf::from("/home/dev/Clubria/ada/ai-builders-hub")
    );
    assert_eq!(
        remote_project_dir(Path::new("/Users/dev"), "bob", "ai-builders-hub"),
        PathBuf::from("/Users/dev/Clubria/bob/ai-builders-hub")
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd riabuild-cli && cargo test a_server_checkout_is_grouped`
Expected: FAIL — `remote_project_dir` not found.

- [ ] **Step 3: Implement**

```rust
/// Where a checkout lands on a server.
///
/// Grouped by GitHub login because a developer `cd`s into this every day and a
/// UUID is not a path anyone can read. Nothing durable rests on the name: the
/// absolute path is recorded in the namespace's `config.json` the first time it
/// is chosen, so a later GitHub rename changes nothing.
///
/// Never `~/Documents`, on any platform: macOS protects it from SSH sessions.
pub fn remote_project_dir(home: &Path, login: &str, repo_name: &str) -> PathBuf {
    home.join(ORG_DIR).join(login).join(repo_name)
}
```

Rename the constant `MACOS_ORG_DIR` to `ORG_DIR` in the same change — it is no longer only
macOS — and update its use in `default_project_dir_on`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test paths::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/src/paths.rs
git commit -m "Group server checkouts by developer, outside Documents"
```

### Task 8: `ScopedRunner`, so a task cannot forget the namespace

**Files:**
- Modify: `riabuild-cli/src/runner.rs`
- Test: `riabuild-cli/src/runner.rs` tests module

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `ScopedRunner::new(inner: Arc<dyn CommandRunner>, env: Vec<(String, String)>) -> ScopedRunner`
  - `FakeRunner::env_of(&self, prefix: &str) -> Vec<(String, String)>`

- [ ] **Step 1: Write the failing tests**

Add a tests module at the end of `riabuild-cli/src/runner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scoped_runner_puts_its_environment_on_every_command() {
        let fake = Arc::new(FakeRunner::new().with("gh auth status", 0, "", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/run/user/1000/riabuild-gh".into())],
        );

        scoped
            .run("gh", &["auth", "status"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("gh auth status"),
            vec![(
                "GH_CONFIG_DIR".to_string(),
                "/run/user/1000/riabuild-gh".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn a_caller_can_still_add_its_own_environment() {
        // `env_local` passes INFISICAL_TOKEN this way. The scope adds to that,
        // never replaces it.
        let fake = Arc::new(FakeRunner::new().with("infisical export", 0, "A=b\n", ""));
        let scoped =
            ScopedRunner::new(fake.clone(), vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())]);

        scoped
            .run(
                "infisical",
                &["export"],
                &RunOptions {
                    env: vec![("INFISICAL_TOKEN".into(), "st.secret".into())],
                    ..Default::default()
                },
            )
            .await
            .expect("runs");

        let env = fake.env_of("infisical export");
        assert!(env.contains(&("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())));
        assert!(env.contains(&("INFISICAL_TOKEN".to_string(), "st.secret".to_string())));
    }

    #[tokio::test]
    async fn an_interactive_command_is_scoped_too() {
        // `gh auth login` is interactive, and it is exactly the command that must
        // not write into another developer's configuration directory.
        let fake = Arc::new(FakeRunner::new().with("gh auth login", 0, "", ""));
        let scoped =
            ScopedRunner::new(fake.clone(), vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())]);

        scoped
            .run_interactive("gh", &["auth", "login"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("gh auth login"),
            vec![("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())]
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test runner::`
Expected: FAIL — `ScopedRunner` and `env_of` do not exist.

- [ ] **Step 3: Record the environment in `FakeRunner`**

Add to the `FakeRunner` struct:

```rust
    /// Invocation and the environment it was given, so a test can assert a task
    /// ran against the right configuration directory and not merely that it ran.
    pub recorded: std::sync::Mutex<Vec<(String, Vec<(String, String)>)>>,
```

In both `run` and `run_interactive`, rename the `_options` parameter to `options` and record
after the existing `calls` push:

```rust
        self.recorded
            .lock()
            .unwrap()
            .push((invocation.clone(), options.env.clone()));
```

Add the accessor:

```rust
    /// The environment the first matching invocation was run with.
    pub fn env_of(&self, prefix: &str) -> Vec<(String, String)> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .find(|(invocation, _)| invocation.starts_with(prefix))
            .map(|(_, env)| env.clone())
            .unwrap_or_default()
    }
```

- [ ] **Step 4: Write `ScopedRunner`**

At the end of `runner.rs`, before the test module:

```rust
/// A `CommandRunner` that adds a fixed environment to every command.
///
/// This is why `github_cli` cannot authenticate the wrong developer on a shared
/// server. `GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` are not something each task
/// remembers to pass — the runner every task already holds carries them, so a
/// task that forgets is not a thing anyone can write.
///
/// Caller environment is applied after the scope's, so a task passing its own
/// variable — `env_local` and `INFISICAL_TOKEN` — still wins.
pub struct ScopedRunner {
    inner: Arc<dyn CommandRunner>,
    env: Vec<(String, String)>,
}

impl ScopedRunner {
    pub fn new(inner: Arc<dyn CommandRunner>, env: Vec<(String, String)>) -> Self {
        Self { inner, env }
    }

    fn merge(&self, options: &RunOptions) -> RunOptions {
        let mut merged = options.clone();
        let mut env = self.env.clone();
        env.extend(options.env.iter().cloned());
        merged.env = env;
        merged
    }
}

#[async_trait]
impl CommandRunner for ScopedRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
        self.inner.run(program, args, &self.merge(options)).await
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        self.inner
            .run_interactive(program, args, &self.merge(options))
            .await
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.inner.which(program)
    }
}
```

Add `use std::sync::Arc;` to the imports at the top of the file.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src/runner.rs
git commit -m "Scope every command to its namespace, in the runner"
```

### Task 9: A file-backed token store, for servers only

**Files:**
- Modify: `riabuild-cli/src/keychain.rs`
- Modify: `riabuild-cli/src/main.rs` (the one call site)
- Test: `riabuild-cli/src/keychain.rs` tests module

**Interfaces:**
- Consumes: `Paths::session_token_file()` from Task 6.
- Produces:
  - `FileKeychain::new(path: PathBuf) -> FileKeychain`
  - `keychain::for_platform(runner: Arc<dyn CommandRunner>, session_token_file: Option<PathBuf>) -> Box<dyn Keychain>` — `Some` means this is a server

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_server_keeps_its_session_in_a_file_it_owns() {
    let home = TempDir::new().expect("tempdir");
    let path = home.path().join("session.token");
    let keychain = FileKeychain::new(path.clone());

    assert_eq!(keychain.get().await.expect("read"), None);
    keychain.set("rb_live_token").await.expect("write");
    assert_eq!(
        keychain.get().await.expect("read"),
        Some("rb_live_token".to_string())
    );

    keychain.delete().await.expect("delete");
    assert_eq!(keychain.get().await.expect("read"), None);
    // Deleting what is already gone is not an error: `apply()` runs twice.
    keychain.delete().await.expect("delete again");
}

#[cfg(unix)]
#[tokio::test]
async fn the_session_file_is_not_readable_by_anyone_else() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().expect("tempdir");
    let path = home.path().join("session.token");
    FileKeychain::new(path.clone())
        .set("rb_live_token")
        .await
        .expect("write");

    let mode = tokio::fs::metadata(&path)
        .await
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "session.token must be 0600");
}

#[tokio::test]
async fn writing_the_token_twice_replaces_it() {
    let home = TempDir::new().expect("tempdir");
    let keychain = FileKeychain::new(home.path().join("session.token"));
    keychain.set("first").await.expect("write");
    keychain.set("second").await.expect("write");
    assert_eq!(
        keychain.get().await.expect("read"),
        Some("second".to_string())
    );
}

#[test]
fn a_server_never_reaches_for_a_keyring() {
    // A macOS server is what makes this a rule rather than a preference:
    // `security` cannot open a login keychain an SSH session has not unlocked,
    // so asking the platform first would pick a store that always fails.
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
    let remote = for_platform(
        runner.clone(),
        Some(PathBuf::from("/home/dev/ns/session.token")),
    );
    assert_eq!(remote.describe(), "this server's riabuild namespace");
}
```

Add `use tempfile::TempDir;` to the test module imports.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test keychain::`
Expected: FAIL — `FileKeychain` does not exist and `for_platform` takes one argument.

- [ ] **Step 3: Implement `FileKeychain`**

```rust
/// A server's own session, in the developer's namespace at 0600.
///
/// The one exception to "no secrets in ~/.riabuild", argued in the remote mode
/// design: a server has no keyring, the token is minted for that server alone,
/// it is labelled and listed in the dashboard, and `riabuild remote forget`
/// revokes it. What the invariant exists to protect — the Infisical credential —
/// is still brokered per use and still never written down.
pub struct FileKeychain {
    path: PathBuf,
}

impl FileKeychain {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl Keychain for FileKeychain {
    async fn get(&self) -> Result<Option<String>> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => {
                let token = text.trim().to_string();
                Ok((!token.is_empty()).then_some(token))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn set(&self, token: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        write_private_token(&self.path, token).await
    }

    async fn delete(&self) -> Result<()> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn describe(&self) -> &'static str {
        "this server's riabuild namespace"
    }
}

#[cfg(unix)]
async fn write_private_token(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await?;
    file.write_all(contents.as_bytes()).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn write_private_token(path: &Path, contents: &str) -> Result<()> {
    tokio::fs::write(path, contents).await?;
    Ok(())
}
```

Add `use std::path::{Path, PathBuf};` to the imports. `env_local.rs` has a `write_private`
of its own; leave it there rather than sharing one — they are a few lines each, and coupling
two unrelated files to save them is the worse trade.

- [ ] **Step 4: Make `for_platform` remote-aware**

```rust
/// Picks the right store for this machine.
///
/// Order matters. An explicit `RIABUILD_TOKEN` wins so automation can run with no
/// store at all. A server comes next, *before* any platform question: a macOS
/// server has `security(1)` and a login keychain an SSH session cannot unlock, so
/// asking the platform first would pick a store that always fails.
pub fn for_platform(
    runner: Arc<dyn CommandRunner>,
    session_token_file: Option<PathBuf>,
) -> Box<dyn Keychain> {
    if std::env::var("RIABUILD_TOKEN").is_ok_and(|value| !value.is_empty()) {
        return Box::new(EnvKeychain);
    }
    if let Some(path) = session_token_file {
        return Box::new(FileKeychain::new(path));
    }
    if cfg!(target_os = "macos") {
        return Box::new(SecurityCliKeychain::new(runner));
    }
    Box::new(SecretToolKeychain::new(runner))
}
```

Update the call site in `main.rs` to pass `None`; Task 10 supplies the real value.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src/keychain.rs riabuild-cli/src/main.rs
git commit -m "Give a server a token store it can actually use"
```

### Task 10: `scope.rs` — one place that knows this riabuild is managed

**Files:**
- Create: `riabuild-cli/src/scope.rs`
- Modify: `riabuild-cli/src/main.rs`
- Modify: `riabuild-cli/CLAUDE.md`
- Modify: `docs/superpowers/specs/2026-08-06-remote-mode-design.md`
- Test: `riabuild-cli/src/scope.rs` tests module

**Interfaces:**
- Consumes: `paths::root_for` (Task 6), `keychain::for_platform` (Task 9).
- Produces:
  - `scope::Scope { pub server: Option<String> }`
  - `Scope::read(value: Option<&str>) -> Scope` — pure
  - `Scope::detect() -> Scope` — reads `RIABUILD_REMOTE`
  - `Scope::is_remote(&self) -> bool`
  - `Scope::banner(&self) -> String`

> **Refinement to fold into the spec in this task.** The spec writes the variable as
> `RIABUILD_REMOTE=1` *and* asks the shell banner to name the server. One variable does
> both if it carries the **name**: `RIABUILD_REMOTE=build-01`. Any non-empty value means
> remote. Update the spec's environment table in the same commit.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_variable_is_a_laptop() {
        assert!(!Scope::read(None).is_remote());
        assert!(!Scope::read(Some("")).is_remote());
    }

    #[test]
    fn a_named_server_is_remote_and_names_itself() {
        let scope = Scope::read(Some("build-01"));
        assert!(scope.is_remote());
        assert!(scope.banner().contains("build-01"), "{}", scope.banner());
        assert!(
            scope.banner().contains("exit"),
            "the way out is always on screen"
        );
    }

    #[test]
    fn a_laptop_banner_is_the_one_it_always_was() {
        assert_eq!(Scope::read(None).banner(), crate::shell::BANNER);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test scope::`
Expected: FAIL — no module `scope`.

- [ ] **Step 3: Write the module**

`riabuild-cli/src/scope.rs`:

```rust
//! Whether this riabuild runs on a developer's own machine or on a server a
//! laptop provisions.
//!
//! One variable, `RIABUILD_REMOTE`, carrying the server's name. Four things
//! follow from it, and they are one idea — *this riabuild is managed from a
//! laptop*:
//!
//! - the session lives in a file in the namespace, not in a keyring
//! - the GitHub configuration lives in a per-session runtime directory
//! - self-update is suppressed, because no package manager owns this binary
//! - the shell banner says which server you are on

pub struct Scope {
    /// The server's name, when riabuild is running on one.
    pub server: Option<String>,
}

impl Scope {
    /// Split from `detect` so the decision is testable without setting a
    /// process-wide variable every other test in this binary would then see.
    pub fn read(value: Option<&str>) -> Scope {
        Scope {
            server: value.filter(|name| !name.is_empty()).map(str::to_string),
        }
    }

    pub fn detect() -> Scope {
        Scope::read(std::env::var("RIABUILD_REMOTE").ok().as_deref())
    }

    pub fn is_remote(&self) -> bool {
        self.server.is_some()
    }

    pub fn banner(&self) -> String {
        match &self.server {
            Some(name) => format!(
                "● Clubria environment active on {name} — type `exit` to leave, \
                 `claude` to start working"
            ),
            None => crate::shell::BANNER.to_string(),
        }
    }
}
```

- [ ] **Step 4: Wire it into `main.rs`**

Add `mod scope;` beside the other modules. In `run`:

```rust
    let scope = scope::Scope::detect();
    let paths: Arc<dyn Paths> = Arc::new(RealPaths::new()?);
    let runner: Arc<dyn CommandRunner> = Arc::new(RealRunner);
    let session_token_file = scope.is_remote().then(|| paths.session_token_file());
    let keychain: Arc<dyn keychain::Keychain> =
        Arc::from(keychain::for_platform(runner.clone(), session_token_file));
```

In `provision`, guard the update check so a managed binary never upgrades itself:

```rust
    if let Some(org) = &ctx.org
        && !scope.is_remote()
    {
```

In `open_shell`, replace `ctx.ui.info(shell::BANNER)` with `ctx.ui.info(&scope.banner())`.

Thread `&Scope` into `provision` and `open_shell` as a parameter rather than storing it on
`Ctx`. It is read three times in one file, and putting it on `Ctx` would invite tasks to
branch on it — which is exactly what `ScopedRunner` exists to make unnecessary.

- [ ] **Step 5: Amend the invariant in `riabuild-cli/CLAUDE.md`**

Under **No secrets in `~/.riabuild/`**, append:

```markdown
A riabuild-managed **server** is the one exception: it may hold its own session
token at `<namespace>/session.token`, mode 0600. It has no keyring, the token is
minted for that server alone, it is labelled and listed in the dashboard, and
`riabuild remote forget` revokes it. Laptops are unchanged, and the Infisical
credential is still brokered per use and never written down.
```

- [ ] **Step 6: Update the spec's environment table**

In `docs/superpowers/specs/2026-08-06-remote-mode-design.md`, change `RIABUILD_REMOTE=1` to
`RIABUILD_REMOTE=<server name>` and note that any non-empty value means remote.

- [ ] **Step 7: Run the suite and commit**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add riabuild-cli/src riabuild-cli/CLAUDE.md docs/superpowers/specs
git commit -m "Teach riabuild when it is the managed end of a laptop"
```

### Task 11: Release assets for a platform we are not

**Files:**
- Modify: `riabuild-cli/src/download.rs`
- Test: `riabuild-cli/src/download.rs` tests module

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `download::rust_target(uname_s: &str, uname_m: &str) -> Result<String>`
  - `download::riabuild_asset(version: &str, target: &str) -> String`
  - `download::riabuild_asset_url(version: &str, target: &str) -> String`
  - `download::riabuild_checksums_url(version: &str) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn uname_output_maps_to_the_target_the_release_publishes() {
    // Captured from real `uname -sm` output. Apple's arm64 is Rust's aarch64,
    // and Linux binaries are musl so one build runs on every distribution rather
    // than on everything newer than the runner's glibc.
    assert_eq!(
        rust_target("Darwin", "arm64").expect("mac"),
        "aarch64-apple-darwin"
    );
    assert_eq!(
        rust_target("Darwin", "x86_64").expect("mac"),
        "x86_64-apple-darwin"
    );
    assert_eq!(
        rust_target("Linux", "x86_64").expect("linux"),
        "x86_64-unknown-linux-musl"
    );
    assert_eq!(
        rust_target("Linux", "aarch64").expect("linux"),
        "aarch64-unknown-linux-musl"
    );
    // Some distributions report arm64 rather than aarch64.
    assert_eq!(
        rust_target("Linux", "arm64").expect("linux"),
        "aarch64-unknown-linux-musl"
    );
    // `uname` output arrives with a trailing newline.
    assert_eq!(
        rust_target("Linux\n", "x86_64\n").expect("linux"),
        "x86_64-unknown-linux-musl"
    );
}

#[test]
fn an_unpublished_platform_is_an_error_rather_than_a_guess() {
    // Installing the wrong architecture produces an exec format error on the
    // server with nothing in it that names riabuild.
    assert!(rust_target("Linux", "i686").is_err());
    assert!(rust_target("Linux", "armv7l").is_err());
    assert!(rust_target("FreeBSD", "x86_64").is_err());
    assert!(rust_target("Darwin", "ppc").is_err());
}

#[test]
fn asset_names_match_what_the_release_workflow_uploads() {
    // release.yml builds `riabuild-$version-$target.tar.gz` and appends each
    // digest to `riabuild-$version-checksums.txt`. If either is renamed there,
    // this test is what fails.
    assert_eq!(
        riabuild_asset("2026.08.06", "aarch64-apple-darwin"),
        "riabuild-2026.08.06-aarch64-apple-darwin.tar.gz"
    );
    assert_eq!(
        riabuild_asset_url("2026.08.06", "x86_64-unknown-linux-musl"),
        "https://github.com/Clubria/riabuild/releases/download/v2026.08.06/riabuild-2026.08.06-x86_64-unknown-linux-musl.tar.gz"
    );
    assert_eq!(
        riabuild_checksums_url("2026.08.06"),
        "https://github.com/Clubria/riabuild/releases/download/v2026.08.06/riabuild-2026.08.06-checksums.txt"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test download::`
Expected: FAIL — none of the four functions exist.

- [ ] **Step 3: Implement**

```rust
const RELEASES: &str = "https://github.com/Clubria/riabuild/releases/download";

/// The Rust target triple a server's `uname -sm` corresponds to.
pub fn rust_target(uname_s: &str, uname_m: &str) -> Result<String> {
    let arch = match uname_m.trim() {
        "x86_64" | "amd64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => anyhow::bail!("riabuild does not publish a build for {other}"),
    };
    match uname_s.trim() {
        "Darwin" => Ok(format!("{arch}-apple-darwin")),
        "Linux" => Ok(format!("{arch}-unknown-linux-musl")),
        other => anyhow::bail!("riabuild does not publish a build for {other}"),
    }
}

pub fn riabuild_asset(version: &str, target: &str) -> String {
    format!("riabuild-{version}-{target}.tar.gz")
}

pub fn riabuild_asset_url(version: &str, target: &str) -> String {
    format!("{RELEASES}/v{version}/{}", riabuild_asset(version, target))
}

pub fn riabuild_checksums_url(version: &str) -> String {
    format!("{RELEASES}/v{version}/riabuild-{version}-checksums.txt")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test download::`
Expected: PASS.

- [ ] **Step 5: Commit and open the Stage B pull request**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add riabuild-cli/src/download.rs
git commit -m "Name the release asset for a platform we are not running on"
gh pr create --fill
gh pr checks --watch
```

---

# Stage C — remote mode

**Blocked on the Linux support PRs A and B.** Do not start this stage until riabuild owns
`gh` and `infisical` on both platforms, and until `release.yml` publishes musl tarballs
beside the darwin ones.

### Task 12: Prompts that refuse to hang

**Files:**
- Modify: `riabuild-cli/src/ui.rs`
- Test: `riabuild-cli/src/ui.rs` tests module

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `Ui::ask(&self, label: &str, default: Option<&str>) -> Result<String>`
  - `Ui::confirm(&self, question: &str) -> Result<bool>`
  - `ui::answer_or_default(input: &str, default: Option<&str>) -> Option<String>`
  - `ui::is_yes(input: &str) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_empty_answer_takes_the_default_and_a_typed_one_wins() {
    assert_eq!(answer_or_default("", Some("22")), Some("22".into()));
    assert_eq!(answer_or_default("  \n", Some("22")), Some("22".into()));
    assert_eq!(answer_or_default("2222\n", Some("22")), Some("2222".into()));
    assert_eq!(answer_or_default("  ada  ", None), Some("ada".into()));
    // No default and no answer is not an answer.
    assert_eq!(answer_or_default("", None), None);
}

#[test]
fn confirmation_defaults_to_no() {
    // The fingerprint prompt is the one this exists for. Anything other than an
    // explicit yes has to mean no, or a developer pressing return through a
    // prompt they did not read trusts a host key they have never seen.
    assert!(is_yes("y"));
    assert!(is_yes("Y\n"));
    assert!(is_yes("yes"));
    assert!(!is_yes(""));
    assert!(!is_yes("\n"));
    assert!(!is_yes("n"));
    assert!(!is_yes("sure"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test ui::`
Expected: FAIL — `answer_or_default` and `is_yes` do not exist.

- [ ] **Step 3: Implement**

Add to `riabuild-cli/src/ui.rs`, outside the `impl Ui`:

```rust
/// What a typed answer means, given a default. Pure, so the rules are testable
/// without a terminal.
pub fn answer_or_default(input: &str, default: Option<&str>) -> Option<String> {
    let typed = input.trim();
    if !typed.is_empty() {
        return Some(typed.to_string());
    }
    default.map(str::to_string)
}

/// Only an explicit yes is a yes. Pressing return through a prompt nobody read
/// must not trust a host key.
pub fn is_yes(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
```

And inside `impl Ui`:

```rust
    /// Asks for one value, showing the default in brackets.
    ///
    /// Blocking stdio, like the rest of this file: `ui.rs` is the documented
    /// exception to the async-IO rule, and a prompt is a handoff to the terminal
    /// rather than IO riabuild performs.
    pub fn ask(&self, label: &str, default: Option<&str>) -> Result<String> {
        if !std::io::stdin().is_terminal() {
            return Err(Failure::new(
                format!("asking you for {label}"),
                "Pass the server as `riabuild remote <user>@<host>:<port>` — \
                 there is no terminal here to ask in.",
            )
            .into());
        }
        loop {
            match default {
                Some(value) => print!("  {label} [{value}] "),
                None => print!("  {label} "),
            }
            std::io::stdout().flush()?;

            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                // stdin closed mid-prompt.
                return Err(Failure::new(
                    format!("asking you for {label}"),
                    "Run `riabuild remote` again from a terminal.",
                )
                .into());
            }
            if let Some(answer) = answer_or_default(&line, default) {
                return Ok(answer);
            }
        }
    }

    pub fn confirm(&self, question: &str) -> Result<bool> {
        if !std::io::stdin().is_terminal() {
            return Err(Failure::new(
                format!("asking you to confirm: {question}"),
                "Run `riabuild remote` from a terminal, where you can answer this.",
            )
            .into());
        }
        print!("  {question} [y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(is_yes(&line))
    }
```

Add `use anyhow::Result;` and extend the existing `use std::io::{IsTerminal, Write};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test ui::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src/ui.rs
git commit -m "Ask for a value, and refuse to ask where nobody can answer"
```

### Task 13: The `Remote` type and its store

**Files:**
- Create: `riabuild-cli/src/remote/mod.rs`
- Create: `riabuild-cli/src/remote/store.rs`
- Modify: `riabuild-cli/src/main.rs` (`mod remote;`)
- Test: both new files' tests modules

**Interfaces:**
- Consumes: `download::sha256_hex`.
- Produces:
  - `Remote { pub name: String, pub host: String, pub port: u16, pub user: String }`
  - `Remote::hash(&self) -> String` — 16 hex characters
  - `Remote::target(&self) -> String` — `user@host`
  - `Remote::parse(spec: &str, default_user: &str) -> Result<Remote>` — `ada@host:2222`
  - `store::Store { pub remotes: Vec<Record> }` with `Record { name, hash, host, port, user, added_at, last_used_at, session_expires_at, last_seen_cli_version }`
  - `Store::load(paths: &dyn Paths) -> Store` (infallible), `Store::save(&self, paths) -> Result<()>`
  - `store::allocate_name(host: &str, taken: &[String]) -> String`
  - `Paths::remotes_file()` — `<root>/remotes.json`, `Paths::identity_dir()`, `Paths::ssh_dir()`

- [ ] **Step 1: Write the failing tests**

`riabuild-cli/src/remote/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_three_answers_always_produce_the_same_key() {
        // This is what makes the whole flow safe to re-run: a second
        // `riabuild remote` finds the key it made the first time.
        let one = Remote { name: "build-01".into(), host: "build-01.fly.dev".into(), port: 22, user: "ada".into() };
        let two = Remote { name: "anything-else".into(), ..one.clone() };
        assert_eq!(one.hash(), two.hash(), "the local name is not part of identity");
        assert_eq!(one.hash().len(), 16);
        assert!(one.hash().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_different_user_or_port_is_a_different_server() {
        let base = Remote { name: "b".into(), host: "box".into(), port: 22, user: "ada".into() };
        let other_user = Remote { user: "bob".into(), ..base.clone() };
        let other_port = Remote { port: 2222, ..base.clone() };
        assert_ne!(base.hash(), other_user.hash());
        assert_ne!(base.hash(), other_port.hash());
    }

    #[test]
    fn a_target_is_parsed_the_way_it_is_typed() {
        let parsed = Remote::parse("ada@build-01.fly.dev:2222", "local").expect("parses");
        assert_eq!(parsed.user, "ada");
        assert_eq!(parsed.host, "build-01.fly.dev");
        assert_eq!(parsed.port, 2222);
        assert_eq!(parsed.name, "build-01");

        let defaults = Remote::parse("build-01.fly.dev", "ada").expect("parses");
        assert_eq!(defaults.user, "ada");
        assert_eq!(defaults.port, 22);
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed_at() {
        assert!(Remote::parse("", "ada").is_err());
        assert!(Remote::parse("ada@", "ada").is_err());
        assert!(Remote::parse("host:not-a-port", "ada").is_err());
        assert!(Remote::parse("host:0", "ada").is_err());
        assert!(Remote::parse("host:99999", "ada").is_err());
    }
}
```

`riabuild-cli/src/remote/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_comes_from_the_first_label_of_the_hostname() {
        assert_eq!(allocate_name("build-01.fly.dev", &[]), "build-01");
        assert_eq!(allocate_name("gpu.internal", &[]), "gpu");
        assert_eq!(allocate_name("192.168.1.10", &[]), "192");
    }

    #[test]
    fn a_taken_name_is_numbered_rather_than_reused() {
        let taken = vec!["build".to_string(), "build-2".to_string()];
        assert_eq!(allocate_name("build.example.com", &taken), "build-3");
    }

    #[test]
    fn a_hostname_with_nothing_usable_in_it_still_gets_a_name() {
        assert_eq!(allocate_name("", &[]), "server");
        assert_eq!(allocate_name("...", &[]), "server");
    }

    #[tokio::test]
    async fn an_unreadable_store_means_no_saved_servers_rather_than_an_error() {
        // Same rule as state.json: a file we cannot parse must degrade, never
        // stop a developer from connecting.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.expect("mkdir");
        tokio::fs::write(paths.remotes_file(), "{{{ not json").await.expect("write");

        assert!(Store::load(&paths).await.remotes.is_empty());
    }

    #[tokio::test]
    async fn a_store_round_trips() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let mut store = Store::default();
        store.remotes.push(Record {
            name: "build-01".into(),
            hash: "9f2c000000000000".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
            added_at: 1,
            last_used_at: 2,
            session_expires_at: 0,
            last_seen_cli_version: "2026.08.06".into(),
        });
        store.save(&paths).await.expect("save");

        let loaded = Store::load(&paths).await;
        assert_eq!(loaded.remotes.len(), 1);
        assert_eq!(loaded.remotes[0].name, "build-01");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::`
Expected: FAIL — no module `remote`.

- [ ] **Step 3: Add the paths**

In `riabuild-cli/src/paths.rs`, add to the `Paths` trait:

```rust
    fn remotes_file(&self) -> PathBuf {
        self.root().join("remotes.json")
    }
    /// Key pairs, one per server.
    fn identity_dir(&self) -> PathBuf {
        self.root().join("ssh-identities")
    }
    /// riabuild's own SSH configuration. Never `~/.ssh`: a bad write there
    /// breaks SSH for everything on the machine, not just for riabuild.
    fn ssh_dir(&self) -> PathBuf {
        self.root().join("ssh")
    }
    fn known_hosts_file(&self) -> PathBuf {
        self.ssh_dir().join("known_hosts")
    }
```

- [ ] **Step 4: Write `remote/mod.rs`**

```rust
//! Remote mode: provisioning a server and opening a shell on it.
//!
//! The laptop drives. The server runs its own riabuild binary, so setup logic is
//! never pushed over SSH — see the remote mode design.

pub mod identity;
pub mod install;
pub mod session;
pub mod shell;
pub mod store;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// A local label only. The server never sees it.
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
}

impl Remote {
    /// Identifies the server for key and session storage.
    ///
    /// Over what the developer typed rather than a resolved address, so `box` and
    /// `box.example.com` are two servers. Predictable beats clever, and the same
    /// three answers must always find the same key.
    pub fn hash(&self) -> String {
        let digest = crate::download::sha256_hex(
            format!("{}@{}:{}", self.user, self.host, self.port).as_bytes(),
        );
        digest[..16].to_string()
    }

    pub fn target(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// `[user@]host[:port]`, with the local login as the default user.
    pub fn parse(spec: &str, default_user: &str) -> Result<Remote> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(anyhow!("no server given"));
        }
        let (user, rest) = match spec.split_once('@') {
            Some((user, rest)) if !user.is_empty() => (user.to_string(), rest),
            Some(_) => return Err(anyhow!("that has an empty username in it")),
            None => (default_user.to_string(), spec),
        };
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => {
                let port: u16 = port
                    .parse()
                    .map_err(|_| anyhow!("`{port}` is not a port number"))?;
                if port == 0 {
                    return Err(anyhow!("0 is not a port number"));
                }
                (host.to_string(), port)
            }
            None => (rest.to_string(), 22),
        };
        if host.is_empty() {
            return Err(anyhow!("that has no hostname in it"));
        }
        let name = store::allocate_name(&host, &[]);
        Ok(Remote { name, host, port, user })
    }
}
```

- [ ] **Step 5: Write `remote/store.rs`**

```rust
//! `remotes.json` — the servers this laptop knows about.

use crate::paths::Paths;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub name: String,
    pub hash: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub added_at: u64,
    pub last_used_at: u64,
    /// When the session minted for this server runs out.
    pub session_expires_at: u64,
    pub last_seen_cli_version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub remotes: Vec<Record>,
}

impl Store {
    /// Infallible, like `State::load`: a file we cannot parse means we know of no
    /// servers, and the correct response is to ask rather than to stop.
    pub async fn load(paths: &dyn Paths) -> Store {
        let Ok(text) = tokio::fs::read_to_string(paths.remotes_file()).await else {
            return Store::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub async fn save(&self, paths: &dyn Paths) -> Result<()> {
        crate::config::write_json(&paths.remotes_file(), self).await
    }

    pub fn find(&self, name: &str) -> Option<&Record> {
        self.remotes.iter().find(|record| record.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        self.remotes.iter().map(|r| r.name.clone()).collect()
    }
}

/// A short local label, from the first label of the hostname.
pub fn allocate_name(host: &str, taken: &[String]) -> String {
    let base: String = host
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let base = if base.is_empty() { "server".to_string() } else { base };

    if !taken.iter().any(|name| name == &base) {
        return base;
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !taken.iter().any(|name| name == &candidate) {
            return candidate;
        }
    }
    base
}
```

Check whether `config::write_json` is already async in this tree; if it is still
synchronous, use it as it stands rather than changing it here.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test remote::`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src
git commit -m "Name a server, hash it, and remember it"
```

### Task 14: The `remote` subcommand

**Files:**
- Modify: `riabuild-cli/src/cli.rs`
- Modify: `riabuild-cli/src/main.rs`
- Test: `riabuild-cli/src/cli.rs` tests module

**Interfaces:**
- Consumes: `Remote::parse` from Task 13.
- Produces: `Command::Remote { target: Option<String>, action: Option<RemoteAction> }` where `RemoteAction` is `List | Forget { name: String }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn bare_remote_reconnects_to_what_is_saved() {
    let cli = Cli::parse_from(["riabuild", "remote"]);
    assert!(matches!(
        cli.command,
        Some(Command::Remote { target: None, action: None })
    ));
}

#[test]
fn a_remote_can_be_named_or_spelled_out() {
    let by_name = Cli::parse_from(["riabuild", "remote", "build-01"]);
    let Some(Command::Remote { target: Some(target), .. }) = by_name.command else {
        panic!("expected a target");
    };
    assert_eq!(target, "build-01");

    let spelled = Cli::parse_from(["riabuild", "remote", "ada@box:2222"]);
    let Some(Command::Remote { target: Some(target), .. }) = spelled.command else {
        panic!("expected a target");
    };
    assert_eq!(target, "ada@box:2222");
}

#[test]
fn remote_has_list_and_forget() {
    let list = Cli::parse_from(["riabuild", "remote", "list"]);
    assert!(matches!(
        list.command,
        Some(Command::Remote { action: Some(RemoteAction::List), .. })
    ));

    let forget = Cli::parse_from(["riabuild", "remote", "forget", "build-01"]);
    let Some(Command::Remote { action: Some(RemoteAction::Forget { name }), .. }) = forget.command
    else {
        panic!("expected forget");
    };
    assert_eq!(name, "build-01");
}

#[test]
fn the_check_flag_still_works_with_remote() {
    let cli = Cli::parse_from(["riabuild", "--check", "remote", "build-01"]);
    assert!(cli.check);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test cli::`
Expected: FAIL — no `Remote` variant.

- [ ] **Step 3: Implement**

In `riabuild-cli/src/cli.rs`, add to `Command`:

```rust
    /// Set up a server and open the Clubria environment on it.
    Remote {
        /// A saved server's name, or `[user@]host[:port]` to add one.
        #[arg(value_name = "SERVER")]
        target: Option<String>,
        #[command(subcommand)]
        action: Option<RemoteAction>,
    },
```

and:

```rust
#[derive(Debug, Subcommand)]
pub enum RemoteAction {
    /// Show the servers this machine knows about.
    List,
    /// Remove a server: its key, its session, and riabuild's traces on it.
    Forget {
        #[arg(value_name = "SERVER")]
        name: String,
    },
}
```

Add a `Some(Command::Remote { .. })` arm to the `match` in `main.rs::run` that dispatches
to `remote::run(...)`, defined in Task 21. Until then, have it return `Ok(0)` so the tree
compiles.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test cli::`
Expected: PASS. `the_command_line_is_well_formed` covers the clap wiring.

- [ ] **Step 5: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src/cli.rs riabuild-cli/src/main.rs
git commit -m "Add the remote subcommand"
```

### Task 15: A key pair, and a host key you agreed to

**Files:**
- Create: `riabuild-cli/src/remote/identity.rs`
- Test: same file's tests module

**Interfaces:**
- Consumes: `Remote` (Task 13), `Ui::confirm` (Task 12).
- Produces:
  - `identity::ssh_options(remote: &Remote, paths: &dyn Paths, identities_only: bool) -> Vec<String>`
  - `identity::ensure_key(remote, paths, runner, ui) -> Result<PathBuf>` (returns the private key path)
  - `identity::fingerprint_of(keygen_stdout: &str) -> Option<String>`
  - `identity::trust_host(remote, paths, runner, ui) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RealPaths;
    use crate::runner::FakeRunner;
    use crate::ui::Ui;
    use std::sync::Arc;

    fn remote() -> Remote {
        Remote { name: "build-01".into(), host: "build-01.fly.dev".into(), port: 2222, user: "ada".into() }
    }

    #[test]
    fn ssh_options_pin_riabuilds_own_known_hosts() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let options = ssh_options(&remote(), &paths, true).join(" ");

        assert!(options.contains("-p 2222"), "{options}");
        assert!(options.contains("StrictHostKeyChecking=yes"), "{options}");
        assert!(options.contains("UserKnownHostsFile="), "{options}");
        assert!(options.contains(".riabuild/ssh/known_hosts"), "{options}");
        assert!(options.contains("IdentitiesOnly=yes"), "{options}");
        // riabuild never reads or writes the developer's own ssh config.
        assert!(!options.contains(".ssh/config"), "{options}");
    }

    #[test]
    fn the_authorising_step_does_not_pin_identities_only() {
        // The common cloud-VM case is a box that already trusts the developer's
        // existing key and has password auth disabled. That key is what
        // authorises the new one, so it must still be offered.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let options = ssh_options(&remote(), &paths, false).join(" ");
        assert!(!options.contains("IdentitiesOnly"), "{options}");
    }

    #[test]
    fn a_fingerprint_is_read_out_of_ssh_keygen_output() {
        let line = "256 SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y host (ED25519)";
        assert_eq!(
            fingerprint_of(line).as_deref(),
            Some("SHA256:qKqvBpVv3sVJ0m9j2sZq8s0Xh3P1r2s3t4u5v6w7x8Y")
        );
        assert_eq!(fingerprint_of("").as_deref(), None);
        assert_eq!(fingerprint_of("nothing useful here").as_deref(), None);
    }

    #[tokio::test]
    async fn a_key_is_generated_once_and_reused() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh-keygen", 0, "", ""));
        let ui = Ui::new(true);

        // First call generates. The fake does not write files, so simulate what
        // ssh-keygen would leave behind before the second call.
        let path = ensure_key(&remote(), &paths, fake.clone(), &ui).await.expect("generate");
        assert!(fake.calls().iter().any(|c| c.contains("ssh-keygen -t ed25519")), "{:?}", fake.calls());
        assert!(fake.calls().iter().any(|c| c.contains("-N ")), "the key must have no passphrase");

        tokio::fs::write(&path, "PRIVATE").await.expect("write");
        let again = Arc::new(FakeRunner::new().with("ssh-keygen", 0, "", ""));
        ensure_key(&remote(), &paths, again.clone(), &ui).await.expect("reuse");
        assert!(again.calls().is_empty(), "an existing key must not be regenerated");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::identity`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement**

```rust
//! The key pair for one server, and the host key riabuild agreed to trust.

use super::Remote;
use crate::paths::Paths;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

/// The `ssh` options every connection to this server uses.
///
/// `identities_only` is false for exactly one step — authorising the new key —
/// where an existing key or the agent is what proves who we are.
pub fn ssh_options(remote: &Remote, paths: &dyn Paths, identities_only: bool) -> Vec<String> {
    let mut options = vec![
        "-p".to_string(),
        remote.port.to_string(),
        "-o".to_string(),
        format!(
            "UserKnownHostsFile={}",
            paths.known_hosts_file().to_string_lossy()
        ),
        "-o".to_string(),
        "StrictHostKeyChecking=yes".to_string(),
        "-i".to_string(),
        key_path(remote, paths).to_string_lossy().into_owned(),
    ];
    if identities_only {
        options.push("-o".to_string());
        options.push("IdentitiesOnly=yes".to_string());
    }
    options
}

pub fn key_path(remote: &Remote, paths: &dyn Paths) -> PathBuf {
    paths.identity_dir().join(remote.hash())
}

/// Generates the key pair if this server does not have one yet.
pub async fn ensure_key(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
) -> Result<PathBuf> {
    let path = key_path(remote, paths);
    if tokio::fs::metadata(&path).await.is_ok() {
        return Ok(path);
    }

    tokio::fs::create_dir_all(paths.identity_dir()).await?;
    set_private_dir(&paths.identity_dir()).await?;
    ui.working("SSH key", "generating one for this server");

    let output = runner
        .run(
            "ssh-keygen",
            &[
                "-t", "ed25519",
                "-N", "",
                "-C", &format!("riabuild {}:{}", remote.target(), remote.port),
                "-f", &path.to_string_lossy(),
            ],
            &RunOptions::default(),
        )
        .await?;
    if !output.ok() {
        return Err(Failure::new(
            format!("making an SSH key for {}", remote.name),
            "Check that ssh-keygen works on this machine, then run `riabuild remote` again.",
        )
        .command("ssh-keygen -t ed25519")
        .detail(output.stderr)
        .into());
    }
    ui.applied("SSH key");
    Ok(path)
}

/// `SHA256:…` out of `ssh-keygen -lf` output.
pub fn fingerprint_of(stdout: &str) -> Option<String> {
    stdout
        .split_whitespace()
        .find(|word| word.starts_with("SHA256:"))
        .map(str::to_string)
}

/// Shows the server's host key and pins it once the developer agrees.
pub async fn trust_host(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
) -> Result<()> {
    let known_hosts = paths.known_hosts_file();
    let existing = tokio::fs::read_to_string(&known_hosts).await.unwrap_or_default();
    let entry_host = if remote.port == 22 {
        remote.host.clone()
    } else {
        format!("[{}]:{}", remote.host, remote.port)
    };
    if existing.lines().any(|line| line.starts_with(&entry_host)) {
        return Ok(());
    }

    let scan = runner
        .run(
            "ssh-keyscan",
            &["-p", &remote.port.to_string(), "-T", "5", &remote.host],
            &RunOptions::default(),
        )
        .await?;
    let keys: String = scan
        .stdout
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !scan.ok() || keys.is_empty() {
        return Err(Failure::new(
            format!("reaching {} on port {}", remote.host, remote.port),
            "Check the hostname and port, and that the server is running SSH. \
             On a Mac, turn on System Settings → General → Sharing → Remote Login.",
        )
        .command(format!("ssh-keyscan -p {} {}", remote.port, remote.host))
        .detail(scan.stderr)
        .into());
    }

    let shown = runner
        .run(
            "ssh-keygen",
            &["-lf", "-"],
            &RunOptions { stdin: Some(keys.clone().into_bytes()), ..Default::default() },
        )
        .await?;
    let fingerprint = fingerprint_of(&shown.stdout)
        .unwrap_or_else(|| "an unreadable fingerprint".to_string());

    ui.note(&format!("fingerprint {fingerprint}"));
    if !ui.confirm("is that the server you expected?")? {
        return Err(Failure::new(
            format!("trusting {}", remote.host),
            "Check the fingerprint with whoever runs that server, then run `riabuild remote` again.",
        )
        .into());
    }

    tokio::fs::create_dir_all(paths.ssh_dir()).await?;
    set_private_dir(&paths.ssh_dir()).await?;
    let mut contents = existing;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(&keys);
    contents.push('\n');
    tokio::fs::write(&known_hosts, contents).await?;
    Ok(())
}

#[cfg(unix)]
async fn set_private_dir(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_private_dir(_path: &std::path::Path) -> Result<()> {
    Ok(())
}
```

- [ ] **Step 4: Change `RunOptions.stdin` to bytes**

`trust_host` pipes a key into `ssh-keygen`, and Task 17 pipes a binary. `stdin: Option<String>`
cannot carry a binary, so change it to `Option<Vec<u8>>` in `runner.rs`, write it with
`write_all(input)`, and update the existing call sites (`keychain.rs`'s `secret-tool store`
is the one in the tree today) to `Some(token.as_bytes().to_vec())`.

Add a test in `runner.rs`:

```rust
#[tokio::test]
async fn stdin_carries_bytes_that_are_not_text() {
    // A gzip header is not valid UTF-8, and Task 17 streams a whole binary.
    let options = RunOptions { stdin: Some(vec![0x1f, 0x8b, 0x08, 0x00]), ..Default::default() };
    assert_eq!(options.stdin.as_deref(), Some(&[0x1f, 0x8b, 0x08, 0x00][..]));
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src
git commit -m "Make a key for a server, and pin the host key once you agree"
```

### Task 16: Authorising the key

**Files:**
- Modify: `riabuild-cli/src/remote/identity.rs`
- Test: same file's tests module

**Interfaces:**
- Consumes: `ssh_options`, `key_path` from Task 15.
- Produces:
  - `identity::offered_methods(stderr: &str) -> Vec<String>`
  - `identity::authorise(remote, paths, runner, ui) -> Result<()>`
  - `identity::can_sign_in(remote, paths, runner) -> Result<bool>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_methods_a_server_offers_are_read_from_its_refusal() {
    assert_eq!(
        offered_methods("ada@box: Permission denied (publickey,password)."),
        vec!["publickey".to_string(), "password".to_string()]
    );
    assert_eq!(
        offered_methods("Permission denied (publickey,keyboard-interactive)."),
        vec!["publickey".to_string(), "keyboard-interactive".to_string()]
    );
    assert_eq!(offered_methods("Permission denied (publickey)."), vec!["publickey".to_string()]);
    assert!(offered_methods("ssh: connect to host box port 22: Connection refused").is_empty());
}

#[tokio::test]
async fn a_publickey_only_server_gets_the_line_to_paste_rather_than_a_prompt() {
    // Nothing to prompt for: sshd never offers the method, so a password box
    // would be a lie. Print the key and say where it goes.
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = RealPaths::rooted_at(home.path());
    tokio::fs::create_dir_all(paths.identity_dir()).await.expect("mkdir");
    tokio::fs::write(paths.identity_dir().join(remote().hash()).with_extension("pub"), "ssh-ed25519 AAAA riabuild")
        .await
        .expect("write pub");

    let fake = Arc::new(
        FakeRunner::new()
            .with("ssh -o PreferredAuthentications=none", 255, "", "Permission denied (publickey).")
            .with("ssh -o BatchMode=yes", 255, "", "Permission denied (publickey)."),
    );
    let error = authorise(&remote(), &paths, fake.clone(), &Ui::new(true))
        .await
        .expect_err("must not claim success");

    let text = format!("{error}");
    assert!(text.contains("authorized_keys"), "{text}");
    assert!(
        !fake.calls().iter().any(|call| call.starts_with("ssh-copy-id")),
        "ssh-copy-id cannot help here and must not be run"
    );
}

#[tokio::test]
async fn a_key_that_already_works_is_not_copied_again() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = RealPaths::rooted_at(home.path());
    let fake = Arc::new(FakeRunner::new().with("ssh -o BatchMode=yes", 0, "", ""));

    authorise(&remote(), &paths, fake.clone(), &Ui::new(true)).await.expect("already fine");
    assert!(!fake.calls().iter().any(|call| call.starts_with("ssh-copy-id")));
}

#[tokio::test]
async fn ssh_copy_id_runs_when_the_server_will_take_a_password() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = RealPaths::rooted_at(home.path());
    let fake = Arc::new(
        FakeRunner::new()
            .with("ssh -o BatchMode=yes", 255, "", "Permission denied (publickey,password).")
            .with("ssh -o PreferredAuthentications=none", 255, "", "Permission denied (publickey,password).")
            .with("ssh-copy-id", 0, "", ""),
    );

    // The second BatchMode probe, after copying, has to succeed for the step to
    // pass. The fake returns the same stub for both, so this asserts the copy ran
    // and that a still-failing sign-in is reported.
    let result = authorise(&remote(), &paths, fake.clone(), &Ui::new(true)).await;
    assert!(fake.calls().iter().any(|call| call.starts_with("ssh-copy-id")), "{:?}", fake.calls());
    assert!(result.is_err(), "a key that still cannot sign in is not success");
}

#[tokio::test]
async fn a_missing_ssh_copy_id_is_a_next_action_not_a_crash() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = RealPaths::rooted_at(home.path());
    // FakeRunner::which only knows programs that have been stubbed.
    let fake = Arc::new(
        FakeRunner::new()
            .with("ssh -o BatchMode=yes", 255, "", "Permission denied (publickey,password).")
            .with("ssh -o PreferredAuthentications=none", 255, "", "Permission denied (publickey,password)."),
    );
    let error = authorise(&remote(), &paths, fake, &Ui::new(true)).await.expect_err("no ssh-copy-id");
    assert!(format!("{error}").contains("authorized_keys"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::identity`
Expected: FAIL — `offered_methods`, `authorise`, `can_sign_in` do not exist.

- [ ] **Step 3: Implement**

```rust
/// The authentication methods sshd named in its refusal.
pub fn offered_methods(stderr: &str) -> Vec<String> {
    let Some(start) = stderr.find("Permission denied (") else {
        return Vec::new();
    };
    let rest = &stderr[start + "Permission denied (".len()..];
    let Some(end) = rest.find(')') else {
        return Vec::new();
    };
    rest[..end]
        .split(',')
        .map(|method| method.trim().to_string())
        .filter(|method| !method.is_empty())
        .collect()
}

/// Can riabuild's own key sign in, without a password and without the agent?
pub async fn can_sign_in(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
) -> Result<bool> {
    let mut args = vec!["-o".to_string(), "BatchMode=yes".to_string()];
    args.extend(ssh_options(remote, paths, true));
    args.push(remote.target());
    args.push("true".to_string());

    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(runner.run("ssh", &refs, &RunOptions::default()).await?.ok())
}

/// Installs riabuild's public key on the server, if it is not there already.
pub async fn authorise(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
) -> Result<()> {
    if can_sign_in(remote, paths, runner.clone()).await? {
        return Ok(());
    }

    let public_key_path = key_path(remote, paths).with_extension("pub");
    let public_key = tokio::fs::read_to_string(&public_key_path)
        .await
        .unwrap_or_default();
    let paste = || {
        Failure::new(
            format!("authorising riabuild's key on {}", remote.host),
            format!(
                "Add this line to ~/.ssh/authorized_keys on {}, then run `riabuild remote` again:\n    {}",
                remote.host,
                public_key.trim()
            ),
        )
    };

    // What will the server actually accept?
    let mut probe = vec![
        "-o".to_string(),
        "PreferredAuthentications=none".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
    ];
    probe.extend(ssh_options(remote, paths, false));
    probe.push(remote.target());
    probe.push("true".to_string());
    let probe_refs: Vec<&str> = probe.iter().map(String::as_str).collect();
    let refusal = runner.run("ssh", &probe_refs, &RunOptions::default()).await?;
    let methods = offered_methods(&refusal.stderr);

    let interactive = methods
        .iter()
        .any(|method| method == "password" || method == "keyboard-interactive");
    if !interactive {
        return Err(paste()
            .detail("that server accepts keys only, so there is no password to ask you for")
            .into());
    }
    if runner.which("ssh-copy-id").is_none() {
        return Err(paste()
            .detail("ssh-copy-id is not installed on this machine")
            .into());
    }

    ui.working("Authorised", "installing the key");
    let mut args = vec![
        "-i".to_string(),
        public_key_path.to_string_lossy().into_owned(),
    ];
    // Deliberately without IdentitiesOnly: an existing key or the agent may be
    // what proves who we are on a server with passwords disabled for everyone
    // but us.
    args.extend(ssh_options(remote, paths, false));
    args.push(remote.target());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = runner
        .run_interactive("ssh-copy-id", &refs, &RunOptions::default())
        .await?;
    if code != 0 {
        return Err(paste().command("ssh-copy-id").into());
    }

    if !can_sign_in(remote, paths, runner).await? {
        return Err(paste()
            .detail("the key was copied, but signing in with it still does not work")
            .into());
    }
    ui.applied("Authorised");
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test remote::identity`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src/remote/identity.rs
git commit -m "Authorise riabuild's key, and say so plainly when we cannot"
```

### Task 17: Putting riabuild on the server

**Files:**
- Create: `riabuild-cli/src/remote/install.rs`
- Modify: `riabuild-cli/src/download.rs` (single-member extraction)
- Test: both files' tests modules

**Interfaces:**
- Consumes: `download::rust_target`, `riabuild_asset_url`, `riabuild_checksums_url`, `digest_for`, `fetch_bytes`, `sha256_hex` (Task 11); `ssh_options` (Task 15).
- Produces:
  - `download::extract_single_file(bytes: &[u8], name: &str) -> Result<Vec<u8>>`
  - `install::remote_binary_path(version: &str) -> String` — `~/.riabuild/riabuild/<version>/riabuild`
  - `install::ensure_riabuild(remote, paths, runner, ui, version) -> Result<String>` — returns the remote path

- [ ] **Step 1: Write the failing tests**

In `download.rs`:

```rust
#[test]
fn a_single_member_is_lifted_out_of_a_tarball() {
    // Built in memory, so the test needs no fixture file and no network.
    let mut archive = tar::Builder::new(Vec::new());
    let payload = b"\x7fELF fake binary";
    let mut header = tar::Header::new_gnu();
    header.set_size(payload.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive.append_data(&mut header, "riabuild", &payload[..]).expect("append");
    let tar_bytes = archive.into_inner().expect("finish");

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut encoder, &tar_bytes).expect("gzip");
    let gz = encoder.finish().expect("gzip");

    assert_eq!(extract_single_file(&gz, "riabuild").expect("extract"), payload);
    assert!(extract_single_file(&gz, "not-there").is_err());
}
```

In `remote/install.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_path_is_versioned_and_shared() {
        // Shared, so five developers on one account get one toolchain; versioned,
        // so two developers on two riabuild versions do not fight over a file.
        assert_eq!(
            remote_binary_path("2026.08.06"),
            "~/.riabuild/riabuild/2026.08.06/riabuild"
        );
    }

    #[tokio::test]
    async fn a_server_already_running_the_right_version_is_left_alone() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(
            FakeRunner::new()
                .with("ssh", 0, "2026.08.06\n", "")
        );

        let path = ensure_riabuild(&remote(), &paths, fake.clone(), &Ui::new(true), "2026.08.06")
            .await
            .expect("already installed");

        assert_eq!(path, remote_binary_path("2026.08.06"));
        assert!(
            !fake.calls().iter().any(|call| call.contains("mkdir")),
            "nothing should be installed: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn an_unpublished_architecture_stops_before_anything_is_written() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        // No version on the box, and a 32-bit uname.
        let fake = Arc::new(
            FakeRunner::new()
                .with("ssh", 1, "", "No such file")
                .with("ssh -p 2222 -o UserKnownHostsFile", 0, "Linux i686\n", ""),
        );
        let error = ensure_riabuild(&remote(), &paths, fake, &Ui::new(true), "2026.08.06")
            .await
            .expect_err("unsupported");
        assert!(format!("{error}").contains("i686"), "{error}");
    }
}
```

Define the `remote()` helper in this module the same way Task 15 does.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::install` and `cargo test download::`
Expected: FAIL — neither function exists.

- [ ] **Step 3: Add `extract_single_file`**

In `download.rs`, beside the existing extractors:

```rust
/// One named member of a gzipped tarball, in memory.
///
/// The release tarball holds `riabuild` at its root. The bytes are wanted rather
/// than a path, because they go straight down an SSH pipe to a server.
pub fn extract_single_file(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let matches = path.file_name().is_some_and(|found| found == name);
        if matches {
            let mut buffer = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buffer)?;
            return Ok(buffer);
        }
    }
    anyhow::bail!("{name} is not in that archive")
}
```

- [ ] **Step 4: Write `remote/install.rs`**

```rust
//! Getting the right riabuild onto a server.
//!
//! Downloaded and verified on the laptop, then streamed over SSH. That keeps
//! digest verification in the one place that already does it properly, and needs
//! nothing installed on the server but a shell.

use super::{Remote, identity};
use crate::download;
use crate::paths::Paths;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::sync::Arc;

pub fn remote_binary_path(version: &str) -> String {
    format!("~/.riabuild/riabuild/{version}/riabuild")
}

/// Runs one command on the server through the key riabuild owns.
async fn ssh(
    remote: &Remote,
    paths: &dyn Paths,
    runner: &Arc<dyn CommandRunner>,
    command: &str,
) -> Result<crate::runner::CommandOutput> {
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(command.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner.run("ssh", &refs, &RunOptions::default()).await
}

pub async fn ensure_riabuild(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    version: &str,
) -> Result<String> {
    let path = remote_binary_path(version);

    let installed = ssh(remote, paths, &runner, &format!("{path} --version")).await?;
    if installed.ok() && installed.trimmed().contains(version) {
        return Ok(path);
    }

    ui.working("riabuild", &format!("installing {version} on the server"));

    let platform = ssh(remote, paths, &runner, "uname -sm").await?;
    if !platform.ok() {
        return Err(Failure::new(
            format!("asking {} what it is", remote.host),
            "Check that you can `ssh` to that server yourself, then run `riabuild remote` again.",
        )
        .command("uname -sm")
        .detail(platform.stderr)
        .into());
    }
    let mut parts = platform.trimmed().split_whitespace();
    let (system, machine) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let target = download::rust_target(system, machine).map_err(|error| {
        Failure::new(
            format!("installing riabuild on {}", remote.host),
            "Use a server riabuild publishes a build for: Linux or macOS, on x86_64 or arm64.",
        )
        .detail(error.to_string())
    })?;

    // Verified on the laptop, before a byte reaches the server.
    let checksums = download::fetch_text(&download::riabuild_checksums_url(version)).await?;
    let asset = download::riabuild_asset(version, &target);
    let expected = download::digest_for(&checksums, &asset).ok_or_else(|| {
        Failure::new(
            format!("verifying the riabuild {version} download"),
            "Tell your team lead — the release is missing a checksum for this platform.",
        )
    })?;
    let tarball = download::fetch_bytes(&download::riabuild_asset_url(version, &target)).await?;
    if download::sha256_hex(&tarball) != expected {
        return Err(Failure::new(
            format!("verifying the riabuild {version} download"),
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail("the download did not match its published digest")
        .into());
    }
    let binary = download::extract_single_file(&tarball, "riabuild")?;

    // Written to a temporary name and moved into place, so a concurrent reader
    // sees a complete binary or none. Two developers installing at once is an
    // ordinary situation on a shared box.
    let dir = format!("~/.riabuild/riabuild/{version}");
    let install = format!(
        "umask 077 && mkdir -p {dir} && cat > {dir}/.riabuild.part && \
         chmod 755 {dir}/.riabuild.part && mv {dir}/.riabuild.part {dir}/riabuild"
    );
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(install);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let written = runner
        .run("ssh", &refs, &RunOptions { stdin: Some(binary), ..Default::default() })
        .await?;
    if !written.ok() {
        return Err(Failure::new(
            format!("installing riabuild on {}", remote.host),
            "Check there is space in your home directory on that server, then run `riabuild remote` again.",
        )
        .detail(written.stderr)
        .into());
    }

    let confirmed = ssh(remote, paths, &runner, &format!("{path} --version")).await?;
    if !confirmed.ok() || !confirmed.trimmed().contains(version) {
        return Err(Failure::new(
            format!("installing riabuild on {}", remote.host),
            "Run `riabuild remote` again. If it keeps failing, tell your team lead.",
        )
        .detail(format!(
            "the server reports {:?} after installing {version}",
            confirmed.trimmed()
        ))
        .into());
    }
    ui.applied("riabuild");
    Ok(path)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src
git commit -m "Install a verified riabuild on the server, over the SSH pipe"
```

### Task 18: The server's own riabuild session

**Files:**
- Create: `riabuild-cli/src/remote/session.rs`
- Modify: `riabuild-cli/src/keychain.rs` (accounts)
- Modify: `riabuild-cli/src/api/auth.rs` (a caller-chosen device label)
- Modify: `riabuild-cli/src/tasks/login.rs` (pass the laptop's own label)
- Test: `riabuild-cli/src/remote/session.rs`, `riabuild-cli/src/keychain.rs`

**Interfaces:**
- Consumes: `Remote` (Task 13), `ssh_options` (Task 15), `Member::public_id` (Task 3).
- Produces:
  - `keychain::for_account(runner, account: &str, session_token_file: Option<PathBuf>) -> Box<dyn Keychain>`
  - `keychain::remote_account(hash: &str) -> String` — `remote:<hash>`
  - `auth::login(api, runner, ui, web_url, version, label: &str)` — label now a parameter
  - `session::namespace(public_id: &str) -> String` — `~/.riabuild-remote/<public-id>`
  - `session::ensure(remote, paths, runner, ui, api, member, web_url, version) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

In `keychain.rs`:

```rust
#[test]
fn a_servers_session_is_stored_under_its_own_account() {
    // One laptop, several servers, one keychain: the account is what keeps them
    // apart, and revoking one must not sign the laptop out.
    assert_eq!(remote_account("9f2c000000000000"), "remote:9f2c000000000000");
    assert_ne!(remote_account("aaaa"), remote_account("bbbb"));
}

#[tokio::test]
async fn a_remote_account_reads_and_writes_its_own_item() {
    let runner = Arc::new(
        FakeRunner::new()
            .with("security find-generic-password", 0, "rb_remote_token\n", "")
            .with("security add-generic-password", 0, "", ""),
    );
    let keychain = SecurityCliKeychain::for_account(runner.clone(), "remote:9f2c");
    keychain.set("rb_remote_token").await.expect("write");

    assert!(
        runner.calls().iter().any(|call| call.contains("remote:9f2c")),
        "{:?}",
        runner.calls()
    );
}
```

In `remote/session.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_namespace_is_named_after_the_immutable_id() {
        // Not the login: a GitHub rename would otherwise orphan a developer's
        // whole environment and silently re-provision them from scratch.
        assert_eq!(
            namespace("550e8400-e29b-41d4-a716-446655440000"),
            "~/.riabuild-remote/550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn the_owner_file_says_who_this_is_in_words() {
        let json = owner_json("ada", "Ada Lovelace", "ada@clubria.dev");
        assert!(json.contains("\"githubLogin\": \"ada\""), "{json}");
        assert!(json.contains("Ada Lovelace"), "{json}");
        // No secret ever goes in here: it is a label, readable by everyone who
        // shares the account.
        assert!(!json.contains("token"), "{json}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::session` and `cargo test keychain::`
Expected: FAIL — `for_account`, `remote_account`, `namespace`, `owner_json` do not exist.

- [ ] **Step 3: Give the keychains accounts**

In `keychain.rs`, replace the `ACCOUNT` constant use with a field:

```rust
pub struct SecurityCliKeychain {
    runner: Arc<dyn CommandRunner>,
    account: String,
}

impl SecurityCliKeychain {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self::for_account(runner, ACCOUNT)
    }

    pub fn for_account(runner: Arc<dyn CommandRunner>, account: &str) -> Self {
        Self { runner, account: account.to_string() }
    }
}
```

Use `&self.account` wherever `ACCOUNT` was passed to `security`. Do the same for
`SecretToolKeychain`. Then:

```rust
/// The keychain account a server's session is stored under, on the laptop.
pub fn remote_account(hash: &str) -> String {
    format!("remote:{hash}")
}

/// Like `for_platform`, but for a named account rather than this machine's own.
pub fn for_account(
    runner: Arc<dyn CommandRunner>,
    account: &str,
    session_token_file: Option<PathBuf>,
) -> Box<dyn Keychain> {
    if let Some(path) = session_token_file {
        return Box::new(FileKeychain::new(path));
    }
    if cfg!(target_os = "macos") {
        return Box::new(SecurityCliKeychain::for_account(runner, account));
    }
    Box::new(SecretToolKeychain::for_account(runner, account))
}
```

`RIABUILD_TOKEN` is deliberately *not* consulted here: it is this machine's override, and
using it for a server's session would give every server the same token.

- [ ] **Step 4: Let the caller choose the device label**

In `api/auth.rs`, change `login` to take `label: &str` and delete the internal
`device_label(runner)` call; move that helper's use into `tasks/login.rs`, which passes
`device_label(ctx.runner.as_ref()).await`. Remote mode passes the server's hostname, so the
dashboard lists the session as that server.

- [ ] **Step 5: Write `remote/session.rs`**

```rust
//! The riabuild session a server runs on.
//!
//! Minted by the laptop, labelled after the server so the dashboard lists it as
//! its own revocable device, and written to the server's namespace at 0600 —
//! the one amendment to "no secrets in ~/.riabuild", argued in the design.

use super::{Remote, identity};
use crate::api::{ApiClient, Member, auth};
use crate::keychain;
use crate::paths::Paths;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::sync::Arc;

pub fn namespace(public_id: &str) -> String {
    format!("~/.riabuild-remote/{public_id}")
}

/// Who a namespace belongs to, for whoever has a shell on the box and finds a
/// directory named after a UUID.
pub fn owner_json(login: &str, name: &str, email: &str) -> String {
    format!(
        "{{\n  \"githubLogin\": \"{login}\",\n  \"name\": \"{name}\",\n  \"email\": \"{email}\"\n}}\n"
    )
}

pub async fn ensure(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    api: &ApiClient,
    member: &Member,
    web_url: &str,
    version: &str,
) -> Result<()> {
    let store = keychain::for_account(
        runner.clone(),
        &keychain::remote_account(&remote.hash()),
        None,
    );

    let token = match store.get().await? {
        Some(token) => token,
        None => {
            ui.note(&format!(
                "Signing {} in to riabuild — approve it in your browser",
                remote.name
            ));
            // Laptop's browser, server's name: the dashboard lists this as its
            // own device, revocable without signing the laptop out.
            let (token, _) = auth::login(api, runner.as_ref(), ui, web_url, version, &remote.host).await?;
            store.set(&token).await?;
            token
        }
    };

    let ns = namespace(&member.public_id);
    let write = format!(
        "umask 077 && mkdir -p {ns} && cat > {ns}/session.token && chmod 600 {ns}/session.token"
    );
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(write);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let written = runner
        .run(
            "ssh",
            &refs,
            &RunOptions { stdin: Some(token.into_bytes()), ..Default::default() },
        )
        .await?;
    if !written.ok() {
        return Err(Failure::new(
            format!("giving {} its riabuild session", remote.name),
            "Check there is space in your home directory on that server, then run `riabuild remote` again.",
        )
        .detail(written.stderr)
        .into());
    }

    let owner = owner_json(&member.github_login, &member.display_name(), &member.email);
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(format!("cat > {ns}/owner.json"));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner
        .run(
            "ssh",
            &refs,
            &RunOptions { stdin: Some(owner.into_bytes()), ..Default::default() },
        )
        .await?;
    Ok(())
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src
git commit -m "Mint a session for the server and write it where the server can read it"
```

### Task 19: The GitHub configuration directory, and wiping it

**Files:**
- Create: `riabuild-cli/src/gh_session.rs`
- Modify: `riabuild-cli/src/main.rs`
- Test: `riabuild-cli/src/gh_session.rs` tests module

**Interfaces:**
- Consumes: `Scope` (Task 10), `ScopedRunner` (Task 8).
- Produces:
  - `gh_session::choose_runtime_dir(xdg: Option<&str>, tmpdir: Option<&str>) -> PathBuf`
  - `GhSession::open(runtime: &Path, public_id: &str, pid: u32) -> Result<GhSession>`
  - `GhSession::config_dir(&self) -> PathBuf`
  - `GhSession::close(self, runner: Arc<dyn CommandRunner>) -> Result<()>`
  - `gh_session::sweep(dir: &Path, runner: Arc<dyn CommandRunner>, now: u64) -> Result<bool>` — true if the tree was wiped

> **Deviation from the spec, to be folded into it in this task.** The spec also asks for
> `SIGTERM`/`SIGHUP`/`SIGINT` handlers that wipe. This plan implements the sweep and the
> normal-exit path only, and drops the signal handlers, because the shell is riabuild's
> **child**: when mosh or ssh goes away the shell exits and `run_interactive` returns, so
> the ordinary path already covers it. What remains is riabuild itself being killed, and
> `SIGKILL` cannot be caught anyway — so a signal handler would be a second mechanism
> covering a strict subset of what the sweep covers. Update the spec's bullet list.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use std::sync::Arc;

    #[test]
    fn a_tmpfs_runtime_directory_is_preferred_to_tmp() {
        // On a systemd host XDG_RUNTIME_DIR is a per-uid tmpfs, so the token
        // never touches a disk at all. TMPDIR is what macOS provides. /tmp is
        // the floor that always exists.
        assert_eq!(
            choose_runtime_dir(Some("/run/user/1000"), Some("/var/folders/x")),
            PathBuf::from("/run/user/1000")
        );
        assert_eq!(
            choose_runtime_dir(None, Some("/var/folders/x")),
            PathBuf::from("/var/folders/x")
        );
        assert_eq!(choose_runtime_dir(None, None), PathBuf::from("/tmp"));
        assert_eq!(choose_runtime_dir(Some(""), None), PathBuf::from("/tmp"));
    }

    #[tokio::test]
    async fn opening_a_session_makes_a_private_directory_and_a_marker() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let session = GhSession::open(home.path(), "550e8400", 4242).await.expect("open");

        assert!(session.config_dir().is_dir());
        assert!(session.config_dir().join("sessions").join("4242").is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(session.config_dir())
                .await
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "/tmp is world-writable and sticky");
        }
    }

    #[tokio::test]
    async fn two_sessions_share_one_sign_in_and_the_last_one_out_wipes_it() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let first = GhSession::open(home.path(), "550e8400", 1).await.expect("open");
        let second = GhSession::open(home.path(), "550e8400", 2).await.expect("open");
        let dir = first.config_dir();
        tokio::fs::write(dir.join("hosts.yml"), "github.com:\n").await.expect("write");

        // Both pids are alive, so nothing is removed yet.
        let alive = Arc::new(FakeRunner::new().with("kill -0", 0, "", ""));
        first.close(alive.clone()).await.expect("close");
        assert!(dir.join("hosts.yml").is_file(), "one session left, keep the sign-in");

        second.close(alive).await.expect("close");
        assert!(!dir.exists(), "the last one out wipes the tree");
    }

    #[tokio::test]
    async fn a_marker_for_a_dead_process_is_swept_and_the_tree_goes_with_it() {
        // The case that actually matters: a mosh session that died with the
        // laptop's battery never ran any exit path at all.
        let home = tempfile::TempDir::new().expect("tempdir");
        let session = GhSession::open(home.path(), "550e8400", 9999).await.expect("open");
        let dir = session.config_dir();
        tokio::fs::write(dir.join("hosts.yml"), "github.com:\n").await.expect("write");
        std::mem::forget(session);

        let dead = Arc::new(FakeRunner::new().with("kill -0", 1, "", "No such process"));
        assert!(sweep(&dir, dead, 0).await.expect("sweep"));
        assert!(!dir.exists(), "a credential must not outlive the session that made it");
    }

    #[tokio::test]
    async fn a_recycled_pid_cannot_keep_a_stale_tree_alive_forever() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let session = GhSession::open(home.path(), "550e8400", 1).await.expect("open");
        let dir = session.config_dir();
        std::mem::forget(session);

        // The pid looks alive, but the marker is older than a day.
        let alive = Arc::new(FakeRunner::new().with("kill -0", 0, "", ""));
        let a_week_later = 8 * 24 * 60 * 60;
        assert!(sweep(&dir, alive, a_week_later).await.expect("sweep"));
        assert!(!dir.exists());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test gh_session::`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write the module**

```rust
//! The server's GitHub configuration directory, which lives only as long as a
//! session.
//!
//! A `gh` OAuth token is the developer's whole GitHub account, and a shared box
//! is the last place it should sit at rest — so it is the one piece of state that
//! is not namespaced onto disk. This buys "no GitHub credential at rest between
//! sessions". It does **not** hide the credential from a co-tenant during a live
//! session, and deleting is not revoking; both are stated in the design.

use crate::runner::{CommandRunner, RunOptions};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A marker whose process still looks alive is ignored after this long, because
/// pids are recycled and a stale marker would otherwise match a live stranger.
const STALE_AFTER_SECS: u64 = 24 * 60 * 60;

pub fn choose_runtime_dir(xdg: Option<&str>, tmpdir: Option<&str>) -> PathBuf {
    for candidate in [xdg, tmpdir] {
        if let Some(path) = candidate.filter(|value| !value.is_empty()) {
            return PathBuf::from(path);
        }
    }
    PathBuf::from("/tmp")
}

pub struct GhSession {
    dir: PathBuf,
    marker: PathBuf,
}

impl GhSession {
    pub async fn open(runtime: &Path, public_id: &str, pid: u32) -> Result<GhSession> {
        let dir = runtime.join(format!("riabuild-gh-{public_id}"));
        let sessions = dir.join("sessions");
        tokio::fs::create_dir_all(&sessions).await?;
        // /tmp is world-writable and sticky, so the directory has to be private
        // the moment it exists.
        set_private(&dir).await?;

        let marker = sessions.join(pid.to_string());
        tokio::fs::write(&marker, crate::config::now_secs().to_string()).await?;
        Ok(GhSession { dir, marker })
    }

    pub fn config_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Drops this session's claim, and wipes the tree if it was the last.
    pub async fn close(self, runner: Arc<dyn CommandRunner>) -> Result<()> {
        let _ = tokio::fs::remove_file(&self.marker).await;
        sweep(&self.dir, runner, crate::config::now_secs()).await?;
        Ok(())
    }
}

/// Removes markers whose process is gone, and wipes a tree nobody is using.
///
/// This is the backstop that matters, because it is the one that does not depend
/// on a dying process getting a chance to run code.
pub async fn sweep(dir: &Path, runner: Arc<dyn CommandRunner>, now: u64) -> Result<bool> {
    let sessions = dir.join("sessions");
    let mut live = 0;

    if let Ok(mut entries) = tokio::fs::read_dir(&sessions).await {
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let Some(pid) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let written: u64 = tokio::fs::read_to_string(&path)
                .await
                .ok()
                .and_then(|text| text.trim().parse().ok())
                .unwrap_or(0);

            let expired = now.saturating_sub(written) > STALE_AFTER_SECS;
            let running = !expired
                && runner
                    .run("kill", &["-0", pid], &RunOptions::default())
                    .await
                    .map(|output| output.ok())
                    .unwrap_or(false);

            if running {
                live += 1;
            } else {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }

    if live == 0 {
        let _ = tokio::fs::remove_dir_all(dir).await;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(unix)]
async fn set_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_private(_path: &Path) -> Result<()> {
    Ok(())
}
```

- [ ] **Step 4: Wire it into `main.rs`**

When `scope.is_remote()`, before building `Ctx`:

```rust
    let gh = if scope.is_remote() {
        let runtime = gh_session::choose_runtime_dir(
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            std::env::var("TMPDIR").ok().as_deref(),
        );
        let public_id = paths
            .root()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(gh_session::GhSession::open(&runtime, &public_id, std::process::id()).await?)
    } else {
        None
    };

    let runner: Arc<dyn CommandRunner> = match &gh {
        Some(session) => Arc::new(runner::ScopedRunner::new(
            runner,
            vec![
                ("GH_CONFIG_DIR".into(), session.config_dir().to_string_lossy().into_owned()),
                ("GIT_CONFIG_GLOBAL".into(), paths.root().join("gitconfig").to_string_lossy().into_owned()),
            ],
        )),
        None => runner,
    };
```

At every `return` from `run`, close the session. Restructure `run` so the body is an inner
function and `run` closes `gh` around it:

```rust
    let code = run_inner(&cli, &scope, ctx).await;
    if let Some(session) = gh {
        let _ = session.close(base_runner.clone()).await;
    }
    code
```

`base_runner` is the unwrapped `RealRunner` — `kill -0` needs no namespace environment, and
closing must work even if the scoped runner is gone.

- [ ] **Step 5: Update the spec's cleanup bullets**

In the design's *Refcounting, and cleaning up after a crash* section, replace the
`SIGTERM`/`SIGHUP`/`SIGINT` bullet with the reasoning in the callout above.

- [ ] **Step 6: Run the tests and commit**

Run: `cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS.

```bash
git add riabuild-cli/src docs/superpowers/specs
git commit -m "Keep the server's GitHub configuration only as long as a session"
```

### Task 20: Seeding the GitHub sign-in

**Files:**
- Modify: `riabuild-cli/src/cli.rs` (a hidden subcommand)
- Modify: `riabuild-cli/src/main.rs`
- Modify: `riabuild-cli/src/remote/session.rs`
- Test: `riabuild-cli/src/remote/session.rs` tests module

**Interfaces:**
- Consumes: `GhSession` (Task 19), `ssh_options` (Task 15).
- Produces:
  - `Command::Internal { action: InternalAction }` with `InternalAction::SeedGithub`
  - `session::seed_github(remote, paths, runner, ui, riabuild_path) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn the_token_is_piped_and_never_put_in_an_argument_list() {
    // Arguments are world-readable through `ps`. This is the same assertion
    // keychain.rs already makes about `secret-tool`.
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = crate::paths::RealPaths::rooted_at(home.path());
    let fake = Arc::new(
        FakeRunner::new()
            .with("gh auth token", 0, "gho_super_secret\n", "")
            .with("ssh", 0, "", ""),
    );

    seed_github(&remote(), &paths, fake.clone(), &Ui::new(true), "~/.riabuild/riabuild/v/riabuild")
        .await
        .expect("seeds");

    assert!(
        !fake.calls().iter().any(|call| call.contains("gho_super_secret")),
        "{:?}",
        fake.calls()
    );
    assert!(
        fake.calls().iter().any(|call| call.contains("internal seed-github")),
        "{:?}",
        fake.calls()
    );
}

#[tokio::test]
async fn a_laptop_with_no_gh_sign_in_does_not_stop_the_run() {
    // The server's own device-code sign-in is the fallback, and it costs no new
    // code: github_cli's check finds gh signed out and its apply signs in over
    // the TTY that setup already has.
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = crate::paths::RealPaths::rooted_at(home.path());
    let fake = Arc::new(FakeRunner::new().with("gh auth token", 1, "", "not logged in"));

    seed_github(&remote(), &paths, fake.clone(), &Ui::new(true), "riabuild")
        .await
        .expect("must not fail the run");
    assert!(!fake.calls().iter().any(|call| call.starts_with("ssh")));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::session`
Expected: FAIL — `seed_github` does not exist.

- [ ] **Step 3: Add the hidden subcommand**

In `cli.rs`:

```rust
    /// Internal plumbing, invoked by riabuild over SSH. Not for people.
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        action: InternalAction,
    },
```

```rust
#[derive(Debug, Subcommand)]
pub enum InternalAction {
    /// Read a GitHub token on stdin and hand it to `gh`.
    SeedGithub,
}
```

In `main.rs`, dispatch it:

```rust
        Some(Command::Internal { action: cli::InternalAction::SeedGithub }) => {
            let mut token = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut token)?;
            let output = ctx
                .runner
                .run(
                    "gh",
                    &["auth", "login", "--with-token"],
                    &RunOptions { stdin: Some(token.trim().as_bytes().to_vec()), ..Default::default() },
                )
                .await?;
            return Ok(if output.ok() { 0 } else { 1 });
        }
```

`gh` writes its own `hosts.yml`, with its own permissions, into the `GH_CONFIG_DIR` the
scoped runner supplies. riabuild never hand-writes that file.

- [ ] **Step 4: Write the laptop half**

In `remote/session.rs`:

```rust
/// Hands the laptop's GitHub sign-in to the server for this session.
///
/// Never fatal. If the laptop has no usable token, `github_cli` on the server
/// signs in for itself over the TTY that setup already has — the fallback the
/// task always had.
pub async fn seed_github(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    riabuild_path: &str,
) -> Result<()> {
    let token = runner
        .run("gh", &["auth", "token"], &RunOptions::default())
        .await?;
    if !token.ok() || token.trimmed().is_empty() {
        ui.note("This laptop has no GitHub sign-in to lend; the server will sign in itself.");
        return Ok(());
    }

    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(format!("{riabuild_path} internal seed-github"));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let seeded = runner
        .run(
            "ssh",
            &refs,
            &RunOptions {
                // On stdin, never in argv: `ps` is readable by everyone.
                stdin: Some(token.trimmed().as_bytes().to_vec()),
                ..Default::default()
            },
        )
        .await?;
    if !seeded.ok() {
        ui.note("Could not lend the server your GitHub sign-in; it will sign in itself.");
    }
    Ok(())
}
```

The remote invocation needs `RIABUILD_ROOT` and `RIABUILD_REMOTE` in front of
`{riabuild_path}`, exactly as the setup run in Task 21 does; build that prefix once in
Task 21 and pass it in rather than spelling it twice.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src
git commit -m "Lend the server this laptop's GitHub sign-in, over stdin"
```

### Task 21: Setup, the shell, and the flow that ties it together

**Files:**
- Create: `riabuild-cli/src/remote/shell.rs`
- Modify: `riabuild-cli/src/remote/mod.rs` (the flow)
- Modify: `riabuild-cli/src/main.rs` (dispatch)
- Test: `riabuild-cli/src/remote/shell.rs`, `riabuild-cli/src/remote/mod.rs`

**Interfaces:**
- Consumes: everything from Tasks 12–20.
- Produces:
  - `remote::env_prefix(public_id: &str, name: &str) -> String`
  - `shell::open(remote, paths, runner, ui, command) -> Result<i32>`
  - `remote::run(ctx: &mut Ctx, cli: &Cli, target: Option<String>, action: Option<RemoteAction>) -> Result<i32>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_remote_invocation_carries_the_namespace_and_the_server_name() {
    let prefix = env_prefix("550e8400", "build-01");
    assert!(prefix.contains("RIABUILD_ROOT=~/.riabuild-remote/550e8400"), "{prefix}");
    assert!(prefix.contains("RIABUILD_REMOTE=build-01"), "{prefix}");
}

#[tokio::test]
async fn mosh_is_used_when_the_server_has_it() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = crate::paths::RealPaths::rooted_at(home.path());
    let fake = Arc::new(
        FakeRunner::new()
            .with("ssh", 0, "/usr/bin/mosh-server\n", "")
            .with("mosh", 0, "", ""),
    );
    shell::open(&remote(), &paths, fake.clone(), &Ui::new(true), "riabuild shell")
        .await
        .expect("opens");

    assert!(fake.calls().iter().any(|call| call.starts_with("mosh ")), "{:?}", fake.calls());
}

#[tokio::test]
async fn a_server_without_mosh_falls_back_to_ssh_rather_than_stopping() {
    // A blocked UDP port is a cloud-firewall default, not a developer error.
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = crate::paths::RealPaths::rooted_at(home.path());
    let fake = Arc::new(FakeRunner::new().with("ssh", 1, "", "command not found"));

    shell::open(&remote(), &paths, fake.clone(), &Ui::new(true), "riabuild shell")
        .await
        .expect("falls back");

    assert!(!fake.calls().iter().any(|call| call.starts_with("mosh ")));
    assert!(
        fake.calls().iter().any(|call| call.contains("-t") && call.contains("riabuild shell")),
        "{:?}",
        fake.calls()
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::shell`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write `remote/shell.rs`**

```rust
//! Opening the environment on a server: mosh when it can, ssh when it cannot.

use super::{Remote, identity};
use crate::paths::Paths;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::Ui;
use anyhow::Result;
use std::sync::Arc;

async fn has_mosh_server(
    remote: &Remote,
    paths: &dyn Paths,
    runner: &Arc<dyn CommandRunner>,
) -> bool {
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push("command -v mosh-server".to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner
        .run("ssh", &refs, &RunOptions::default())
        .await
        .map(|output| output.ok())
        .unwrap_or(false)
}

pub async fn open(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    command: &str,
) -> Result<i32> {
    let local_mosh = runner.which("mosh").is_some();
    if local_mosh && has_mosh_server(remote, paths, &runner).await {
        let ssh = format!("ssh {}", identity::ssh_options(remote, paths, true).join(" "));
        let args = vec![
            format!("--ssh={ssh}"),
            remote.target(),
            "--".to_string(),
            command.to_string(),
        ];
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let code = runner.run_interactive("mosh", &refs, &RunOptions::default()).await?;
        // A mosh that cannot get its UDP through exits without a session. Falling
        // back beats leaving a developer stranded behind a cloud firewall.
        if code == 0 {
            return Ok(code);
        }
        ui.warn("mosh could not connect — falling back to ssh.");
    } else if !local_mosh {
        ui.note("Install mosh for a connection that survives sleep and roaming: `brew install mosh`");
    } else {
        ui.note(&format!(
            "{} has no mosh-server; using ssh. Install mosh there for a connection that survives sleep.",
            remote.name
        ));
    }

    let mut args = vec!["-t".to_string()];
    args.extend(identity::ssh_options(remote, paths, true));
    args.push("-o".to_string());
    args.push("ServerAliveInterval=20".to_string());
    args.push(remote.target());
    args.push(command.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner.run_interactive("ssh", &refs, &RunOptions::default()).await
}
```

- [ ] **Step 4: Write the flow in `remote/mod.rs`**

```rust
/// The environment the server's riabuild runs under.
pub fn env_prefix(public_id: &str, name: &str) -> String {
    format!(
        "RIABUILD_ROOT={} RIABUILD_REMOTE={name}",
        session::namespace(public_id)
    )
}

/// `riabuild remote` — the whole flow.
pub async fn run(
    ctx: &mut Ctx,
    cli: &Cli,
    target: Option<String>,
    action: Option<RemoteAction>,
) -> Result<i32> {
    let mut store = store::Store::load(ctx.paths.as_ref()).await;

    match action {
        Some(RemoteAction::List) => return list(ctx, &store),
        Some(RemoteAction::Forget { name }) => return forget(ctx, &mut store, &name).await,
        None => {}
    }

    // The laptop runs exactly two tasks: sign-in, because it mints the server's
    // session, and GitHub, because the server borrows this laptop's sign-in.
    // `github_cli`'s check also re-verifies org membership, so a departed
    // developer fails here rather than on somebody's server.
    ensure_local_prerequisites(ctx).await?;

    let remote = choose(ctx, &mut store, target).await?;
    let member = ctx
        .member
        .clone()
        .ok_or_else(|| anyhow!("riabuild does not know who you are yet"))?;
    let version = ctx.org()?.latest_cli_version.clone();

    ctx.ui.heading(&format!("Connecting to {}", remote.target()));
    identity::ensure_key(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &ctx.ui).await?;
    identity::trust_host(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &ctx.ui).await?;
    identity::authorise(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &ctx.ui).await?;
    let binary =
        install::ensure_riabuild(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &ctx.ui, &version)
            .await?;

    session::ensure(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &ctx.api,
        &member,
        &ctx.web_url,
        &ctx.cli_version,
    )
    .await?;

    let prefix = env_prefix(&member.public_id, &remote.name);
    session::seed_github(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &format!("{prefix} {binary}"),
    )
    .await?;

    warn_about_macos_claude(ctx, &remote).await;

    ctx.ui.heading(&format!("Checking {}", remote.name));
    let mut setup = format!("{prefix} {binary} --no-shell");
    if cli.check {
        setup.push_str(" --check");
    }
    if cli.quiet {
        setup.push_str(" --quiet");
    }
    if let Some(project) = &cli.project {
        setup.push_str(&format!(" --project {project}"));
    }
    let code = shell::open(&remote, ctx.paths.as_ref(), ctx.runner.clone(), &ctx.ui, &setup).await?;
    if code != 0 {
        return Ok(code);
    }

    remember(ctx, &mut store, &remote, &version).await?;
    if cli.no_shell || cli.check {
        return Ok(0);
    }
    shell::open(
        &remote,
        ctx.paths.as_ref(),
        ctx.runner.clone(),
        &ctx.ui,
        &format!("{prefix} {binary} shell"),
    )
    .await
}
```

Write `ensure_local_prerequisites`, `choose`, `remember`, `list`, `forget` and
`warn_about_macos_claude` as small private functions in the same file. `choose` asks the
three questions from Task 12 when there is nothing saved, offers a numbered list when there
is more than one, and reconnects silently when there is exactly one. If `mod.rs` passes
roughly 300 lines, move `choose`, `list` and `forget` into `store.rs`, which is where the
saved servers already live.

`warn_about_macos_claude` runs `uname -s` on the server and, on `Darwin`, prints the warning
from the design — Claude Code keeps its credentials in the account's login keychain rather
than in `CLAUDE_CONFIG_DIR`, so everyone sharing that account shares one Claude sign-in, and
unlocking the keychain over SSH exposes it to them. Name the other developers by reading the
sibling `owner.json` files.

- [ ] **Step 5: Dispatch it from `main.rs`**

Replace the placeholder arm from Task 14 with a call to `remote::run(&mut ctx, &cli, target, action).await`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd riabuild-cli && cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add riabuild-cli/src
git commit -m "Provision a server and open the environment on it"
```

### Task 22: `list`, `forget`, docs, and the container test

**Files:**
- Modify: `riabuild-cli/src/remote/mod.rs` (or `store.rs`)
- Create: `.github/workflows/ci.yml` job `remote-mode`
- Create: `e2e/remote/Dockerfile`, `e2e/remote/run.sh`
- Modify: `README.md`, `riabuild-cli/CLAUDE.md` (the layout table)

**Interfaces:**
- Consumes: everything above.
- Produces: no new interfaces.

- [ ] **Step 1: Write the failing tests for `forget`**

```rust
#[tokio::test]
async fn forgetting_a_server_removes_the_key_the_entry_and_the_session() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = crate::paths::RealPaths::rooted_at(home.path());
    let remote = remote();
    tokio::fs::create_dir_all(paths.identity_dir()).await.expect("mkdir");
    tokio::fs::write(paths.identity_dir().join(remote.hash()), "KEY").await.expect("key");

    let mut store = store::Store::default();
    store.remotes.push(record_for(&remote));
    store.save(&paths).await.expect("save");

    let fake = Arc::new(FakeRunner::new().with("ssh", 0, "", "").with("security", 0, "", ""));
    forget_remote(&paths, fake.clone(), &Ui::new(true), &mut store, "build-01")
        .await
        .expect("forgets");

    assert!(store.find("build-01").is_none());
    assert!(!paths.identity_dir().join(remote.hash()).exists());
    assert!(
        fake.calls().iter().any(|call| call.contains("rm -rf")),
        "the namespace on the server goes too: {:?}",
        fake.calls()
    );
}

#[tokio::test]
async fn forgetting_an_unreachable_server_says_what_it_left_behind() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let paths = crate::paths::RealPaths::rooted_at(home.path());
    let mut store = store::Store::default();
    store.remotes.push(record_for(&remote()));

    let fake = Arc::new(FakeRunner::new().with("ssh", 255, "", "Connection refused"));
    forget_remote(&paths, fake, &Ui::new(true), &mut store, "build-01")
        .await
        .expect("must still forget locally");

    // The local half always succeeds: a server you cannot reach must not be a
    // server you cannot remove.
    assert!(store.find("build-01").is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd riabuild-cli && cargo test remote::`
Expected: FAIL — `forget_remote` does not exist.

- [ ] **Step 3: Implement `list` and `forget`**

`list` prints each saved server as `name  user@host[:port]  used <when>`, using
`ui::duration_words` for the last column. `forget_remote` does, in order: revoke the session
through `DELETE /api/v1/cli/sessions` if the API exposes it — otherwise delete the keychain
item, which is what stops this laptop using it — delete the key pair, remove the
`remotes.json` entry, and then attempt the server-side cleanup:

```rust
    let cleanup = format!(
        "rm -rf {} && sed -i.bak '/riabuild {}/d' ~/.ssh/authorized_keys",
        session::namespace(public_id),
        remote.target()
    );
```

If the server cannot be reached, say exactly what was left behind rather than failing:

```rust
        ui.warn(&format!(
            "Could not reach {}. Its riabuild namespace and authorized_keys line are still there.",
            remote.host
        ));
```

- [ ] **Step 4: Write the container test**

`e2e/remote/Dockerfile` builds a Debian image with `openssh-server`, one unprivileged user,
and that user's `authorized_keys` pre-seeded from a key CI generates. Pre-seeding is
deliberate and is a **stated gap**: it means the container run exercises everything except
`ssh-copy-id`, which needs a password prompt no CI job can answer. That path stays covered
by the unit tests in Task 16.

`e2e/remote/run.sh` then, against that container:

1. runs `riabuild remote testuser@localhost:2222` as one developer, with a stub
   riabuild-web, and asserts the flow completes
2. repeats as a second developer with a different `publicId`
3. asserts `~/.riabuild-remote/<id-a>` and `<id-b>` both exist and hold their own
   `state.json`, `gitconfig` and `owner.json`
4. asserts `~/.riabuild/node/<version>` exists exactly once — one toolchain, two developers
5. asserts each developer's checkout is under `~/Clubria/<login>/`
6. asserts that after both sessions end, `find / -name hosts.yml` finds nothing under any
   runtime directory

Add a `remote-mode` job to `.github/workflows/ci.yml` that builds the image and runs the
script.

- [ ] **Step 5: Run it locally**

Run: `bash e2e/remote/run.sh`
Expected: all six assertions pass.

- [ ] **Step 6: Update the documentation**

Add `remote/` and `gh_session.rs` and `scope.rs` to the layout table in
`riabuild-cli/CLAUDE.md`, and add a *Working on a server* section to `README.md` covering
`riabuild remote`, `list` and `forget`.

- [ ] **Step 7: Commit and open the Stage C pull request**

```bash
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
git add .
git commit -m "List and forget servers, and prove isolation in a container"
gh pr create --fill
gh pr checks --watch
```

---

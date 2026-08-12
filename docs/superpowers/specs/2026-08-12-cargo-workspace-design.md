# Cargo workspace — Design

**Date:** 2026-08-12
**Status:** Implemented
**Scope:** `riabuild-cli/` only. No behaviour change, no change to the shipped binary.

## Purpose

`riabuild-cli` was one crate holding 39,674 lines of Rust and 944 tests. Every edit anywhere recompiles all of it, and every `cargo test`
links one binary containing all of it.

Splitting it into a workspace buys three things, in this order:

1. **Iteration time.** Editing `remote` — 24% of the crate, and where much of the recent
   work has landed — should not rebuild the other 76%. Thirteen test binaries build and
   run in parallel where one did.
2. **Invariants the compiler checks.** `riabuild-runner` cannot name a `Task`; `riabuild-fetch`
   cannot reach the API client. Today those are prose in `CLAUDE.md`.
3. **Navigability.** Thirteen crates with named, compiler-checked boundaries, rather than
   27 modules sharing one `crate::` namespace in which anything may reach anything.

This document does **not** claim to fix file size. `runner/mod.rs` is 1,950 lines and
stays 1,950 lines inside `riabuild-runner`. Splitting large files is worthwhile and
separate; conflating it with this change would hide both.

## Non-goals

- No behaviour change. The binary is byte-comparable before and after, and CI proves it.
- No change to CI, `packaging/`, `Formula/`, or `e2e/`. See [Why the workspace root
  stays put](#why-the-workspace-root-stays-put).
- No untangling of `tasks ⇄ accounts ⇄ shell ⇄ shims`. Those four modules, with `scope`,
  stay in one crate. Breaking them apart is a separate design.
- No publishing to crates.io. These crates exist for this workspace; their public
  surfaces need to be no more stable than that.

## The graph today

Cross-module dependencies form a clean layered bottom and a knot at the top:

```
leaves        archive   download   theme   version
layer 1       ui → theme+art      runner → theme      tools → archive+download
layer 2       paths → ui      config → paths      keychain/api → runner+ui
─────────────────────────────────────────────────────────────────────────────
the knot      accounts ⇄ tasks ⇄ shell ⇄ shims ⇄ channel
              testing ⇄ everything  (cfg(test), reached from production modules)
top           remote → 13 of the other 26 modules; consumed only by main + internal
```

Four edges spoil it, and all four are warts worth removing on their own merit:

| Edge | Refs | What it is | Why it is wrong |
|---|---|---|---|
| `channel → shims` | 4 | `channel::dispatch` invokes the clipboard and browser shims | `dispatch` is a command handler that drifted into a protocol module |
| `remote → main` | 2 | `remote/flow.rs` calls `crate::connect` | 20 lines of pure `Ctx` manipulation living in the binary root |
| `remote`/`tasks`/`channel` → `cli` | 4 | imports of clap-derived types | Library code reaching into the shape of `argv` |
| `ui → cli` | 1 | `art::banner` reads `cli::VERSION` | The product version is not a command-line concern |

Those eleven references are **every** upward reference in the crate. Counting cross-crate
references against the layout below finds no others, so the four refactors in
[Staging](#staging) are provably sufficient to make the graph acyclic.

## Layout

```
riabuild-cli/
  Cargo.toml            virtual workspace manifest
  Cargo.lock            unchanged location
  crates/
    theme/        riabuild-theme        423   —
    version/      riabuild-version      179   —
    fetch/        riabuild-fetch      1,549   —
    ui/           riabuild-ui         1,542   theme, version
    runner/       riabuild-runner     3,224   theme
    paths/        riabuild-paths      1,352   ui
    keychain/     riabuild-keychain   1,120   runner, ui
    api/          riabuild-api        1,308   runner, ui
    gh-session/   riabuild-gh-session   731   paths, runner, ui
    channel/      riabuild-channel    4,918   gh-session, paths, runner, ui
    tasks/        riabuild-tasks     10,608   all of the above
      assets/                               claude-statusline.js
    remote/       riabuild-remote     9,402   all of the above
    cli/          riabuild-cli        3,488   all of the above  [[bin]] name = "riabuild"
```

Module contents:

| Crate | Modules |
|---|---|
| `riabuild-theme` | `theme` |
| `riabuild-version` | `version`, including the `VERSION` constant |
| `riabuild-fetch` | `archive`, `download`, `tools` |
| `riabuild-ui` | `ui`, `art` |
| `riabuild-runner` | `runner` |
| `riabuild-paths` | `paths`, `config`, `filelock` |
| `riabuild-keychain` | `keychain` |
| `riabuild-api` | `api` |
| `riabuild-gh-session` | `gh_session` |
| `riabuild-channel` | `channel`, less `dispatch` |
| `riabuild-tasks` | `tasks`, `accounts`, `shell`, `shims`, `scope` |
| `riabuild-remote` | `remote` |
| `riabuild-cli` | `main`, `cli`, `internal`, `provision`, `reset`, `move_project`, `fs_move`, `update`, and the three relocated dispatchers |

The graph is acyclic with no upward edges. Nothing below `tasks` can name `Ctx` — which is
already true in the code today: no module in the bottom nine crates, and no part of
`channel`, mentions `Ctx` at all. The boundary is being written down, not invented.

### Why `api` and `fetch` are separate crates

They look mergeable — both are "get bytes from the network" — but `archive`, `download`
and `tools` have **no dependencies at all**, and `api` needs `runner` and `ui`. Merging
drags the tar, zip and digest code above `ui`, which owns `Failure`, the most-edited type
in the codebase. Every `ui.rs` edit would then rebuild the download verifier. That is a
direct regression against the first goal.

The separation also carries an invariant: `download` verifies published digests and
cannot reach the API client, so a server-supplied string cannot become a download URL
inside it.

### Why `fs_move` moves to the binary

`fs_move` is used by exactly one caller, `move_project`, and both belong in the binary
crate. It is in the bottom layer today only by accident. Moving it also removes the one
edge from the shared layer into `testing`, which simplifies the test-support work below.

### Why the workspace root stays put

`riabuild-cli/Cargo.toml` becomes a virtual manifest rather than moving to the repository
root. This is load-bearing, not cosmetic:

- `.github/workflows/ci.yml` runs cargo with `working-directory: riabuild-cli` and reads
  `riabuild-cli/target/release/riabuild`.
- `release.yml` builds per-target and reads `riabuild-cli/target/<triple>/release/riabuild`.
- `e2e/run.sh`, `e2e/remote/run.sh` and `e2e/remote/channel.sh` hard-code the same paths.
- `Cargo.lock` stays where `--locked` expects it.

Keeping the root at `riabuild-cli/` means **none of those files change**. Workspace member
binaries land in the shared `target/<profile>/` root, so the output path is identical.

## Removing the clap coupling

Clap types stay in the binary crate. Library crates take named data — parse, don't pass
the parser. Three mechanisms, applied per site:

**Move the dispatcher up.** Where the only use of an action enum is a `match` mapping to
public functions, the match belongs in the binary and the arms become the crate's public
API. This covers three of the four sites and is a single pattern:

| Today | After |
|---|---|
| `channel::dispatch(&ChannelAction, quiet)` | binary matches `ChannelAction`; `channel` exposes the arms. The import disappears entirely. |
| `accounts::command::run(ctx, Option<ClaudeAction>)` | binary matches; `list`, `new`, `delete`, `primary` become `pub` in `riabuild-tasks`. |
| `remote::flow::run`'s `RemoteAction` arm | binary matches `List`/`Forget`; `store::list` and `forget::forget_remote` are already `pub`. |

**Name the request.** `remote/flow/connect.rs` reads six scattered fields off the global
`Cli` — `check`, `quiet`, `project`, `no_shell`, and `accept_host_key` dug out by matching
`cli.command`. That last one's own doc comment concedes the awkwardness. Replace with a
struct owned by `riabuild-remote`:

```rust
/// What `riabuild remote` needs from the command line, named rather than parsed.
pub struct Request {
    pub target: Option<String>,
    pub accept_host_key: Option<String>,
    pub check: bool,
    pub quiet: bool,
    pub no_shell: bool,
    pub project: Option<String>,
}
```

The binary builds it from `Cli`; `accept_host_key_of` moves up with its tests.

**Cost.** Thirteen test call sites in `remote/` construct `Cli::parse_from(["riabuild",
"remote", …])` and pass `&cli`. They become `Request { … }` literals, which states the
intent directly instead of round-tripping through `argv` to reach it.

## Test support across crate boundaries

`cfg(test)` code is not shared between crates. `testing.rs` is `#![cfg(test)]`, builds a
`Ctx` against a tempdir, and is reached from nine modules that will span four crates.
This is the largest hidden cost in the change and the one most likely to be
underestimated.

The surface is small: eight items, all already `cfg(test)`, all in the bottom layer.

| Item | Crate |
|---|---|
| `FakeRunner`, `runner::Recorded` | `riabuild-runner` |
| `MemoryKeychain` | `riabuild-keychain` |
| `Ui::scripted`, `asked`, `noted`, `warned` | `riabuild-ui` |
| `RealPaths::rooted_at` | `riabuild-paths` |
| `Theme::with_depth` | `riabuild-theme` |

Each becomes `#[cfg(any(test, feature = "testing"))]` behind a `testing` feature on its
own crate. `riabuild-tasks` gains a `testing` feature that enables the others and hosts
the `Ctx` builders (`test_ctx`, `ctx_with`, `build`) that `testing.rs` holds today.
`riabuild-channel`, `riabuild-remote` and the binary take it as a **dev-dependency**.

The feature cannot reach the shipped binary, because `cargo build` never resolves
dev-dependencies. That is a guarantee of the resolver rather than a convention — but it
is narrower than it first looks, and the original wording here was too strong.
`cargo build --all-targets` *does* build test targets, which pulls dev-dependencies into
the graph, and cargo then unifies their features onto the same copy of the library the
binary links. A `riabuild` produced that way has `testing` compiled in, and would take
every prompt's default on a real laptop.

Nothing ships such a binary: `release.yml` and the e2e scripts all use plain
`cargo build --release --locked`. But "the resolver guarantees it" is only true of the
command that actually builds the release, so CI asserts the graph rather than trusting
the sentence — and asserts that the feature still exists to be absent, since an
absence-only check goes green the moment it is renamed.

## What must move to the workspace root

Two of these fail silently if forgotten, which is why they are called out rather than
left to the implementation.

**`[profile.release]`.** Profiles in member manifests are **ignored with only a warning**.
Losing `lto`, `codegen-units = 1` and `strip` ships an unoptimised, unstripped binary —
and the existing comment records that stripping interacts with Apple ad-hoc signing, so
the failure would not stay cosmetic.

**`[workspace.lints.clippy] unwrap_used = "deny"`**, with `[lints] workspace = true` in
all thirteen members. Missing this drops the panic policy from twelve of thirteen crates.
`CLAUDE.md` is explicit that a panic is riabuild's worst failure: it prints a backtrace on
a developer's laptop, says nothing about what riabuild was doing, and leaves the machine
half-provisioned. The `cfg_attr` exempting tests is replicated per crate.

**`[workspace.dependencies]`** for every third-party crate. The current manifest's
comments are documentation of decisions that cost real debugging — `deflate-flate2` rather
than `deflate` to keep zopfli and a second deflate backend out of the binary,
`rustls-tls-native-roots` so a corporate TLS-inspecting proxy does not break every
command, `tokio` without `rt-multi-thread` so a stray `Runtime::new()` cannot spawn a
worker pool. Copying those into thirteen manifests guarantees drift. They live once, at
the root; members inherit with `workspace = true`.

## Payoff

Incremental rebuild is the crate you touched plus everything downstream of it.

| Edit | Today | After | Win |
|---|---|---|---|
| `remote` | 39.8k | 12.9k | **3.1×** |
| `tasks` | 39.8k | 23.5k | 1.7× |
| `channel` | 39.8k | 28.4k | 1.4× |
| `api` | 39.8k | 24.8k | 1.6× |
| `fetch` | 39.8k | 25.0k | 1.6× |
| `runner` | 39.8k | 34.8k | 1.1× |
| `ui`, `theme` | 39.8k | ~39.8k | none — accepted |

Editing the universal bottom still rebuilds nearly everything. That is inherent, not a
flaw in the split: `Failure` and `Theme` are reached from everywhere, and no boundary
changes that.

The effect that shows up on **every** run rather than only on downstream edits is test
linking. One binary of 944 tests became thirteen, built and run in parallel, the largest
holding 275. `cargo test -p riabuild-channel` runs 137 tests without relinking 35k
unrelated lines.

Release builds are unaffected either way: `lto = true` at the workspace root still
performs cross-crate LTO.

## Invariants gained, and the honest limit

Crate boundaries enforce **reachability**. That is genuinely useful here:

- `riabuild-runner`, `riabuild-fetch`, `riabuild-keychain` and `riabuild-api` cannot name
  `Ctx` or `Task`. Setup logic cannot leak downward into the primitives.
- `riabuild-tasks` cannot reach `riabuild-remote`, so a task cannot grow a remote-mode
  branch — which is exactly what `Ctx.server`'s doc comment warns against today, in prose.
- The binary is the only crate that can see clap types, so no library can branch on the
  shape of `argv`.

It does **not** enforce policy. "No task shells out to Homebrew, apt or dnf" and "secrets
are brokered, never stored" both live inside a single crate, and a crate boundary cannot
see them. They still need tests or lints. Selling the workspace on those would be selling
a promise it cannot keep.

## What implementation found

Three things the graph did not show, all of which would have failed late and
confusingly.

**`tokio`'s `sync` feature was used and never declared.** `riabuild-runner` (`Notify`,
`Mutex`) and `riabuild-channel` (`watch`, `Mutex`) both need it, and it was reaching them
only because `hyper` asks `reqwest` for it and cargo unifies features graph-wide. Neither
crate has `reqwest` anywhere in its own graph, so `cargo build -p riabuild-runner` would
have failed on a dependency the single-crate build had been getting for free. This is the
general hazard of a workspace: feature unification hides missing declarations until
something is built alone.

**`Ui::new` decided interactivity from `cfg!(test)`.** A dependency crate is never
compiled with `cfg(test)`, so once `riabuild-ui` was a library the flag read false while
downstream tests ran, and `interactive` became whatever the developer's terminal
reported. Paired with `#[cfg(not(test))] fn read_answer`, a test that reached a prompt
would have blocked on a real `stdin().read_line()` — a hung suite with no output, which
is exactly what the "every prompt has a default" invariant exists to prevent. Both gates
now read `any(test, feature = "testing")`, and they had to move together: changing either
alone is worse than changing neither.

**The panic policy needed thirteen exemptions, not one.** `unwrap_used = "deny"` was
exempted for tests by a single `#![cfg_attr(test, allow(...))]` at the old crate root.
Every crate root needs its own now, and the six that export test scaffolding need the
feature in the gate as well — with `testing` on, the crate is compiled as a dependency
and `cfg(test)` is false, so a `test`-only exemption would not apply.

Two smaller corrections: `filelock` (added by #53 while this was being designed) belongs
in `riabuild-paths` and brings `libc` with it, and `riabuild-paths` was the only crate
whose third-party dependency list the design got wrong.

## Risks

| Risk | Mitigation |
|---|---|
| 457 cross-crate reference rewrites (measured before `filelock` landed); intermediate states do not compile | One PR, staged as reviewable commits. 82% target a bottom-layer crate and are a mechanical prefix rewrite. |
| `[profile.release]` silently ignored in a member | Called out above; CI compares release binary size against `main`. |
| Lint policy dropped from new crates | `[workspace.lints]` + `[lints] workspace = true` in every member; clippy runs `--all-targets` over the workspace already. |
| `testing` feature leaking into the release build | Dev-dependency only; CI asserts the feature is off for `cargo build --release`. |
| `option_env!("RIABUILD_VERSION")` moves to `riabuild-version` | Cargo passes the environment to every crate in the build; verified by a release dry-run before merge. |
| Behaviour drift during the move | No logic changes except the four documented refactors; the 928 tests move with their code and must all pass. |

## Staging

One PR, four commits, each compiling and green:

1. **Prep, still one crate.** `cli::VERSION` → `version::VERSION`; `main::connect` →
   `Ctx::connect`; `main::build_ctx` → `Ctx::new`; `channel::dispatch`,
   `accounts::command::run` and `remote::flow::run`'s action arm up to `main`;
   `remote::Request` replacing `&Cli`; `path_without` down out of `shims`.
2. **Workspace scaffold and the bottom layer.** Virtual manifest, `[workspace.dependencies]`,
   `[workspace.lints]`, `[profile.release]`; extract `theme`, `version`, `fetch`, `ui`,
   `runner`, `paths`, `keychain`, `api`, `gh-session`, including the `testing` features.
3. **`channel`, `tasks`, `remote`,** and the binary reduced to what remains.
4. **Docs and CI.** Root `CLAUDE.md` layout table, `riabuild-cli/CLAUDE.md`, the
   `riabuild-cli/assets/` reference in "The server ships data, never logic" (assets move
   with their `include_str!` to `crates/tasks/assets/`), plus the binary-size and
   feature-leak assertions.

## Open questions

None. The knot (`tasks ⇄ accounts ⇄ shell ⇄ shims`) is deliberately left whole; if
iteration on it still feels slow after this lands, that is the next design, informed by
real use rather than by the graph alone.

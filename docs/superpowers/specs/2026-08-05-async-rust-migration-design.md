# riabuild-cli — async migration

**Date:** 2026-08-05
**Status:** Approved
**Scope:** `riabuild-cli` only. `riabuild-web` is untouched.
**Supersedes:** nothing. Extends [2026-08-04-riabuild-design.md](2026-08-04-riabuild-design.md).

## Purpose

Move `riabuild-cli` from fully synchronous IO to async Rust on a current-thread tokio
runtime, and record an invariant that keeps it there.

The driver is a consistent idiom across Clubria's Rust, not throughput. Provisioning
speed is dominated by `brew install` and `npm install -g`, which block no matter what
runtime wraps them. This migration is therefore expected to be **performance-neutral**,
and a change that claims otherwise should be treated as suspect.

One exception is a genuine simplification rather than a reskin, and it is the best
argument for the work — see [The loopback listener](#the-loopback-listener).

## Decisions

| Question | Decision |
|---|---|
| Runtime | `#[tokio::main(flavor = "current_thread")]`, no `rt-multi-thread` |
| Trait strategy | `#[async_trait]` on all three IO traits |
| HTTP client | `reqwest`, `rustls-tls` + `rustls-platform-verifier` |
| Filesystem | `tokio::fs`, accepting that it is threadpool-backed |
| Where the rule lives | Invariant in `riabuild-cli/CLAUDE.md`, prose only |
| Concurrency | Deferred. Waves preserved; `Ctx` split not attempted |

## Non-goals

- **Concurrent task execution.** Deferred deliberately, see [Deferred](#deferred).
- **Lint enforcement of the async rule.** Considered and rejected, see
  [The invariant](#the-invariant).
- **Streaming downloads to disk.** Rejected — it would weaken a security property, see
  [HTTP](#http).
- Any change to `riabuild-web` or the `/api/v1` contract. The wire format is identical
  before and after.

## Blast radius

Measured across `riabuild-cli/src`, production code only:

| Surface | Sites | Files |
|---|---|---|
| `ureq` HTTP | 5 | `api/mod.rs`, `download.rs` |
| `std::fs` | ~50 | 12 files |
| `std::process` | 3 | `runner.rs` only |
| Loopback TCP | 1 listener | `api/auth.rs` |

`std::process` appearing in exactly one file is the existing **"every external process
goes through `CommandRunner`"** invariant paying for itself: the entire subprocess
migration is confined to `runner.rs`.

## Runtime

`main` becomes `#[tokio::main(flavor = "current_thread")]`. Tokio features: `rt`,
`macros`, `fs`, `process`, `net`, `io-util`, `time`. `rt-multi-thread` is not enabled.

### `tokio::fs` is threadpool-backed, and that is accepted

There is no portable async file API at the OS level. `tokio::fs` is `std::fs` dispatched
to a `spawn_blocking` pool, which has two consequences that must not surprise a future
reader:

1. **"Single-threaded" describes the reactor, not the process.** Once `tokio::fs` is in
   use, the binary has blocking-pool threads. This is tokio working as designed.
2. **Per-call file IO gets marginally slower**, not faster — a channel hop and a thread
   wake versus a direct syscall. At ~50 call sites this is unmeasurable, but it is a
   cost, not a saving.

The alternative — keeping `std::fs` and exempting files from the rule — was offered and
rejected. A single idiom is the point of the exercise, and an "async everywhere except
files" rule is the kind of carve-out that erodes.

## HTTP

`reqwest` with `default-features = false`, `rustls-tls`, `rustls-platform-verifier`.

- `ApiClient` holds one `reqwest::Client` for the process lifetime. This is an
  improvement on today's per-call `ureq::request`, which pools nothing.
- Error mapping is unchanged. `ApiError` still deserializes from the `{ error: … }`
  envelope, still carries `status`/`code`/`message`/`action`, and is still recovered by
  `downcast_ref` at the `main` boundary and in `connect()`.

### Downloads stay buffered

`download.rs` continues to read whole tarballs into memory rather than streaming to
disk. `fetch_bytes` keeps its 400 MB cap and 300 s timeout, via `.bytes().await`.

This is deliberate: the sha256 checksum is verified against the complete byte buffer
**before** anything is extracted. Streaming to disk would mean writing unverified bytes
to a developer's filesystem and checking them afterwards, which is a weaker property for
a tool whose job is to install executables.

### Trust roots change, deliberately

Today `ureq` bundles `webpki-roots`. After this change, `rustls-platform-verifier` uses
the OS trust store.

This **fixes a latent failure**: a developer behind a corporate TLS-inspecting proxy
currently gets an opaque TLS error from every `riabuild` command, with a MITM root that
is installed and trusted by every other tool on their machine. For a provisioner, that
is the worst possible first-run experience. After this change, such a laptop works.

The cost is that riabuild's trust decisions now depend on machine state rather than on
what shipped in the binary. That is the correct trade for a tool that runs on laptops
we do not control.

## Traits

`#[async_trait]` on `CommandRunner`, `Keychain`, and `Task`.

`async fn` in a trait is not dyn-compatible — it returns an anonymous opaque future with
no known size, so there is no vtable layout for it. All three traits are held as
`Arc<dyn …>` or `Box<dyn …>`, and that indirection is what the entire test suite is
built on. `async_trait` boxes the returned future, restoring dyn-compatibility at the
cost of one heap allocation per call, which is irrelevant beside spawning `brew`.

Enum dispatch was considered for `CommandRunner` and `Keychain` (exactly two impls
each). Rejected: `Task` has ~10 heterogeneous impls in a registry and must stay `dyn`
regardless, so enums would buy an unmeasurable allocation saving in exchange for two
different dispatch patterns in one crate.

**`Ctx` does not change shape.** `Arc<dyn CommandRunner>` still compiles.

### What stays synchronous

| Item | Why |
|---|---|
| `Paths` (whole trait) | Pure `PathBuf` joins. Zero IO. |
| `CommandRunner::which` | One cheap PATH stat sweep; making it async infects every `check()` for no gain |
| `ui.rs` stdio | See [The invariant](#the-invariant) |
| `run_interactive` | A terminal handoff to a child, not IO riabuild performs |
| `extract_tarball` | CPU work over an in-memory `&[u8]` |
| `dirs` lookups in `paths.rs` | Environment reads |

## The loopback listener

`api/auth.rs::wait_for_code` currently hand-rolls a poll loop: non-blocking `accept()`,
a 120 ms `thread::sleep`, and `Instant` deadline arithmetic, plus a `set_read_timeout`
on each accepted stream.

```rust
// before
while Instant::now() < deadline {
    match listener.accept() { … }
    std::thread::sleep(Duration::from_millis(120));
}

// after
let (stream, _) = tokio::time::timeout(LOGIN_TIMEOUT, listener.accept()).await??;
```

The loop, the sleep, the deadline arithmetic, and the per-stream read timeout all
collapse into one `timeout`. Login behaviour is unchanged — same 180 s budget, same
`state` verification — but the polling latency floor disappears.

This is the one place where async removes code rather than adding it.

## The engine, and why concurrency is deferred

`topological_order` already computes dependency **waves**: each pass of its loop collects
every task whose dependencies are satisfied. It then flattens them into a linear
`Vec<usize>`, discarding the parallel structure.

This migration changes it to return `Vec<Vec<usize>>`, preserving the waves. Execution
stays strictly sequential — `run_all` iterates waves, then tasks within each wave, in the
same deterministic `BTreeSet` order as today. **No behaviour change.** Task output order
on a developer's terminal is byte-identical.

Concurrency is deferred because waves are not the blocker. `Task::apply` takes
`&mut Ctx`, and N concurrent tasks cannot each hold a `&mut`. Real concurrency requires
splitting `Ctx` into a shared immutable part and per-task mutable state — a change with
its own design questions about how `notes`, `env`, and `state` merge across tasks. That
belongs in its own spec, not smuggled into a runtime migration.

Preserving the waves now costs almost nothing and means that work starts from a graph
that still knows its own shape.

## The invariant

Added to `riabuild-cli/CLAUDE.md` in the existing **Invariants** section, beside "Every
external process goes through `CommandRunner`". That file is auto-loaded whenever anyone
works in the CLI, which is the property being bought — a skill would only load when
explicitly invoked.

The invariant states: all IO is async, via `tokio::fs`, `reqwest`, and `tokio::process`;
never `std::fs` or `std::process`. It names stdio as the exception — `ui.rs` writes with
`println!`/`eprintln!`, and `run_interactive` hands the terminal to a child — and lists
the CPU-bound sync exemptions above so they do not read as violations.

**Prose only, no lint.** A `clippy.toml` `disallowed-methods` list was considered, by
analogy with the `unwrap_used` deny landed the same day. Rejected because the exemptions
here are legitimate and numerous — stdio, tarball extraction, path probing — so the lint
would demand `#[allow]` churn at every one. `unwrap_used` had zero legitimate production
exemptions, which is what made denying it clean. If `std::fs` starts creeping back in,
revisit.

## Testing

The suite is the correctness gate: **136 tests pass today and the same 136 must pass
after.** A dropped test is a migration failure, not a cleanup.

- Every `#[test]` touching IO becomes `#[tokio::test]` (current-thread by default).
- `FakeRunner` and `MemoryKeychain` gain `#[async_trait]`.
- `FakeRunner`'s `Mutex<Vec<String>>` call log stays a `std::sync::Mutex`. It is never
  held across an `.await`; `tokio::sync::Mutex` is only required when a guard must
  survive a yield point, and paying for it reflexively is a common async-migration error.
- Pure-logic tests (`version.rs`, `topological_order`, `ui.rs` rendering) stay
  synchronous `#[test]`. Making them async would be noise.

## Sequencing

**One PR, sequenced into reviewable commits.**

Async is viral: it propagates up every caller to `main`, so a partially converted crate
does not compile. Splitting into two PRs bridged by `block_on` was considered and is
worse than it sounds — on a current-thread runtime `block_in_place` panics outright, and
`Runtime::block_on` panics when called from inside a runtime. A bridge only survives
while `main` is still synchronous, so the intermediate state would violate the very
invariant this spec adds.

1. Runtime and dependencies
2. HTTP layer — `api/`, `download.rs`
3. Traits — `runner.rs`, `keychain.rs`, `tasks/mod.rs`
4. Filesystem call sites — the ~50 conversions
5. Engine waves
6. The CLAUDE.md invariant

## Risks

| Risk | Handling |
|---|---|
| **Binary size.** tokio + reqwest + rustls is a real increase, and `Cargo.toml` documents that every developer downloads this over a laptop connection | Measure release binary before and after; report the delta in the PR body. Do not absorb a bad number silently. |
| Trust-root change is a genuine behaviour change | Called out in the PR body. It fixes more than it risks. |
| `tokio::process` reaps children through a signal handler rather than a direct `wait` | Verify `run_interactive` still returns correct exit codes; existing tests cover this |
| Migration silently drops test coverage | Assert the post-migration test count is 136 |

## Deferred

- **Concurrent DAG execution** — needs the `Ctx` split described above. Own spec.
- **Lint enforcement** of the async invariant — revisit if drift appears.
- **Streaming downloads** — only worth it if tarball sizes grow enough that peak memory
  matters, and it must not weaken checksum-before-extract.

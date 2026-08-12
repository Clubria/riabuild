# Concurrent runs on one machine

**Status:** accepted, 2026-08-12
**Amends:** nothing — this closes a hole the invariants never covered

## Problem

A developer runs `riabuild` in two terminal windows. Nothing in the binary notices.

There is no lock of any kind: no `flock`, no lockfile, no single-instance guard. Every
`lock` in the crate today is an in-process `Mutex` belonging to a test double. Two
riabuild processes on one machine are wholly unaware of each other, and they share three
JSON files.

Those files are written by a read-modify-write that is **split across the process
lifetime**. `UserConfig::load` and `State::load` run once at startup; `save` serializes
the whole struct and writes it back, possibly many minutes later. The engine saves after
every task. So each save is a full-snapshot clobber built from a snapshot taken when the
process began, and the later writer wins with the staler data.

`state.json` survives this. It is a cache of decisions, and a lost `TaskRecord` costs one
redundant `check()` — exactly the degradation it is designed for.

`config.json` does not. It is the source of truth for the checkout path, the pinned Node
and pnpm versions, and the ordered list of Claude Code accounts. `riabuild claude new` in
one window appends a UUID and creates its config directory; any save from a window
holding an older snapshot deletes that account from the registry and leaves the directory
orphaned on disk. Because position *is* the account number, the next run's adoption of
that orphan can re-add it at a different index — and `claude-2` starts launching a
different account than it did yesterday. `remotes.json` has the same shape and loses a
saved server the same way.

Underneath both sits a second, independent defect: `write_json` is a plain
`tokio::fs::write`, which truncates and then writes. An interrupt inside that window
leaves a truncated file. For `state.json` that is harmless by policy. For `config.json`
it is not, because `UserConfig::load` has the same `.ok().and_then(…).unwrap_or_default()`
degradation — and a defaulted `UserConfig` silently forgets the checkout, the pinned
versions, and every account. The same degradation policy is correct for a cache and
quietly destructive for a source of truth.

The codebase has thought carefully about concurrency before, but always framed as *two
developers sharing one server*: `host_key.rs` appends rather than rewrites,
`staging.rs` renames rather than deletes, `remote/install/binary.rs` writes to a temp
name and moves. Every one of those is a lock-free technique applied to one specific file.
Nobody applied one to `config.json`, because the second process was always imagined as
another person, never as the same person's other window.

## Approach

Move the read inside the critical section, and make the write atomic.

The merge semantics one would otherwise have to design — how to reconcile two divergent
`claude_accounts` vectors, which `project_path` wins — exist only because the read
happens at startup and the write happens later. Close that gap and there is nothing to
reconcile. A single primitive replaces `save` everywhere:

```rust
impl UserConfig {
    /// acquire → load fresh from disk → mutate → write atomically → release
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self>;
}
```

`State` and `remote::Store` get the same method. Each returns the value it wrote, so the
caller's in-memory copy is refreshed rather than left to drift.

`save` is **removed**, not left beside `update` as a convenience. A public method that
writes without taking the lock is one an unrelated future change will reach for, and the
resulting lost update would look exactly like the bug this spec closes. The engine's
`mark_satisfied`-then-`save` pairs become a single `update` whose closure calls
`mark_satisfied`; `Ctx` grows `update_config` and `update_state` wrappers so call sites
keep reading as one statement.

This demotes `Ctx.config` from the authority to a read-only snapshot for the run, which
is what it honestly always was. Reads during a run — `project_dir()`, `node_version` —
are unchanged and may be one write stale, which is the same staleness they have today.

## The lock

`src/filelock.rs`: an RAII guard holding an exclusive advisory `flock`.

```rust
pub struct FileLock { file: tokio::fs::File }  // released by the kernel on close
```

`flock` is confirmed present on both supported platforms. `libc` declares it in the
shared `src/unix/mod.rs`, gated only by `cfg(not(target_os = "solaris"))`, and
`LOCK_EX`/`LOCK_NB`/`LOCK_UN` carry identical values in `bsd/apple/mod.rs` and
`linux_like/mod.rs`. No new dependency: `libc` is already in the tree for `gh_session`.

**Acquired non-blocking.** `LOCK_EX | LOCK_NB` in a retry loop with
`tokio::time::sleep` between attempts, never a blocking `flock`. A blocking lock syscall
on a current-thread runtime stalls every other future on it — the precise failure the
"All IO is async" invariant exists to prevent, and the same reasoning `runner/pty.rs`
follows when it pumps through `AsyncFd` rather than a blocking read. The loop is also
where a wait becomes visible: past a threshold the guard invokes a caller-supplied
callback, so `filelock.rs` prints nothing itself and never depends on `Ui`.

**No staleness handling, by construction.** A `flock` belongs to the open file
description, so the kernel releases it when the process dies — crash, `SIGKILL`, power
loss alike. There is no stale lockfile to detect and no PID liveness check to write. This
is the property that makes `flock` the right choice over a hand-rolled lockfile.

**The lock file is never the data file.** Writes land by temp-file-and-rename, so a lock
taken on `config.json` would be a lock on an inode the next rename unlinks: the following
process would lock a fresh inode, observe no contention, and proceed. That failure is
invisible to every single-process test. A lock's identity must outlive the data it
guards, so the locks live at their own paths.

**Two lock files, and the split is load-bearing.**

| Lock | Guards | Held for |
|---|---|---|
| `~/.riabuild/.state.lock` | every state-file read-modify-write | milliseconds |
| `~/.riabuild/.provision.lock` | `engine::run_all` | seconds to minutes |

They cannot be one. A run holding the provisioning lock saves state after every task, and
`flock` on a second descriptor for the same file blocks even within a single process — one
lock file would deadlock riabuild against itself on the first task it completed.

## The atomic write

`write_json` becomes serialize → write `<path>.tmp.<pid>` → `sync_all` → `rename`. The
temporary sits in the same directory as its target, so the filesystem is the same one and
the rename is atomic rather than a copy — the reasoning `archive/staging.rs` already uses
to land a tool tree.

A reader therefore sees the old file or the new one and never a torn one, which is what
makes concurrent `--check` safe without giving it a lock.

The same treatment applies to the generated launchers in `~/.riabuild/bin`
(`shims/mod.rs`, four call sites). Their content is deterministic, so concurrent writers
agree and no lock is needed — but an interrupt mid-write leaves a truncated `claude-2`
that fails with a shell syntax error, and temp-and-rename removes that outcome for the
same few lines.

## The provisioning lock

Held across `engine::run_all` in `provision.rs`. It prevents two runs from both
downloading and installing the same toolchain — roughly 130 MB per lost race, leaked into
a directory nothing sweeps.

Its boundaries matter at both ends:

- Acquired **after** `update::upgrade_and_reexec`. A `flock` survives `exec`, so
  acquiring first would carry the lock into the replacement process image with no guard
  tracking it and nothing to release it.
- Released **before** `shell::spawn`. That call awaits the developer's interactive shell
  through `run_interactive`, so riabuild is alive for the whole session — potentially
  hours. A lock held there would make the second window hang until the first developer
  closed their terminal, which is a worse defect than the one being fixed.

Skipped entirely when `ctx.dry_run`. `--check` writes nothing and must never block.

**Per-namespace, not per-machine.** The lock path derives from `paths.root()`, which is
already namespaced per member in remote mode. Two developers on one server can still
install concurrently, and `staging.rs` already makes that safe. A machine-wide lock there
would let one developer block another under a shared uid — a denial of service wearing
robustness as a disguise.

## Errors

Failing to acquire within a cap — fifteen minutes for provisioning, seconds for state —
becomes a `Failure` carrying what was attempted, the detail, and one next action. Never a
panic: `unwrap_used` is denied crate-wide precisely so a provisioner never answers a
developer with a backtrace.

A wait prints a notice once it passes about a second, and again periodically, so waiting
never reads as hanging.

Corrupt JSON still degrades to `default` under the lock — the policy is unchanged — but
the atomic write that follows means the degraded value cannot re-corrupt the file for the
next reader.

Non-unix gets a no-op guard, mirroring the existing `#[cfg(not(unix))]` arms and their
standing caveat that no CI job compiles them.

## Testing

Real concurrency, following the pattern `remote/host_key.rs` already establishes for two
concurrent host-key pins.

- Two tasks contend for a lock; the second observes the first's write rather than
  overwriting it.
- **The regression test that fails today:** two concurrent `claude new`-shaped config
  updates, asserting both accounts survive and both directories remain registered.
- A `load` racing many `update`s never returns `default`, proving no reader observes a
  truncated file.
- `--check` acquires no provisioning lock, and the provisioning lock is released before
  the interactive shell call `FakeRunner` records.
- A guard dropped while held releases the lock, so a second acquire succeeds.

## Out of scope

Cross-developer locking on a shared server, for the denial-of-service reason above.
Duplicated downloads between two developers on one server remain possible and remain
harmless.

# Concurrent Run Safety Implementation Plan

> **Completed — historical record, do not execute.** Shipped in #53, 2026-08-12. The
> unchecked `- [ ]` boxes below are how the plan was written and not work outstanding, and
> the instruction to an agentic worker to implement it task-by-task that stood here has
> been removed: acting on it would rebuild something that already ships. See
> [`README.md`](README.md) for the index, and the design spec for what the code does now.

**Goal:** Make `riabuild` safe to run in two terminal windows at once, so no run can lose another's writes to `config.json`, `state.json` or `remotes.json`, and no interrupt can leave a half-written file behind.

**Architecture:** An advisory file lock (`std::fs::File::try_lock` / `lock`, the same API cargo uses) wraps a read-modify-write that now happens entirely inside the critical section — the read moves from process start to save time, which removes any need for merge semantics. Every write lands by `rename` from a same-directory temporary, so a reader sees the old file or the new one and never a torn one. A second, separate lock spans the provisioning phase so two runs do not install the same toolchain twice.

**Tech Stack:** Rust 2024 edition, tokio (current-thread), serde_json, anyhow. No new dependencies.

Spec: `docs/superpowers/specs/2026-08-12-concurrent-runs-design.md`. Read it before Task 1.

## Global Constraints

- **All IO is async.** `tokio::fs`, never `std::fs`, except where this plan explicitly says otherwise (the lock file handle, which exists to be locked and is never read or written through).
- **No panics.** `unwrap_used = "deny"` is crate-wide. Use `?`, `let else`, or `match`. Tests are exempt.
- **No `cfg!(target_os)` or `std::env::consts::OS`** outside `paths.rs`, `keychain/`, `tools.rs`, `download/`, `update.rs`. `#[cfg(unix)]` is fine and already used in `keychain/file.rs`.
- **Roughly 300 lines of production code per file**, `#[cfg(test)]` modules excluded.
- **Every task ends green**: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. All commands run from `riabuild-cli/`.
- **Minimum Rust 1.89.0** for the file-locking API. Nothing pins riabuild below it — no `rust-toolchain.toml`, no `rust-version` in `Cargo.toml`, CI is `dtolnay/rust-toolchain@stable`.
- **Work happens on branch `worktree-fix+concurrent-run-safety`** in this worktree. Do not push to `main`.

---

### Task 1: The lock primitive

**Files:**
- Create: `riabuild-cli/src/filelock.rs`
- Modify: `riabuild-cli/src/main.rs` (add `mod filelock;`)
- Modify: `riabuild-cli/src/paths.rs` (add two default trait methods after `config_file`, around line 30)
- Test: inline `#[cfg(test)]` module in `riabuild-cli/src/filelock.rs`

**Interfaces:**
- Consumes: `crate::paths::Paths` (existing trait).
- Produces:
  - `filelock::FileLock` — an RAII guard. Dropping it releases the lock.
  - `FileLock::acquire(path: &Path, on_wait: impl FnOnce()) -> Result<FileLock>`
  - `Paths::state_lock_file(&self) -> PathBuf` — `root()/.state.lock`
  - `Paths::provision_lock_file(&self) -> PathBuf` — `root()/.provision.lock`

- [ ] **Step 1: Add the two lock paths to the `Paths` trait**

In `riabuild-cli/src/paths.rs`, directly after the existing `config_file` method:

```rust
    /// Guards a read-modify-write of `state.json`, `config.json` or
    /// `remotes.json`. Held for milliseconds.
    ///
    /// Deliberately not any of those files: writes land by `rename`, so a lock
    /// taken on the data file would be a lock on an inode the next write
    /// unlinks — the following process would lock a fresh inode, see no
    /// contention, and proceed. A lock's identity has to outlive the data it
    /// guards.
    fn state_lock_file(&self) -> PathBuf {
        self.root().join(".state.lock")
    }

    /// Guards the provisioning phase, so two runs do not install one toolchain
    /// twice. Held for seconds to minutes, and never across the shell handoff.
    ///
    /// Separate from `state_lock_file` because a run holding this one saves
    /// state after every task, and `std` is explicit that a second lock taken
    /// by a process already holding one is unspecified and may deadlock.
    fn provision_lock_file(&self) -> PathBuf {
        self.root().join(".provision.lock")
    }
```

- [ ] **Step 2: Write the failing tests**

Create `riabuild-cli/src/filelock.rs` with only the test module and a stub, so the file compiles and the tests fail for the right reason:

```rust
//! One exclusive advisory lock, so two riabuilds on one machine take turns.

use anyhow::{Context, Result};
use std::path::Path;

pub struct FileLock {
    /// `None` when the filesystem refused to lock at all. The guard still
    /// exists so every caller has one shape to handle.
    _file: Option<std::fs::File>,
}

impl FileLock {
    pub async fn acquire(_path: &Path, _on_wait: impl FnOnce()) -> Result<Self> {
        anyhow::bail!("not implemented")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[tokio::test]
    async fn an_uncontended_lock_is_taken_without_reporting_a_wait() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join(".state.lock");
        let waited = Arc::new(AtomicBool::new(false));

        let flag = Arc::clone(&waited);
        let _lock = FileLock::acquire(&path, move || flag.store(true, Ordering::SeqCst))
            .await
            .expect("acquire");

        assert!(
            !waited.load(Ordering::SeqCst),
            "an uncontended acquire must say nothing at all"
        );
        assert!(path.exists(), "the lock file is created on demand");
    }

    #[tokio::test]
    async fn the_parent_directory_is_created_when_it_is_missing() {
        // The very first riabuild on a machine locks before ~/.riabuild exists.
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join("riabuild").join(".state.lock");

        let _lock = FileLock::acquire(&path, || {}).await.expect("acquire");

        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_dropped_lock_can_be_taken_again() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join(".state.lock");

        let lock = FileLock::acquire(&path, || {}).await.expect("first");
        drop(lock);

        let waited = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&waited);
        let _second = FileLock::acquire(&path, move || flag.store(true, Ordering::SeqCst))
            .await
            .expect("second");

        assert!(
            !waited.load(Ordering::SeqCst),
            "a released lock is not contended"
        );
    }

    /// The point of the whole file: a second acquire waits for the first, and
    /// says so exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_contended_lock_waits_for_the_holder_and_reports_it_once() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join(".state.lock");
        let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let waits = Arc::new(AtomicUsize::new(0));

        let held = FileLock::acquire(&path, || {}).await.expect("first");

        let second = {
            let path = path.clone();
            let order = Arc::clone(&order);
            let waits = Arc::clone(&waits);
            tokio::spawn(async move {
                let counter = Arc::clone(&waits);
                let lock = FileLock::acquire(&path, move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
                .await
                .expect("second acquire");
                order.lock().expect("lock").push("second acquired");
                drop(lock);
            })
        };

        // Give the spawned task time to reach the blocking wait, then release.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        order.lock().expect("lock").push("first released");
        drop(held);

        second.await.expect("join");

        assert_eq!(
            *order.lock().expect("lock"),
            vec!["first released", "second acquired"],
            "the second acquire must not complete before the first releases"
        );
        assert_eq!(
            waits.load(Ordering::SeqCst),
            1,
            "contention is reported once, not once per retry"
        );
    }
}
```

- [ ] **Step 3: Run the tests and watch them fail**

```sh
cd riabuild-cli && cargo test filelock
```

Expected: all four fail with `not implemented`.

- [ ] **Step 4: Implement `acquire`**

Replace the `impl FileLock` block in `riabuild-cli/src/filelock.rs`:

```rust
impl FileLock {
    /// Takes the lock, calling `on_wait` once if — and only if — another
    /// process holds it and this call is about to wait.
    ///
    /// `try_lock` first, then report, then block: the uncontended path costs
    /// one syscall and says nothing, and a wait is announced exactly once
    /// rather than once per poll. This is cargo's sequence, which developers
    /// here already meet as "Blocking waiting for file lock" when two
    /// worktrees build at once.
    pub async fn acquire(path: &Path, on_wait: impl FnOnce()) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("could not create {}", parent.display()))?;
        }

        // Opened through tokio and handed over, because the locking methods
        // live on `std::fs::File` and `tokio::fs::File` does not have them.
        // `read` as well as `write` is required: Windows refuses to lock a
        // handle opened append-only, and this costs nothing on unix.
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .await
            .with_context(|| format!("could not open {}", path.display()))?
            .into_std()
            .await;

        match file.try_lock() {
            Ok(()) => return Ok(Self { _file: Some(file) }),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) if refuses_to_lock(&error) => {
                // Fail open. riabuild is the first thing to run on a machine
                // nobody has characterised — a home on NFS, an unusual
                // container filesystem — and "cannot provision, because cannot
                // lock" is a worse answer for a provisioner than the rare
                // interleaving the lock guards against.
                return Ok(Self { _file: None });
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error)
                    .with_context(|| format!("could not lock {}", path.display()));
            }
        }

        on_wait();

        // `lock()` parks the thread until the holder releases, and riabuild
        // runs its reactor on this one — so it goes to the blocking pool. The
        // file moves in and comes back out; `std::fs::File` is `Send`.
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
            file.lock()?;
            Ok(file)
        })
        .await
        .context("the thread waiting for the lock did not finish")?
        .with_context(|| format!("could not lock {}", path.display()))?;

        Ok(Self { _file: Some(file) })
    }
}

/// Whether the filesystem is telling us it does not do locking.
///
/// Written as comparisons rather than an or-pattern on purpose: `ENOTSUP` and
/// `EOPNOTSUPP` are both 95 on Linux and different values on macOS, so
/// `Some(libc::ENOTSUP | libc::EOPNOTSUPP)` is an unreachable-pattern warning
/// on one platform and correct on the other — and `-D warnings` turns that
/// into a Linux-only build failure.
fn refuses_to_lock(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::ENOSYS
}
```

- [ ] **Step 5: Register the module**

In `riabuild-cli/src/main.rs`, add `mod filelock;` in the existing `mod` list, alphabetically between `download` and `fs_move` (match whatever ordering is already there).

- [ ] **Step 6: Run the tests and watch them pass**

```sh
cd riabuild-cli && cargo test filelock
```

Expected: 4 passed.

- [ ] **Step 7: Check the whole crate still builds clean**

```sh
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

Expected: no warnings, all tests pass.

- [ ] **Step 8: Commit**

```sh
git add riabuild-cli/src/filelock.rs riabuild-cli/src/main.rs riabuild-cli/src/paths.rs
git commit -m "Add the file lock two riabuilds take turns on"
```

---

### Task 2: Atomic writes

**Files:**
- Modify: `riabuild-cli/src/config.rs:140-151` (`write_json`)
- Test: inline `#[cfg(test)]` module in `riabuild-cli/src/config.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `config::write_json<T: Serialize>(path: &Path, value: &T) -> Result<()>` — same signature as today, now atomic.
  - `config::write_atomic(path: &Path, bytes: &[u8]) -> Result<()>` — public, used by Task 6 for the shims.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `riabuild-cli/src/config.rs`:

```rust
    #[tokio::test]
    async fn a_write_leaves_no_temporary_behind() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join("state.json");

        write_json(&path, &State::default()).await.expect("write");

        let mut entries = tokio::fs::read_dir(home.path()).await.expect("read_dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        assert_eq!(
            names,
            vec!["state.json".to_string()],
            "the temporary must be renamed away, not left beside the target"
        );
    }

    #[tokio::test]
    async fn a_reader_never_observes_a_half_written_file() {
        // The torn-read regression. Without rename-into-place this fails,
        // because `fs::write` truncates before it writes and `load` answers a
        // truncated file with `Default`.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.expect("mkdir");

        let mut full = State::default();
        for n in 0..200 {
            full.mark_satisfied(&format!("task_{n}"), 1, "never_run");
        }
        write_json(&paths.state_file(), &full).await.expect("seed");

        let writer = {
            let paths = crate::paths::RealPaths::rooted_at(home.path());
            let full = full.clone();
            tokio::spawn(async move {
                for _ in 0..50 {
                    write_json(&paths.state_file(), &full).await.expect("write");
                }
            })
        };

        for _ in 0..50 {
            let seen = State::load(&paths).await;
            assert_eq!(
                seen.tasks.len(),
                200,
                "a reader saw a file that was neither the old one nor the new one"
            );
        }

        writer.await.expect("join");
    }
```

- [ ] **Step 2: Run the tests and watch them fail**

```sh
cd riabuild-cli && cargo test config::tests::a_write_leaves_no_temporary_behind config::tests::a_reader_never_observes_a_half_written_file
```

Expected: `a_write_leaves_no_temporary_behind` passes already (nothing writes a temporary yet); `a_reader_never_observes_a_half_written_file` fails intermittently with a length other than 200. If it passes by luck, raise the loop counts until it fails — a test that cannot fail is not evidence.

- [ ] **Step 3: Implement the atomic write**

Replace `write_json` in `riabuild-cli/src/config.rs` and add `write_atomic` beside it:

```rust
pub async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    write_atomic(path, format!("{text}\n").as_bytes()).await
}

/// Writes beside the target and renames over it, so a reader sees the whole old
/// file or the whole new one and never the gap between.
///
/// `tokio::fs::write` truncates and then writes; an interrupt inside that
/// window leaves a truncated file, and `UserConfig::load` answers a truncated
/// file with `Default` — which silently forgets the checkout, the pinned
/// versions, and every Claude account. Same reasoning as `archive/staging.rs`,
/// and the same requirement that the temporary share a directory with its
/// target so the rename is atomic rather than a copy.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("could not create {}", parent.display()))?;
    }

    let temp = temp_beside(path);
    let written = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(bytes).await?;
        // Durable before the rename, so a power loss cannot leave the new name
        // pointing at unwritten blocks.
        file.sync_all().await
    }
    .await;

    if let Err(error) = written {
        // Best effort: the error being returned says more than this could.
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error).with_context(|| format!("could not write {}", temp.display()));
    }

    tokio::fs::rename(&temp, path)
        .await
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

/// `…/.state.json.4171-3.tmp`, in the target's own directory.
///
/// The counter is not decoration, for the same reason `archive/staging.rs`
/// carries one: keyed on the pid alone, two writes to one path from a single
/// process would compute the same temporary and interleave inside it.
fn temp_beside(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let call = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}-{call}.tmp", std::process::id()))
}
```

- [ ] **Step 4: Run the tests and watch them pass**

```sh
cd riabuild-cli && cargo test config::
```

Expected: all pass, including the two new ones.

- [ ] **Step 5: Commit**

```sh
git add riabuild-cli/src/config.rs
git commit -m "Land every state write by rename, never by truncate"
```

---

### Task 3: `State::update`, and the engine on top of it

**Files:**
- Modify: `riabuild-cli/src/config.rs` (add `State::update`, remove `State::save`)
- Modify: `riabuild-cli/src/tasks/mod.rs` (add `Ctx::update_state`)
- Modify: `riabuild-cli/src/tasks/engine.rs:155-162`
- Modify: `riabuild-cli/src/main.rs:396-398` (`logout`)
- Test: inline modules in `config.rs` and `tasks/engine.rs`

**Interfaces:**
- Consumes: `filelock::FileLock::acquire`, `Paths::state_lock_file` (Task 1); `config::write_json` (Task 2).
- Produces:
  - `State::update(paths: &dyn Paths, mutate: impl FnOnce(&mut State)) -> Result<State>`
  - `Ctx::update_state(&mut self, mutate: impl FnOnce(&mut State)) -> Result<()>`
  - `State::save` no longer exists.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `riabuild-cli/src/config.rs`:

```rust
    #[tokio::test]
    async fn concurrent_state_updates_do_not_lose_each_other() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.expect("mkdir");

        let mut writers = Vec::new();
        for n in 0..8 {
            let paths = crate::paths::RealPaths::rooted_at(home.path());
            writers.push(tokio::spawn(async move {
                State::update(&paths, |state| {
                    state.mark_satisfied(&format!("task_{n}"), 1, "never_run");
                })
                .await
                .expect("update");
            }));
        }
        for writer in writers {
            writer.await.expect("join");
        }

        let final_state = State::load(&paths).await;
        assert_eq!(
            final_state.tasks.len(),
            8,
            "every concurrent update must survive; got {:?}",
            final_state.tasks.keys().collect::<Vec<_>>()
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

```sh
cd riabuild-cli && cargo test concurrent_state_updates_do_not_lose_each_other
```

Expected: FAIL, `no method named 'update' found`.

- [ ] **Step 3: Add `State::update` and delete `State::save`**

In `riabuild-cli/src/config.rs`, replace the `save` method inside `impl State`:

```rust
    /// Acquires the lock, reads what is on disk *now*, applies `mutate`, and
    /// writes the result atomically.
    ///
    /// The read is inside the lock on purpose. Loading at process start and
    /// writing back much later is what made two riabuilds clobber each other:
    /// the later writer wins with a snapshot from whenever it began. With the
    /// read here there is no stale snapshot and so nothing to merge.
    ///
    /// There is no `save`. A method that writes without taking the lock is one
    /// a later change reaches for, and the lost update it reintroduces looks
    /// exactly like the bug this replaced.
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self> {
        // Contention here is measured in milliseconds, so a wait is not worth
        // a line on the developer's terminal.
        let _lock = crate::filelock::FileLock::acquire(&paths.state_lock_file(), || {}).await?;
        let mut state = Self::load(paths).await;
        mutate(&mut state);
        write_json(&paths.state_file(), &state).await?;
        Ok(state)
    }
```

- [ ] **Step 4: Add `Ctx::update_state`**

In `riabuild-cli/src/tasks/mod.rs`, inside `impl Ctx`:

```rust
    /// Applies `mutate` to the state on disk under the lock, and refreshes this
    /// run's copy from what was actually written.
    pub async fn update_state(&mut self, mutate: impl FnOnce(&mut State)) -> Result<()> {
        self.state = State::update(self.paths.as_ref(), mutate).await?;
        Ok(())
    }
```

- [ ] **Step 5: Convert the engine**

In `riabuild-cli/src/tasks/engine.rs`, replace the `mark_satisfied`-then-`save` pair (currently lines 155-157):

```rust
        ctx.update_state(|state| {
            state.mark_satisfied(task.id(), task.version(), &reason.tag())
        })
        .await?;
```

And replace the trailing save (currently line 162):

```rust
    // Not redundant, and not a no-op write: `State::load` drops records for
    // tasks riabuild no longer has, and this is the write that makes that
    // dropping stick. `a_dropped_record_does_not_come_back_on_the_next_save`
    // in `config.rs` is the test that fails if this goes.
    ctx.update_state(|_| {}).await?;
```

- [ ] **Step 6: Convert `logout`**

In `riabuild-cli/src/main.rs`, replace lines 397-398:

```rust
    ctx.update_state(|state| state.forget("login")).await?;
```

- [ ] **Step 7: Build, and fix every remaining `state.save` call site the compiler names**

```sh
cd riabuild-cli && cargo build 2>&1 | grep -E "^error" -A 5
```

Expected: errors only where `State::save` was called. Convert each to `update`. In `config.rs`'s own test module, `state.save(&paths)` becomes `State::update(&paths, |s| *s = state.clone())` — or rewrite the test to build its value inside the closure, which reads better.

- [ ] **Step 8: Run the tests and watch them pass**

```sh
cd riabuild-cli && cargo test
```

Expected: all pass, including `a_dropped_record_does_not_come_back_on_the_next_save` and the new concurrency test.

- [ ] **Step 9: Commit**

```sh
git add riabuild-cli/src
git commit -m "Read state under the lock, so two runs cannot clobber each other"
```

---

### Task 4: `UserConfig::update`, and the account registry on top of it

This is the task that fixes real data loss. `config.json` is the source of truth for the checkout path, the pinned toolchain versions, and the ordered account list — not a cache.

**Files:**
- Modify: `riabuild-cli/src/config.rs` (add `UserConfig::update`, remove `UserConfig::save`)
- Modify: `riabuild-cli/src/tasks/mod.rs` (add `Ctx::update_config`)
- Modify: `riabuild-cli/src/main.rs:322`, `riabuild-cli/src/main.rs:396`
- Modify: `riabuild-cli/src/move_project.rs:92-93`
- Modify: `riabuild-cli/src/accounts/command.rs:74`, `:151`, `:255`, `:297`
- Test: inline module in `riabuild-cli/src/config.rs`

**Interfaces:**
- Consumes: `FileLock::acquire`, `Paths::state_lock_file`, `config::write_json`.
- Produces:
  - `UserConfig::update(paths: &dyn Paths, mutate: impl FnOnce(&mut UserConfig)) -> Result<UserConfig>`
  - `Ctx::update_config(&mut self, mutate: impl FnOnce(&mut UserConfig)) -> Result<()>`
  - `UserConfig::save` no longer exists.

- [ ] **Step 1: Write the failing test — the account-loss regression**

Add to `#[cfg(test)] mod tests` in `riabuild-cli/src/config.rs`:

```rust
    /// Two `riabuild claude new` runs in two terminals. Before the lock, the
    /// later writer's snapshot did not contain the earlier writer's account, so
    /// one UUID vanished from the registry while its directory stayed on disk —
    /// and because position *is* the account number, adopting that orphan later
    /// renumbers whatever `claude-2` points at.
    #[tokio::test]
    async fn concurrent_account_additions_do_not_lose_an_account() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.expect("mkdir");

        let mut writers = Vec::new();
        for n in 0..8 {
            let paths = crate::paths::RealPaths::rooted_at(home.path());
            writers.push(tokio::spawn(async move {
                UserConfig::update(&paths, |config| {
                    config.claude_accounts.push(format!("account-{n}"));
                })
                .await
                .expect("update");
            }));
        }
        for writer in writers {
            writer.await.expect("join");
        }

        let config = UserConfig::load(&paths).await;
        let mut found = config.claude_accounts.clone();
        found.sort();
        let expected: Vec<String> = (0..8).map(|n| format!("account-{n}")).collect();
        assert_eq!(found, expected, "an account was lost between two windows");
    }

    #[tokio::test]
    async fn an_update_returns_what_it_wrote() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.expect("mkdir");

        let written = UserConfig::update(&paths, |config| {
            config.project_path = Some("/srv/checkout".into());
        })
        .await
        .expect("update");

        assert_eq!(written.project_path.as_deref(), Some("/srv/checkout"));
        assert_eq!(
            UserConfig::load(&paths).await.project_path.as_deref(),
            Some("/srv/checkout"),
            "what was returned must be what landed on disk"
        );
    }
```

- [ ] **Step 2: Run them and watch them fail**

```sh
cd riabuild-cli && cargo test concurrent_account_additions_do_not_lose_an_account an_update_returns_what_it_wrote
```

Expected: FAIL, `no method named 'update' found`.

- [ ] **Step 3: Add `UserConfig::update` and delete `UserConfig::save`**

In `riabuild-cli/src/config.rs`, replace the `save` method inside `impl UserConfig`:

```rust
    /// Acquires the lock, reads what is on disk *now*, applies `mutate`, and
    /// writes the result atomically. See `State::update` for why the read is
    /// inside the lock and why there is no `save`.
    ///
    /// This one matters more than `State`'s. State is a cache and a lost record
    /// costs one redundant `check()`; `config.json` is where the checkout path,
    /// the pinned versions and the account list live, and a lost update there
    /// is data a developer has to notice and re-enter.
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self> {
        let _lock = crate::filelock::FileLock::acquire(&paths.state_lock_file(), || {}).await?;
        let mut config = Self::load(paths).await;
        mutate(&mut config);
        write_json(&paths.config_file(), &config).await?;
        Ok(config)
    }
```

- [ ] **Step 4: Add `Ctx::update_config`**

In `riabuild-cli/src/tasks/mod.rs`, inside `impl Ctx`, beside `update_state`:

```rust
    /// Applies `mutate` to the config on disk under the lock, and refreshes this
    /// run's copy from what was actually written.
    pub async fn update_config(&mut self, mutate: impl FnOnce(&mut UserConfig)) -> Result<()> {
        self.config = UserConfig::update(self.paths.as_ref(), mutate).await?;
        Ok(())
    }
```

- [ ] **Step 5: Convert `remember_project` in `main.rs`**

Replace lines 320-322:

```rust
    let expanded = expand_tilde(project, &ctx.paths.home());
    ctx.update_config(|config| {
        config.project_path = Some(expanded.to_string_lossy().into_owned());
    })
    .await
```

- [ ] **Step 6: Convert `logout` in `main.rs`**

Replace line 396:

```rust
    ctx.update_config(|config| config.session_expires_at = None)
        .await?;
```

- [ ] **Step 7: Convert `move_project.rs`**

Replace lines 92-93:

```rust
    let destination = to.to_string_lossy().into_owned();
    ctx.update_config(|config| config.project_path = Some(destination))
        .await?;
```

- [ ] **Step 8: Convert the four `accounts/command.rs` sites**

Line 74, in `new` — the mutation already happened via `accounts::add` into `ctx.config`, so this one persists the in-memory list rather than computing a fresh one. Take the value first, then write it inside the closure:

```rust
    let accounts = ctx.config.claude_accounts.clone();
    ctx.update_config(|config| config.claude_accounts = accounts)
        .await?;
```

Apply the same shape at line 151 (`roll_back`), line 255 (`delete`) and line 297 (`primary`): clone `ctx.config.claude_accounts` after the local mutation, then assign it inside the closure.

> **Why this shape and not a closure that re-derives the change:** `accounts::add` and `accounts::remove` own the cap rule and the numbering, and they operate on `&mut UserConfig`. Re-running them inside the closure against freshly-loaded state would be *more* correct in principle, but they also return the number that the surrounding code has already printed and branched on. Assigning the resulting list keeps one source of truth for that number. The lock still makes the read-modify-write atomic; what is written is simply computed just above it.

- [ ] **Step 9: Build and fix everything the compiler names**

```sh
cd riabuild-cli && cargo build 2>&1 | grep -E "^error" -A 5
```

Convert every remaining `config.save` call site, including any in test modules.

- [ ] **Step 10: Run the full suite**

```sh
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

Expected: green.

- [ ] **Step 11: Commit**

```sh
git add riabuild-cli/src
git commit -m "Read config under the lock, so a second window cannot drop an account"
```

---

### Task 5: `remote::Store::update`

**Files:**
- Modify: `riabuild-cli/src/remote/store.rs:69-70` (add `update`, remove `save`), `:278`
- Modify: `riabuild-cli/src/remote/session.rs:234`
- Modify: `riabuild-cli/src/remote/forget.rs:132`
- Modify: `riabuild-cli/src/remote/flow/connect.rs:106`, `:132`
- Test: inline module in `riabuild-cli/src/remote/store.rs`

**Interfaces:**
- Consumes: `FileLock::acquire`, `Paths::state_lock_file`, `config::write_json`.
- Produces: `Store::update(paths: &dyn Paths, mutate: impl FnOnce(&mut Store)) -> Result<Store>`. `Store::save` no longer exists.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `riabuild-cli/src/remote/store.rs`:

```rust
    #[tokio::test]
    async fn concurrent_server_additions_do_not_lose_a_server() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.expect("mkdir");

        let mut writers = Vec::new();
        for n in 0..6 {
            let paths = crate::paths::RealPaths::rooted_at(home.path());
            writers.push(tokio::spawn(async move {
                Store::update(&paths, |store| {
                    let mut record = record_for(&remote());
                    record.name = format!("build-{n:02}");
                    store.remotes.push(record);
                })
                .await
                .expect("update");
            }));
        }
        for writer in writers {
            writer.await.expect("join");
        }

        assert_eq!(
            Store::load(&paths).await.remotes.len(),
            6,
            "a saved server was lost between two windows"
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

```sh
cd riabuild-cli && cargo test concurrent_server_additions_do_not_lose_a_server
```

Expected: FAIL, `no method named 'update' found`.

- [ ] **Step 3: Add `Store::update` and delete `Store::save`**

In `riabuild-cli/src/remote/store.rs`, replace the `save` method:

```rust
    /// Acquires the lock, reads what is on disk *now*, applies `mutate`, and
    /// writes the result atomically. See `config::State::update`.
    ///
    /// Shares `state_lock_file` with the other two: contention is milliseconds,
    /// and one lock for all three removes any question of lock ordering.
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self> {
        let _lock = crate::filelock::FileLock::acquire(&paths.state_lock_file(), || {}).await?;
        let mut store = Self::load(paths).await;
        mutate(&mut store);
        crate::config::write_json(&paths.remotes_file(), &store).await?;
        Ok(store)
    }
```

- [ ] **Step 4: Convert the five call sites**

Each currently mutates a `&mut Store` and then calls `save`. Convert each to compute the mutation inside the closure where it is self-contained, or — where the surrounding code needs the mutated value — mutate locally and assign `store.remotes` inside the closure, exactly as Task 4 Step 8 does for accounts. Run the build to find them:

```sh
cd riabuild-cli && cargo build 2>&1 | grep -E "^error" -A 5
```

- [ ] **Step 5: Run the full suite**

```sh
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

- [ ] **Step 6: Commit**

```sh
git add riabuild-cli/src/remote
git commit -m "Read the server store under the lock too"
```

---

### Task 6: Atomic launcher writes

**Files:**
- Modify: `riabuild-cli/src/shims/mod.rs:134`, `:175`, `:215`, `:241`
- Test: inline module in `riabuild-cli/src/shims/mod.rs`

**Interfaces:**
- Consumes: `config::write_atomic` (Task 2).
- Produces: nothing new.

No lock is involved. Launcher content is deterministic given the account list, so concurrent writers agree — the hazard is only an interrupt landing mid-write and leaving a truncated `claude-2` that fails with a shell syntax error.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `riabuild-cli/src/shims/mod.rs`:

```rust
    #[tokio::test]
    async fn a_launcher_write_leaves_no_temporary_behind() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let path = home.path().join("claude-1");

        write_launcher(&path, "#!/bin/sh\nexec claude \"$@\"\n")
            .await
            .expect("write");

        let mut entries = tokio::fs::read_dir(home.path()).await.expect("read_dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, vec!["claude-1".to_string()]);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("read"),
            "#!/bin/sh\nexec claude \"$@\"\n"
        );
    }
```

- [ ] **Step 2: Run it and watch it pass**

```sh
cd riabuild-cli && cargo test a_launcher_write_leaves_no_temporary_behind
```

Expected: PASS — nothing writes a temporary yet. This test is a guard against the *next* change, not a red-to-green step. Keep it: it is what fails if someone reintroduces a temporary that is not renamed away.

- [ ] **Step 3: Swap the four writes**

In `riabuild-cli/src/shims/mod.rs`, replace each `tokio::fs::write(&path, script).await?` (lines 134, 175, 215, 241) with:

```rust
    crate::config::write_atomic(&path, script.as_bytes()).await?;
```

At line 134 the variable is `shim` and the content is `exec_shim(binary)`:

```rust
    crate::config::write_atomic(&shim, exec_shim(binary).as_bytes()).await?;
```

`make_executable` still runs after each, unchanged — `rename` preserves the temporary's mode, and the chmod that follows sets the mode the launcher actually needs.

- [ ] **Step 4: Run the suite**

```sh
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

Expected: green, including the existing shim tests that assert launcher content.

- [ ] **Step 5: Commit**

```sh
git add riabuild-cli/src/shims/mod.rs
git commit -m "Land the launchers by rename, so an interrupt cannot truncate one"
```

---

### Task 7: The provisioning lock

**Files:**
- Modify: `riabuild-cli/src/provision.rs:41-56`
- Test: inline module in `riabuild-cli/src/provision.rs`

**Interfaces:**
- Consumes: `FileLock::acquire`, `Paths::provision_lock_file` (Task 1).
- Produces: `provision::provisioning_lock(ctx: &Ctx) -> Result<Option<FileLock>>` — private to the module, exercised directly by tests.

> **Why a helper rather than testing `provision` itself:** `provision` calls `connect`,
> then `engine::run_all` over the entire real task registry. The existing tests in this
> file never call it — they test `write_launchers_with` directly — and that is the
> convention to follow. Extracting the acquisition makes the two properties that matter
> (dry runs take nothing; the guard is droppable) testable without standing up a machine.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `riabuild-cli/src/provision.rs` (the module already
imports `FakeRunner` and `ctx_with`):

```rust
    /// `--check` changes nothing, so it must never make a second window wait.
    #[tokio::test]
    async fn a_dry_run_takes_no_provisioning_lock() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.dry_run = true;

        let taken = provisioning_lock(&ctx).await.expect("dry run");

        assert!(
            taken.is_none(),
            "a run that promises to change nothing must not hold the provisioning lock"
        );
    }

    #[tokio::test]
    async fn a_real_run_takes_the_lock_and_releases_it_when_dropped() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;

        let taken = provisioning_lock(&ctx).await.expect("acquire");
        assert!(taken.is_some(), "a real run holds the lock across its tasks");
        drop(taken);

        // Dropping is what `provision` does before the shell handoff, so a
        // second window must find the lock free immediately afterwards.
        let waited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&waited);
        let _second = crate::filelock::FileLock::acquire(&ctx.paths.provision_lock_file(), move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst)
        })
        .await
        .expect("second acquire");

        assert!(
            !waited.load(std::sync::atomic::Ordering::SeqCst),
            "the lock was still held after the guard was dropped"
        );
    }
```

- [ ] **Step 2: Run them and watch them fail**

```sh
cd riabuild-cli && cargo test provision::tests
```

Expected: both FAIL with `cannot find function 'provisioning_lock'`.

- [ ] **Step 3: Add the helper**

In `riabuild-cli/src/provision.rs`, above `provision`:

```rust
/// The lock a provisioning run holds across its tasks, or `None` under `--check`.
///
/// Two runs would otherwise both find node missing and both download it —
/// roughly 130 MB per lost race, into a directory nothing sweeps.
///
/// Not taken under `--check`, which writes nothing and must never make another
/// window wait. Not machine-wide either: the path comes from `root()`, which is
/// namespaced per developer on a server, so a shared lock would let one
/// developer block another under one Unix account — a denial of service wearing
/// robustness as a disguise.
async fn provisioning_lock(ctx: &Ctx) -> Result<Option<crate::filelock::FileLock>> {
    if ctx.dry_run {
        return Ok(None);
    }
    let path = ctx.paths.provision_lock_file();
    // The callback borrows the Ui rather than owning it — `Ui` is not `Clone`,
    // and it does not need to be for a message printed before the wait.
    let lock = crate::filelock::FileLock::acquire(&path, || {
        ctx.ui
            .info("Waiting for the riabuild already setting up this machine…");
    })
    .await
    .map_err(|error| {
        ui::Failure::new(
            "waiting for another riabuild to finish",
            "close the other riabuild, or run this again once it has finished",
        )
        .detail(format!("{error:#}"))
    })?;
    Ok(Some(lock))
}
```

- [ ] **Step 4: Call it, and let go before the shell**

In `riabuild-cli/src/provision.rs`, immediately before the existing
`ctx.ui.heading("Checking this machine");` (currently line 41):

```rust
    // After the upgrade block above, because a `flock` survives `exec` and
    // `upgrade_and_reexec` replaces this process image: acquiring first would
    // carry the lock into the new process with no guard tracking it.
    let provisioning = provisioning_lock(ctx).await?;
```

And immediately after `log_run(ctx, &outcome).await;` (currently line 56):

```rust
    // Before every return below, and before `open_shell` above all: that call
    // awaits the developer's interactive shell for as long as their window
    // stays open, and a lock held there would make the second window wait on a
    // human rather than on a download.
    drop(provisioning);
```

- [ ] **Step 5: Run the tests and watch them pass**

```sh
cd riabuild-cli && cargo test provision::
```

Expected: both pass, along with the two existing shim tests in this module. If the
release test fails, `drop` is placed after a `return` — it must run on every path that
reaches the shell.

- [ ] **Step 6: Run the suite**

```sh
cd riabuild-cli && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

- [ ] **Step 7: Commit**

```sh
git add riabuild-cli/src/provision.rs
git commit -m "Serialise the provisioning phase, and let go before the shell"
```

---

### Task 8: Write the invariant down

A rule nobody can find is a rule the next change breaks.

**Files:**
- Modify: `riabuild-cli/CLAUDE.md` (add an invariant after "**No secrets in `~/.riabuild/`**")

- [ ] **Step 1: Add the invariant**

```markdown
**State is read under a lock, and written by rename.** `config.json`, `state.json` and
`remotes.json` are changed through `update`, never a `save` — there is no `save`. `update`
takes `~/.riabuild/.state.lock`, reads what is on disk *now*, applies the closure, and
lands the result with `rename` from a temporary beside it. The read is inside the lock
because riabuild can be running in two terminal windows: loading at process start and
writing back at save time means the later writer wins with a snapshot from whenever it
began, which silently drops a Claude account and orphans its directory. Adding a method
that writes without the lock reintroduces exactly that.

`engine::run_all` additionally holds `~/.riabuild/.provision.lock`, so two runs do not
install one toolchain twice. It is acquired after `update::upgrade_and_reexec` — a
`flock` survives `exec` — and dropped before the shell handoff, which awaits the
developer's terminal for hours. `--check` takes neither.

Both locks are `std::fs::File::try_lock`, then a reported wait, then a blocking `lock()`
on the blocking pool: cargo's sequence, and cargo's fallback of treating a filesystem that
cannot lock as success rather than refusing to provision on it. Design:
`../docs/superpowers/specs/2026-08-12-concurrent-runs-design.md`.
```

- [ ] **Step 2: Commit**

```sh
git add riabuild-cli/CLAUDE.md
git commit -m "Write down the locking invariant"
```

---

### Task 9: Open the pull request

- [ ] **Step 1: Final verification from a clean build**

```sh
cd riabuild-cli && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Expected: green. Do not proceed on a red suite.

- [ ] **Step 2: Push and open the PR**

```sh
git push -u origin worktree-fix+concurrent-run-safety
gh pr create --fill
```

- [ ] **Step 3: Watch CI to completion**

```sh
gh pr checks --watch
```

Per `CLAUDE.md`: work is not finished until PR CI has completed. If CI fails, fixing it is part of this task, not a follow-up.

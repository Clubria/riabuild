# Claude Code Account Management Implementation Plan

> **Completed — historical record, do not execute.** Shipped in #28, 2026-08-07. The
> unchecked `- [ ]` boxes below are how the plan was written and not work outstanding, and
> the instruction to an agentic worker to implement it task-by-task that stood here has
> been removed: acting on it would rebuild something that already ships. See
> [`README.md`](README.md) for the index, and the design spec for what the code does now.

**Goal:** Replace riabuild's single Claude Code profile with an ordered list of up to nine accounts, each with its own launcher command, all sharing the org's Claude settings and all trusting the checkout.

**Architecture:** `UserConfig.claude_accounts` is an ordered `Vec<String>` of UUID directory names — position *is* the number the developer types, so deleting account 3 renumbers 4 into 3 with no bookkeeping. A new `accounts/` module owns the registry, the concurrent identity lookup, the rendered box, and the `riabuild claude` subcommand. `shims/` generates `claude` plus `claude-1`…`claude-N`, each execing an absolute path with `--settings` layered.

**Tech Stack:** Rust 2024, tokio (current-thread), clap, serde/serde_json, async-trait, anyhow. Tests use the in-crate `FakeRunner` and `testing::ctx_with`.

**Spec:** `docs/superpowers/specs/2026-08-06-claude-accounts-design.md`. Read it before Task 1.

## Global Constraints

Every task's requirements implicitly include all of these. They come from `riabuild-cli/CLAUDE.md` and the spec.

- **All IO is async.** `tokio::fs`, never `std::fs`. `paths.rs` (pure), `CommandRunner::which` (stats `PATH`), and archive extraction are the only synchronous exceptions.
- **Every external process goes through `CommandRunner`.** No `std::process::Command` or `tokio::process` outside `runner.rs`.
- **`unwrap_used` is denied** outside `#[cfg(test)]`. Use `let else`, `match`, `unwrap_or`, or `?`.
- **Every reachable error is a `ui::Failure`** carrying what was attempted, the command, the detail, and one next action.
- **Closures cannot be async.** `unwrap_or_else`/`and_then` chains around IO must be unrolled into `match` or `let else`.
- **`apply()` must be safe to run twice**, and is always followed by a re-run of `check()`.
- **`check()` is authoritative**; `version()` is only for drift a check genuinely cannot observe.
- **One task per file; roughly 300 lines is the ceiling** for any file.
- **Claude Code version floor is `2.1.223`** (`MIN_VERSION`). Raised from `2.0.0` during
  execution: the three behaviours this feature rests on were only ever verified against
  that build, and `install_claude` is unpinned, so the floor upgrades rather than blocks.
- **The account cap is 9** (`accounts::MAX`).
- **Prompts belong in `apply()` or a subcommand, never in `check()`**, and every prompt has a default — `Ui::ask` returns `None` when there is no terminal.
- After every task: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- All work goes through a pull request; CI must pass before the work is done.

All paths below are relative to `riabuild-cli/` unless stated otherwise.

---

### Task 1: Config stores an ordered list of accounts

Behaviour is unchanged — one account — but every reader and writer now goes through the list, so nothing else has to change in lockstep later.

**Files:**
- Modify: `src/config.rs` (`UserConfig`, `load`)
- Modify: `src/tasks/claude_profiles.rs:130` (the writer)
- Modify: `src/tasks/claude_trust.rs:98,130` (readers)
- Modify: `src/shims/mod.rs:85` (reader)
- Modify: `src/shell/mod.rs:109` (reader)

**Interfaces:**
- Produces: `UserConfig.claude_accounts: Vec<String>`, `UserConfig::primary_account(&self) -> Option<&str>`. `UserConfig.claude_profile` survives as a load-only legacy field.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block at the bottom of `src/config.rs`:

```rust
    #[tokio::test]
    async fn a_legacy_profile_becomes_the_first_account() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(
            paths.config_file(),
            r#"{"claude_profile":"11111111-2222-4333-8444-555555555555"}"#,
        )
        .await
        .unwrap();

        let config = UserConfig::load(&paths).await;
        assert_eq!(
            config.claude_accounts,
            vec!["11111111-2222-4333-8444-555555555555".to_string()]
        );
        // Folded in on load, so nothing downstream ever sees the old field.
        assert_eq!(config.claude_profile, None);
        assert_eq!(
            config.primary_account(),
            Some("11111111-2222-4333-8444-555555555555")
        );
    }

    #[tokio::test]
    async fn an_account_list_wins_over_a_legacy_profile() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(
            paths.config_file(),
            r#"{"claude_profile":"aaaaaaaa-2222-4333-8444-555555555555",
                "claude_accounts":["bbbbbbbb-2222-4333-8444-555555555555"]}"#,
        )
        .await
        .unwrap();

        let config = UserConfig::load(&paths).await;
        assert_eq!(
            config.claude_accounts,
            vec!["bbbbbbbb-2222-4333-8444-555555555555".to_string()]
        );
    }

    #[tokio::test]
    async fn saving_drops_the_legacy_profile_from_the_file() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        let config = UserConfig {
            claude_accounts: vec!["11111111-2222-4333-8444-555555555555".into()],
            claude_profile: Some("11111111-2222-4333-8444-555555555555".into()),
            ..Default::default()
        };
        config.save(&paths).await.unwrap();

        let text = tokio::fs::read_to_string(paths.config_file()).await.unwrap();
        assert!(!text.contains("claude_profile"), "{text}");
        assert!(text.contains("claude_accounts"), "{text}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml config::`
Expected: FAIL — `no field 'claude_accounts' on type 'UserConfig'`.

- [ ] **Step 3: Add the field, the accessor, and the fold**

In `src/config.rs`, replace the `claude_profile` field in `UserConfig` with:

```rust
    /// Claude Code config directories, in the order the developer numbers them.
    ///
    /// Position *is* the number: account 3 is index 2, and removing it makes
    /// what was account 4 into account 3 with no renumbering code at all. The
    /// UUID is the only identity anything persists.
    #[serde(default)]
    pub claude_accounts: Vec<String>,
    /// The single profile older riabuilds recorded.
    ///
    /// Read on load and folded into `claude_accounts`, never written back —
    /// which is what `skip_serializing` is for. Keeping it means a developer
    /// who upgrades does not lose the account they are already signed in to.
    #[serde(default, skip_serializing)]
    pub claude_profile: Option<String>,
```

Add to `impl UserConfig`, and call the fold from `load`:

```rust
    /// The account `claude` runs.
    pub fn primary_account(&self) -> Option<&str> {
        self.claude_accounts.first().map(String::as_str)
    }

    /// Folds the single profile of an older riabuild into the account list.
    ///
    /// Takes the field rather than copying it, so no caller can read a value
    /// that will not be saved.
    fn fold_legacy_profile(&mut self) {
        // Taken unconditionally: a value that will not be saved must not be
        // readable either. `extend` over the Option keeps this one statement
        // rather than a nested `if`, which `clippy::collapsible_if` rejects.
        let legacy = self.claude_profile.take();
        if self.claude_accounts.is_empty() {
            self.claude_accounts.extend(legacy);
        }
    }
```

In `load`, bind the parsed config to a `mut` local, call `config.fold_legacy_profile()`, and return it instead of returning the parse result directly.

- [ ] **Step 4: Point every reader and writer at the list**

`src/tasks/claude_profiles.rs`, in `apply`, replace `ctx.config.claude_profile = Some(profile);` with:

```rust
        if !ctx.config.claude_accounts.contains(&profile) {
            ctx.config.claude_accounts.push(profile);
        }
```

`src/shims/mod.rs`, in `write_all`, replace the `let Some(profile) = &ctx.config.claude_profile else` line with:

```rust
    let Some(profile) = ctx.config.primary_account() else {
        return Ok(());
    };
```

`src/shell/mod.rs`, in `environment`, replace `if let Some(profile) = &ctx.config.claude_profile {` with `if let Some(profile) = ctx.config.primary_account() {`.

`src/tasks/claude_trust.rs`, in both `check` and `apply`, replace `ctx.config.claude_profile.clone()` with `ctx.config.primary_account().map(str::to_string)`.

In the `mod tests` of `claude_profiles.rs`, `claude_trust.rs`, `shims/mod.rs` and `shell/mod.rs`, replace every `ctx.config.claude_profile = Some(x)` with `ctx.config.claude_accounts = vec![x];`.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml`
Expected: PASS. Then `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all`.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/src
git commit -m "Store Claude Code accounts as an ordered list"
```

---

### Task 2: The account registry

**Files:**
- Create: `src/accounts/mod.rs`
- Modify: `src/main.rs` (add `mod accounts;` to the module list, alphabetically before `mod api;`)

**Interfaces:**
- Consumes: `UserConfig.claude_accounts` from Task 1.
- Produces: `accounts::MAX: usize`, `accounts::new_id() -> String`, `accounts::looks_like_id(&str) -> bool`, `accounts::id_of(&UserConfig, usize) -> Result<String>`, `accounts::add(&mut UserConfig, String) -> Result<usize>`, `accounts::remove(&mut UserConfig, usize) -> Result<String>`, `accounts::promote(&mut UserConfig, usize) -> Result<String>`.

- [ ] **Step 1: Write the failing tests**

Create `src/accounts/mod.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(count: usize) -> UserConfig {
        UserConfig {
            claude_accounts: (0..count).map(|n| format!("id-{n}")).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn deleting_an_account_renumbers_the_ones_after_it() {
        // The whole reason position is the number: this needs no renumbering
        // code, and so cannot have a renumbering bug.
        let mut config = config_with(5);
        assert_eq!(remove(&mut config, 3).unwrap(), "id-2");
        assert_eq!(
            config.claude_accounts,
            vec!["id-0", "id-1", "id-3", "id-4"]
        );
        assert_eq!(id_of(&config, 3).unwrap(), "id-3");
        assert_eq!(id_of(&config, 4).unwrap(), "id-4");
    }

    #[test]
    fn promoting_keeps_every_other_account_in_order() {
        let mut config = config_with(4);
        promote(&mut config, 3).unwrap();
        assert_eq!(
            config.claude_accounts,
            vec!["id-2", "id-0", "id-1", "id-3"]
        );
    }

    #[test]
    fn promoting_the_primary_changes_nothing() {
        let mut config = config_with(3);
        promote(&mut config, 1).unwrap();
        assert_eq!(config.claude_accounts, vec!["id-0", "id-1", "id-2"]);
    }

    #[test]
    fn a_tenth_account_is_refused() {
        let mut config = config_with(MAX);
        let error = add(&mut config, "one-too-many".into()).unwrap_err();
        assert!(error.to_string().contains("adding a Claude Code account"));
        assert_eq!(config.claude_accounts.len(), MAX);
    }

    #[test]
    fn a_number_nobody_has_is_refused_rather_than_wrapping() {
        let config = config_with(2);
        assert!(id_of(&config, 0).is_err());
        assert!(id_of(&config, 3).is_err());
        assert_eq!(id_of(&config, 2).unwrap(), "id-1");
    }

    #[test]
    fn generates_well_formed_ids() {
        let id = new_id();
        assert!(looks_like_id(&id), "{id}");
        assert_ne!(id, new_id());
        // Version 4, variant 1 — the bits a UUID library would set.
        assert_eq!(id.chars().nth(14), Some('4'));
        assert!(matches!(id.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
    }

    #[test]
    fn rejects_directories_that_are_not_accounts() {
        assert!(!looks_like_id("settings"));
        assert!(!looks_like_id("not-a-uuid"));
        assert!(!looks_like_id(""));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `mod accounts;` to `src/main.rs`, then run:
`cargo test --manifest-path riabuild-cli/Cargo.toml accounts::`
Expected: FAIL — `cannot find function 'remove' in this scope`.

- [ ] **Step 3: Write the registry**

Put this above the test module in `src/accounts/mod.rs`:

```rust
//! The developer's Claude Code accounts, in the order they are numbered.
//!
//! Position is the number: account 3 is `claude_accounts[2]`, and removing it
//! makes what was account 4 into account 3 without a line of renumbering code.
//! A design that stored the number would have an invariant to maintain on every
//! mutation, and would eventually fail to maintain it.
//!
//! Each account is a directory under `~/.riabuild/claude/<uuid>/` that Claude
//! Code is pointed at with `CLAUDE_CONFIG_DIR`. That variable scopes the login
//! as well as the settings — on macOS the keychain item is named for a hash of
//! the directory's path — so two accounts really are two independent sign-ins.

use crate::config::UserConfig;
use crate::ui::{Failure, plural};
use anyhow::Result;
use rand::RngCore;

/// Nine keeps every launcher name single-digit — `claude-1` … `claude-9` — and
/// makes `riabuild claude delete 12` an obvious mistake rather than something
/// to interpret.
pub const MAX: usize = 9;

/// A v4 UUID for an account directory name.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Whether a directory name is one riabuild would have created.
pub fn looks_like_id(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(expected, part)| part.len() == *expected)
        && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// The account a developer's number refers to.
///
/// `0` is not an account, and `wrapping_sub` turns it into an index nobody has
/// rather than into the last one.
pub fn id_of(config: &UserConfig, number: usize) -> Result<String> {
    match config.claude_accounts.get(number.wrapping_sub(1)) {
        Some(id) => Ok(id.clone()),
        None => Err(Failure::new(
            format!("finding Claude Code account {number}"),
            "Run `riabuild claude` to see the accounts you have.",
        )
        .detail(format!(
            "you have {}",
            plural(config.claude_accounts.len() as u64, "Claude Code account")
        ))
        .into()),
    }
}

/// Appends an account, refusing past `MAX`.
pub fn add(config: &mut UserConfig, id: String) -> Result<usize> {
    if config.claude_accounts.len() >= MAX {
        return Err(Failure::new(
            "adding a Claude Code account",
            "Delete one with `riabuild claude delete <number>` first.",
        )
        .detail(format!(
            "riabuild keeps at most {}, and you already have that many",
            plural(MAX as u64, "account")
        ))
        .into());
    }
    config.claude_accounts.push(id);
    Ok(config.claude_accounts.len())
}

/// Removes an account and returns its id.
///
/// Every later account shifts down one number. That is the feature, not a side
/// effect — see the module comment.
pub fn remove(config: &mut UserConfig, number: usize) -> Result<String> {
    let id = id_of(config, number)?;
    config.claude_accounts.remove(number - 1);
    Ok(id)
}

/// Makes an account the primary one, preserving the order of the rest.
pub fn promote(config: &mut UserConfig, number: usize) -> Result<String> {
    let id = remove(config, number)?;
    config.claude_accounts.insert(0, id.clone());
    Ok(id)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml accounts::`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/src
git commit -m "Add the Claude Code account registry"
```

---

### Task 3: FakeRunner matches on environment

Without this, "account 1 is signed in, account 2 is signed out" cannot be expressed: the command string is identical for every account and only `CLAUDE_CONFIG_DIR` differs.

**Files:**
- Modify: `src/runner.rs:170-258` (the `FakeRunner` block) and `:263-297` (the `CommandRunner` impl)

**Interfaces:**
- Produces: `FakeRunner::with_env(self, &str, &[(&str, &str)], i32, &str, &str) -> Self`. `FakeRunner::with` keeps its signature and matches any environment.

- [ ] **Step 1: Write the failing tests**

Add at the bottom of `src/runner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn in_dir(dir: &str) -> RunOptions {
        RunOptions {
            env: vec![("CLAUDE_CONFIG_DIR".to_string(), dir.to_string())],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_stub_can_be_scoped_to_an_environment_variable() {
        // The same command, twice, told apart only by the directory it is
        // pointed at — which is exactly how riabuild asks each Claude Code
        // account who it is.
        let runner = FakeRunner::new()
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/one")],
                0,
                r#"{"loggedIn":true,"email":"first@example.com"}"#,
                "",
            )
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/two")],
                1,
                r#"{"loggedIn":false}"#,
                "",
            );

        let one = runner
            .run("claude", &["auth", "status", "--json"], &in_dir("/one"))
            .await
            .unwrap();
        assert!(one.stdout.contains("first@example.com"), "{one:?}");

        let two = runner
            .run("claude", &["auth", "status", "--json"], &in_dir("/two"))
            .await
            .unwrap();
        assert_eq!(two.code, Some(1));
        assert!(two.stdout.contains("false"), "{two:?}");
    }

    #[tokio::test]
    async fn a_stub_with_no_environment_still_matches_anything() {
        let runner = FakeRunner::new().with("claude --version", 0, "2.1.223 (Claude Code)", "");
        let output = runner
            .run("claude", &["--version"], &in_dir("/anywhere"))
            .await
            .unwrap();
        assert!(output.ok(), "{output:?}");
    }

    #[tokio::test]
    async fn an_environment_stub_beats_a_general_one() {
        let runner = FakeRunner::new()
            .with("claude auth status --json", 1, r#"{"loggedIn":false}"#, "")
            .with_env(
                "claude auth status --json",
                &[("CLAUDE_CONFIG_DIR", "/one")],
                0,
                r#"{"loggedIn":true,"email":"first@example.com"}"#,
                "",
            );

        let one = runner
            .run("claude", &["auth", "status", "--json"], &in_dir("/one"))
            .await
            .unwrap();
        assert!(one.stdout.contains("first@example.com"), "{one:?}");

        let other = runner
            .run("claude", &["auth", "status", "--json"], &in_dir("/elsewhere"))
            .await
            .unwrap();
        assert_eq!(other.code, Some(1));
    }

    #[tokio::test]
    async fn a_later_stub_replaces_an_identical_earlier_one() {
        let runner = FakeRunner::new()
            .with("claude --version", 0, "2.0.0 (Claude Code)", "")
            .with("claude --version", 0, "2.1.223 (Claude Code)", "");
        let output = runner
            .run("claude", &["--version"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(output.trimmed(), "2.1.223 (Claude Code)");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml runner::`
Expected: FAIL — `no function or associated item named 'with_env'`.

- [ ] **Step 3: Replace the response map with a stub list**

In `src/runner.rs`, replace `use std::collections::HashMap;` (the `#[cfg(test)]` one) with nothing — it becomes unused — and change the struct:

```rust
#[cfg(test)]
/// One scripted response, and the conditions it answers to.
struct Stub {
    invocation: String,
    /// Environment entries that must all be present for this stub to apply.
    /// Empty means "any environment", which is what `with` produces.
    env: Vec<(String, String)>,
    output: CommandOutput,
}

#[cfg(test)]
/// Scripted `CommandRunner` for tests.
///
/// Keys are `"program arg1 arg2"` prefixes; the longest matching prefix wins, so
/// a test can stub `gh auth status` and `gh --version` independently.
///
/// A stub can also require environment entries. `claude auth status --json` is
/// the same command string for every Claude Code account — only
/// `CLAUDE_CONFIG_DIR` differs — so without this the central behaviour of the
/// account feature could not be written as a test at all.
#[derive(Default)]
pub struct FakeRunner {
    responses: Vec<Stub>,
    available: Vec<String>,
    pub calls: std::sync::Mutex<Vec<String>>,
}
```

Replace `with` and add `with_env`:

```rust
    pub fn with(self, invocation: &str, code: i32, stdout: &str, stderr: &str) -> Self {
        self.with_env(invocation, &[], code, stdout, stderr)
    }

    /// A stub that only answers when the environment carries every named pair.
    pub fn with_env(
        mut self,
        invocation: &str,
        env: &[(&str, &str)],
        code: i32,
        stdout: &str,
        stderr: &str,
    ) -> Self {
        self.responses.push(Stub {
            invocation: invocation.to_string(),
            env: env
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            output: CommandOutput {
                code: Some(code),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            },
        });
        let program = invocation.split_whitespace().next().unwrap_or_default();
        if !self.available.iter().any(|p| p == program) {
            self.available.push(program.to_string());
        }
        self
    }
```

Replace `stubbed`, and thread `options` through `resolve` and `lookup`:

```rust
    fn stubbed(&self, invocation: &str, options: &RunOptions) -> Option<CommandOutput> {
        self.responses
            .iter()
            .filter(|stub| {
                let name_matches = invocation == stub.invocation
                    || invocation.starts_with(&format!("{} ", stub.invocation));
                name_matches
                    && stub.env.iter().all(|(key, value)| {
                        options.env.iter().any(|(k, v)| k == key && v == value)
                    })
            })
            // Most specific wins: the longest command, then the most
            // environment entries. `max_by_key` keeps the last of equal
            // candidates, so a later identical stub replaces an earlier one —
            // which is what the map this replaced did.
            .max_by_key(|stub| (stub.invocation.len(), stub.env.len()))
            .map(|stub| stub.output.clone())
    }

    fn resolve(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Option<CommandOutput> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.stubbed(&full, options)
            .or_else(|| self.stubbed(&FakeRunner::stub_key(program, args), options))
    }

    fn lookup(&self, program: &str, args: &[&str], options: &RunOptions) -> CommandOutput {
        self.resolve(program, args, options)
            .unwrap_or_else(|| CommandOutput {
                code: Some(127),
                stdout: String::new(),
                stderr: format!(
                    "fake runner: no stub for `{}`",
                    FakeRunner::stub_key(program, args)
                ),
            })
    }
```

In the `CommandRunner for FakeRunner` impl, rename `_options` to `options` in both methods, and pass it: `Ok(self.lookup(program, args, options))` and `self.resolve(program, args, options)`.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml`
Expected: PASS — the four new tests plus every existing test, because `with` now delegates to `with_env` with no requirements.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/src/runner.rs
git commit -m "Let FakeRunner tell two invocations apart by environment"
```

---

### Task 4: `Ctx::claude()` — the binary by absolute path

**Files:**
- Modify: `src/tasks/mod.rs` (add the method to `impl Ctx`, and a test module)

**Interfaces:**
- Produces: `Ctx::claude(&self) -> String`.

- [ ] **Step 1: Write the failing tests**

Add at the bottom of `src/tasks/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;

    #[tokio::test]
    async fn claude_is_the_one_riabuilds_node_installed() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.node_version = Some("22.23.1".into());
        let claude = ctx.claude();
        assert!(claude.ends_with("/node/22.23.1/bin/claude"), "{claude}");
        assert!(claude.starts_with(&ctx.paths.root().to_string_lossy().into_owned()));
    }

    #[tokio::test]
    async fn without_a_pinned_node_the_bare_name_is_all_there_is() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(ctx.claude(), "claude");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml tasks::tests`
Expected: FAIL — `no method named 'claude' found`.

- [ ] **Step 3: Add the accessor**

In `src/tasks/mod.rs`, add to `impl Ctx` next to `gh()` and `infisical()`:

```rust
    /// The Claude Code riabuild installed, by absolute path.
    ///
    /// Same reasoning as `gh()`, with one addition: `which("claude")` reads the
    /// ambient `PATH`, which during provisioning does not contain riabuild's
    /// Node — so it finds whatever the developer happens to have installed, or
    /// nothing at all in the moment just after riabuild installed one. Claude
    /// Code is installed by riabuild's own npm, so its home is the pinned
    /// Node's `bin`.
    ///
    /// Falls back to the bare name before a Node is pinned, which is the only
    /// thing a machine with no toolchain yet could use.
    pub fn claude(&self) -> String {
        match &self.config.node_version {
            Some(version) => self
                .paths
                .node_dir(version)
                .join("bin")
                .join("claude")
                .to_string_lossy()
                .into_owned(),
            None => "claude".to_string(),
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml tasks::tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/src/tasks/mod.rs
git commit -m "Run Claude Code by absolute path, like gh and infisical"
```

---

### Task 5: Ask every account who it is, concurrently

**Files:**
- Create: `src/accounts/status.rs`
- Modify: `src/accounts/mod.rs` (add `pub mod status;` at the top)

**Interfaces:**
- Consumes: `Ctx::claude()` (Task 4), `FakeRunner::with_env` (Task 3), `UserConfig.claude_accounts` (Task 1).
- Produces: `accounts::status::Identity` (`LoggedIn(String)` | `LoggedOut` | `Unknown(String)`), `accounts::status::Account { number: usize, id: String, identity: Identity }`, `accounts::status::read_all(&Ctx) -> Vec<Account>`, `accounts::status::read(&Ctx, &str) -> Identity`.

- [ ] **Step 1: Write the failing tests**

Create `src/accounts/status.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
    use std::sync::Arc;

    #[tokio::test]
    async fn accounts_are_told_apart_by_their_config_directory() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let one = accounts::new_id();
        let two = accounts::new_id();
        ctx.config.claude_accounts = vec![one.clone(), two.clone()];

        let first_dir = ctx
            .paths
            .claude_profile_dir(&one)
            .to_string_lossy()
            .into_owned();
        let second_dir = ctx
            .paths
            .claude_profile_dir(&two)
            .to_string_lossy()
            .into_owned();
        ctx.runner = Arc::new(
            FakeRunner::new()
                .with_env(
                    "claude auth status --json",
                    &[("CLAUDE_CONFIG_DIR", &first_dir)],
                    0,
                    r#"{"loggedIn":true,"authMethod":"claude.ai","email":"first@example.com"}"#,
                    "",
                )
                .with_env(
                    "claude auth status --json",
                    &[("CLAUDE_CONFIG_DIR", &second_dir)],
                    1,
                    r#"{"loggedIn":false,"authMethod":"none"}"#,
                    "",
                ),
        );

        let found = read_all(&ctx).await;
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].number, 1);
        assert_eq!(found[0].id, one);
        assert_eq!(
            found[0].identity,
            Identity::LoggedIn("first@example.com".into())
        );
        assert_eq!(found[1].number, 2);
        assert_eq!(found[1].identity, Identity::LoggedOut);
    }

    #[tokio::test]
    async fn an_answer_that_will_not_parse_is_not_reported_as_signed_out() {
        // The distinction that matters: riabuild not knowing must never render
        // as "(logged out)", which is a claim about the account.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id()];

        let found = read_all(&ctx).await;
        assert!(
            matches!(found[0].identity, Identity::Unknown(_)),
            "{:?}",
            found[0].identity
        );
    }

    #[tokio::test]
    async fn an_answer_with_no_email_is_not_a_sign_in() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let id = accounts::new_id();
        ctx.config.claude_accounts = vec![id.clone()];
        ctx.runner = Arc::new(FakeRunner::new().with(
            "claude auth status --json",
            0,
            r#"{"loggedIn":true}"#,
            "",
        ));

        assert!(matches!(read(&ctx, &id).await, Identity::Unknown(_)));
    }

    #[tokio::test]
    async fn no_accounts_means_no_subprocesses() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert!(read_all(&ctx).await.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod status;` to the top of `src/accounts/mod.rs`, then run:
`cargo test --manifest-path riabuild-cli/Cargo.toml accounts::status`
Expected: FAIL — `cannot find function 'read_all' in this scope`.

- [ ] **Step 3: Write the lookup**

Put this above the test module in `src/accounts/status.rs`:

```rust
//! Who each Claude Code account is signed in as.
//!
//! Asked of Claude Code rather than read off disk: `claude auth status --json`
//! is a supported command that reports `loggedIn` and `email` for whatever
//! `CLAUDE_CONFIG_DIR` points at. The alternative — parsing `oauthAccount` out
//! of `.claude.json` — reads a key nothing promises to keep.
//!
//! The call costs about 450 ms, almost all of it the child process's own
//! startup, so the accounts are asked all at once. The runtime is
//! current-thread and that is fine: every task is blocked awaiting a subprocess,
//! so the children run concurrently even though riabuild's own work does not.

use crate::runner::{CommandRunner, RunOptions};
use crate::tasks::Ctx;
use serde_json::Value;
use std::path::Path;
use tokio::task::JoinSet;

/// What Claude Code says about one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    LoggedIn(String),
    LoggedOut,
    /// riabuild could not tell, and says so rather than guessing. Rendering
    /// this as "logged out" would assert something about the account that
    /// nothing established.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// 1-based, derived from position in the list. Never stored.
    pub number: usize,
    pub id: String,
    pub identity: Identity,
}

/// Every account, asked at the same time.
pub async fn read_all(ctx: &Ctx) -> Vec<Account> {
    let mut asking = JoinSet::new();
    for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
        let runner = ctx.runner.clone();
        let claude = ctx.claude();
        let dir = ctx.paths.claude_profile_dir(id);
        let id = id.clone();
        asking.spawn(async move {
            let identity = ask(runner.as_ref(), &claude, &dir).await;
            Account {
                number: index + 1,
                id,
                identity,
            }
        });
    }

    let mut found = Vec::new();
    while let Some(joined) = asking.join_next().await {
        match joined {
            Ok(account) => found.push(account),
            // A panicked lookup must not take the whole box with it.
            Err(_) => continue,
        }
    }
    // Tasks finish in whatever order the children do; the developer numbers
    // them in one fixed order.
    found.sort_by_key(|account| account.number);
    found
}

/// One account, by id.
pub async fn read(ctx: &Ctx, id: &str) -> Identity {
    ask(
        ctx.runner.as_ref(),
        &ctx.claude(),
        &ctx.paths.claude_profile_dir(id),
    )
    .await
}

/// The exit code is deliberately not consulted.
///
/// It says the same thing as `loggedIn` today, but an exit code is one channel
/// that a future release could also use for "could not reach the server" — and
/// reporting that as a signed-out account would be a lie riabuild told itself.
async fn ask(runner: &dyn CommandRunner, claude: &str, dir: &Path) -> Identity {
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };

    let output = match runner
        .run(claude, &["auth", "status", "--json"], &options)
        .await
    {
        Ok(output) => output,
        Err(error) => return Identity::Unknown(format!("{error:#}")),
    };

    let Ok(value) = serde_json::from_str::<Value>(&output.stdout) else {
        return Identity::Unknown("Claude Code did not answer in JSON".to_string());
    };
    match value.get("loggedIn") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => return Identity::LoggedOut,
        // Absent, or not a bool: riabuild does not know, and must not spend
        // that ignorance as a claim that the account is signed out.
        _ => return Identity::Unknown("Claude Code reported no sign-in state".to_string()),
    }
    match value.get("email").and_then(Value::as_str) {
        Some(email) => Identity::LoggedIn(email.to_string()),
        None => Identity::Unknown("Claude Code reported no email".to_string()),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml accounts::status`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/src/accounts
git commit -m "Ask every Claude Code account who it is, all at once"
```

---

### Task 6: Render the account box

**Files:**
- Create: `src/accounts/render.rs`
- Modify: `src/accounts/mod.rs` (add `pub mod render;`)

**Interfaces:**
- Consumes: `Account`, `Identity` (Task 5), `accounts::MAX` (Task 2).
- Produces: `accounts::render::accounts_box(&[Account], bool) -> String`.

- [ ] **Step 1: Write the failing tests**

Create `src/accounts/render.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn account(number: usize, identity: Identity) -> Account {
        Account {
            number,
            id: format!("id-{number}"),
            identity,
        }
    }

    fn three() -> Vec<Account> {
        vec![
            account(1, Identity::LoggedIn("clubria@proton.me".into())),
            account(2, Identity::LoggedIn("other@gmail.com".into())),
            account(3, Identity::LoggedOut),
        ]
    }

    #[test]
    fn every_account_is_listed_with_the_command_that_runs_it() {
        let text = accounts_box(&three(), false);
        assert!(text.contains("Your Claude Code accounts:"), "{text}");
        // The primary carries both names, because both work.
        assert!(text.contains("1. claude-1 / claude   clubria@proton.me"), "{text}");
        assert!(text.contains("2. claude-2            other@gmail.com"), "{text}");
        assert!(text.contains("3. claude-3            (logged out)"), "{text}");
    }

    #[test]
    fn only_commands_that_would_work_are_offered() {
        let text = accounts_box(&three(), false);
        assert!(text.contains("Add an account:     riabuild claude new"), "{text}");
        assert!(text.contains("Delete an account:  riabuild claude delete 3"), "{text}");
        assert!(text.contains("Make it primary:    riabuild claude primary 2"), "{text}");
        assert!(text.contains("Log in:             claude-3 auth login"), "{text}");
    }

    #[test]
    fn a_single_account_is_offered_neither_delete_nor_primary() {
        // Both refuse or do nothing with one account, and a hint that fails is
        // worse than no hint.
        let one = vec![account(1, Identity::LoggedIn("clubria@proton.me".into()))];
        let text = accounts_box(&one, false);
        assert!(text.contains("riabuild claude new"), "{text}");
        assert!(!text.contains("delete"), "{text}");
        assert!(!text.contains("primary"), "{text}");
    }

    #[test]
    fn a_fully_signed_in_list_is_not_told_how_to_log_in() {
        let signed_in = vec![
            account(1, Identity::LoggedIn("a@example.com".into())),
            account(2, Identity::LoggedIn("b@example.com".into())),
        ];
        assert!(!accounts_box(&signed_in, false).contains("auth login"));
    }

    #[test]
    fn a_full_list_is_not_offered_another_account() {
        let full: Vec<Account> = (1..=MAX)
            .map(|number| account(number, Identity::LoggedIn(format!("{number}@example.com"))))
            .collect();
        assert!(!accounts_box(&full, false).contains("riabuild claude new"));
    }

    #[test]
    fn not_knowing_is_said_out_loud() {
        let unsure = vec![account(1, Identity::Unknown("Claude Code did not answer in JSON".into()))];
        let text = accounts_box(&unsure, false);
        assert!(text.contains("(cannot tell — Claude Code did not answer in JSON)"), "{text}");
        assert!(!text.contains("logged out"), "{text}");
    }

    #[test]
    fn without_colour_there_are_no_escapes() {
        // This text is baked into a generated rcfile, so NO_COLOR has to be
        // decided here rather than by whatever ends up printing it.
        assert!(!accounts_box(&three(), false).contains('\x1b'));
        assert!(accounts_box(&three(), true).contains('\x1b'));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod render;` to `src/accounts/mod.rs`, then run:
`cargo test --manifest-path riabuild-cli/Cargo.toml accounts::render`
Expected: FAIL — `cannot find function 'accounts_box' in this scope`.

- [ ] **Step 3: Write the renderer**

Put this above the test module in `src/accounts/render.rs`:

```rust
//! The account list a developer sees at every shell start.
//!
//! `colour` is a parameter rather than something read from `Ui`, for the same
//! reason `shell::banner` takes one: this text is printed by a generated rcfile,
//! and the `NO_COLOR` decision has to cross that boundary as data.

use super::MAX;
use super::status::{Account, Identity};

pub fn accounts_box(accounts: &[Account], colour: bool) -> String {
    let mut lines = vec![paint("Your Claude Code accounts:", "1", colour), String::new()];

    let width = accounts
        .iter()
        .map(|account| label(account).chars().count())
        .max()
        .unwrap_or(0);
    for account in accounts {
        let label = label(account);
        let padding = " ".repeat(width - label.chars().count());
        lines.push(format!(
            "  {}. {label}{padding}   {}",
            account.number,
            identity(&account.identity, colour)
        ));
    }

    let hints = hints(accounts);
    if !hints.is_empty() {
        lines.push(String::new());
        let width = hints
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        for (label, command) in hints {
            let padding = " ".repeat(width - label.chars().count());
            lines.push(format!("  {}{padding}  {command}", paint(&label, "2", colour)));
        }
    }

    lines.join("\n")
}

/// The command that runs this account. The primary answers to two names.
fn label(account: &Account) -> String {
    if account.number == 1 {
        "claude-1 / claude".to_string()
    } else {
        format!("claude-{}", account.number)
    }
}

fn identity(identity: &Identity, colour: bool) -> String {
    match identity {
        Identity::LoggedIn(email) => email.clone(),
        Identity::LoggedOut => paint("(logged out)", "2", colour),
        Identity::Unknown(why) => paint(&format!("(cannot tell — {why})"), "2", colour),
    }
}

/// Only the commands that would succeed right now.
///
/// A hint that refuses when typed is worse than no hint: it reads as riabuild
/// being broken rather than as the developer asking for something impossible.
fn hints(accounts: &[Account]) -> Vec<(String, String)> {
    let mut hints = Vec::new();
    if accounts.len() < MAX {
        hints.push((
            "Add an account:".to_string(),
            "riabuild claude new".to_string(),
        ));
    }
    if accounts.len() > 1 {
        hints.push((
            "Delete an account:".to_string(),
            format!("riabuild claude delete {}", accounts.len()),
        ));
        hints.push((
            "Make it primary:".to_string(),
            "riabuild claude primary 2".to_string(),
        ));
    }
    if let Some(account) = accounts
        .iter()
        .find(|account| account.identity == Identity::LoggedOut)
    {
        hints.push((
            "Log in:".to_string(),
            format!("claude-{} auth login", account.number),
        ));
    }
    hints
}

fn paint(text: &str, code: &str, colour: bool) -> String {
    if colour {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml accounts::render`
Expected: PASS (7 tests). If the column assertions fail, the padding arithmetic is off by the fixed three spaces after the label — fix the format string, not the test.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/src/accounts/render.rs riabuild-cli/src/accounts/mod.rs
git commit -m "Render the Claude Code account box"
```

---

### Task 7: One launcher per account

**Files:**
- Modify: `src/shims/mod.rs` (module comment, `launcher_script`, `write_all`, tests)

**Interfaces:**
- Consumes: `Ctx::claude()` (Task 4), `accounts::MAX` (Task 2), `UserConfig.claude_accounts` (Task 1).
- Produces: `shims::launcher_script(&Path, &str, &Path, &Path) -> String` (config dir, claude binary, org settings, bin dir), and a `write_all` that generates `claude` and `claude-1`…`claude-N` and prunes everything stale.

- [ ] **Step 1: Write the failing tests**

Replace the `the_launcher_*` tests and `writing_the_shim_twice_is_safe` in `src/shims/mod.rs` with:

```rust
    fn script() -> String {
        launcher_script(
            Path::new("/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555"),
            "/Users/ada/.riabuild/node/22.23.1/bin/claude",
            Path::new("/Users/ada/.riabuild/org-settings.json"),
            Path::new("/Users/ada/.riabuild/bin"),
        )
    }

    #[test]
    fn the_launcher_sets_the_account_and_layers_org_settings() {
        let script = script();
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains(
            r#"CLAUDE_CONFIG_DIR="/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555""#
        ));
        assert!(script.contains(r#"--settings "/Users/ada/.riabuild/org-settings.json""#));
        // Arguments must reach claude, or `claude-2 --resume` silently loses
        // them — and `claude-2 auth login`, which the account box tells the
        // developer to run, would do nothing at all.
        assert!(script.contains(r#""$@""#));
    }

    #[test]
    fn the_launcher_can_never_exec_itself() {
        // `~/.riabuild/bin` is first on PATH, so a script called `claude` that
        // runs `exec claude` finds itself and forks until the shell dies.
        let script = script();
        assert!(!script.contains("exec claude"), "{script}");
        assert!(
            script.contains(r#"claude_binary="/Users/ada/.riabuild/node/22.23.1/bin/claude""#),
            "{script}"
        );
        assert!(script.contains(r#"exec "$claude_binary""#), "{script}");
    }

    #[test]
    fn a_binary_that_moved_is_found_without_riabuilds_own_bin() {
        // `claude update` can migrate to a native install, which leaves the
        // recorded path dangling until the next `riabuild`. A dead `claude`
        // command reads as Claude Code being uninstalled.
        let script = script();
        assert!(script.contains(r#"if [ ! -x "$claude_binary" ]"#), "{script}");
        assert!(
            script.contains(r#"grep -vxF "/Users/ada/.riabuild/bin""#),
            "{script}"
        );
        // `tr '\n' ':'` would leave a trailing colon, and an empty PATH entry
        // means the current directory.
        assert!(script.contains("paste -sd: -"), "{script}");
    }

    #[test]
    fn the_launcher_still_works_before_settings_have_been_fetched() {
        let script = script();
        assert!(script.contains(r#"if [ -f "/Users/ada/.riabuild/org-settings.json" ]"#));
    }

    #[tokio::test]
    async fn every_account_gets_a_launcher_and_the_first_gets_two() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let ids = vec![accounts::new_id(), accounts::new_id(), accounts::new_id()];
        ctx.config.claude_accounts = ids.clone();

        write_all(&ctx).await.unwrap();
        // Safe to run twice, like every other apply().
        write_all(&ctx).await.unwrap();

        let bin = ctx.paths.bin_dir();
        for (index, id) in ids.iter().enumerate() {
            let script = tokio::fs::read_to_string(bin.join(format!("claude-{}", index + 1)))
                .await
                .unwrap();
            assert!(script.contains(id.as_str()), "claude-{}", index + 1);
        }
        let primary = tokio::fs::read_to_string(bin.join("claude")).await.unwrap();
        assert!(primary.contains(ids[0].as_str()), "{primary}");
    }

    #[tokio::test]
    async fn launchers_for_accounts_that_are_gone_are_removed() {
        // An orphan is worse than a missing shim: it points at a deleted
        // directory, so Claude Code makes it afresh, asks for a login, and
        // leaves an account no riabuild command can see.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec![accounts::new_id(), accounts::new_id()];
        write_all(&ctx).await.unwrap();

        let bin = ctx.paths.bin_dir();
        // An older riabuild's launcher, and a third account since deleted.
        tokio::fs::write(bin.join("c"), "#!/bin/sh\n").await.unwrap();
        tokio::fs::write(bin.join("claude-3"), "#!/bin/sh\n")
            .await
            .unwrap();

        ctx.config.claude_accounts.truncate(1);
        write_all(&ctx).await.unwrap();

        assert!(tokio::fs::try_exists(bin.join("claude-1")).await.unwrap());
        assert!(!tokio::fs::try_exists(bin.join("claude-2")).await.unwrap());
        assert!(!tokio::fs::try_exists(bin.join("claude-3")).await.unwrap());
        assert!(!tokio::fs::try_exists(bin.join("c")).await.unwrap());
    }
```

Add `use crate::accounts;` to the test module's imports.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml shims::`
Expected: FAIL — `this function takes 2 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Rewrite the generator and `write_all`**

Replace `launcher_script` in `src/shims/mod.rs`:

```rust
/// One account's launcher: `claude`, or `claude-<n>`.
pub fn launcher_script(
    config_dir: &Path,
    claude: &str,
    org_settings: &Path,
    bin_dir: &Path,
) -> String {
    format!(
        r#"#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
#
# Launches Claude Code with one account's config directory and the team's
# settings layered on top. --settings wins over the account's own settings,
# which is how org policy stays current without riabuild ever editing
# settings.json.
set -e
CLAUDE_CONFIG_DIR="{config_dir}"
export CLAUDE_CONFIG_DIR
claude_binary="{claude}"
if [ ! -x "$claude_binary" ]; then
  # The recorded binary is gone: a `claude update` that migrated to a native
  # install, or a Node version change since the last run. Fall back to PATH
  # with riabuild's own bin/ removed — without that this script finds itself,
  # because bin/ comes first inside the environment shell.
  PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "{bin_dir}" | paste -sd: -)
  export PATH
  claude_binary=claude
fi
if [ -f "{settings}" ]; then
  exec "$claude_binary" --settings "{settings}" "$@"
fi
exec "$claude_binary" "$@"
"#,
        config_dir = config_dir.display(),
        bin_dir = bin_dir.display(),
        settings = org_settings.display(),
    )
}
```

Replace `write_all`:

```rust
pub async fn write_all(ctx: &Ctx) -> Result<()> {
    let bin = ctx.paths.bin_dir();
    tokio::fs::create_dir_all(&bin).await?;

    let claude = ctx.claude();
    let settings = ctx.paths.org_settings_file();
    let ids = ctx.config.claude_accounts.clone();

    for (index, id) in ids.iter().enumerate() {
        let script = launcher_script(
            &ctx.paths.claude_profile_dir(id),
            &claude,
            &settings,
            &bin,
        );
        write_launcher(&bin.join(format!("claude-{}", index + 1)), &script).await?;
        if index == 0 {
            write_launcher(&bin.join("claude"), &script).await?;
        }
    }

    prune(&bin, ids.len()).await;
    Ok(())
}

async fn write_launcher(path: &Path, script: &str) -> Result<()> {
    tokio::fs::write(path, script).await?;
    make_executable(path).await?;
    Ok(())
}

/// Removes launchers that no longer name an account.
///
/// Errors are ignored on purpose: every one of these is "it was not there",
/// which is the state being asked for.
async fn prune(bin: &Path, count: usize) {
    // `c` is what riabuild called the launcher before accounts existed.
    let _ = tokio::fs::remove_file(bin.join("c")).await;
    if count == 0 {
        let _ = tokio::fs::remove_file(bin.join("claude")).await;
    }
    for number in count + 1..=crate::accounts::MAX {
        let _ = tokio::fs::remove_file(bin.join(format!("claude-{number}"))).await;
    }
}
```

Update the module comment at the top of the file: the launcher is `claude`, one per account, and `c` is gone.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml shims::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/src/shims/mod.rs
git commit -m "Generate one Claude Code launcher per account"
```

---

### Task 8: The box opens every environment shell

**Files:**
- Modify: `src/shell/mod.rs` (`environment`, `spawn`, new `prelude`)
- Modify: `src/shell/zsh.rs`, `src/shell/bash.rs`, `src/shell/fish.rs` (`prepare` takes the prelude)

**Interfaces:**
- Consumes: `accounts::status::read_all` (Task 5), `accounts::render::accounts_box` (Task 6).
- Produces: `shell::prelude(&[Account], bool) -> String`; `zsh::prepare(&Ctx, &str)`, `bash::prepare(&Ctx, &str)`, `fish::prepare(&Ctx, &str)`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/shell/mod.rs`, and delete `the_environment_marks_the_session_and_points_claude_at_the_profile`:

```rust
    #[tokio::test]
    async fn the_environment_marks_the_session_but_pins_no_account() {
        // The launchers each set CLAUDE_CONFIG_DIR themselves. Exporting it too
        // would go stale the moment `riabuild claude primary` reorders the
        // list, and would send any claude started outside a launcher to a
        // Clubria account with no org settings layered.
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.claude_accounts = vec!["11111111-2222-4333-8444-555555555555".into()];
        let env = environment(&ctx);

        assert!(env.iter().any(|(key, value)| key == "RIABUILD_SHELL" && value == "1"));
        assert!(
            !env.iter().any(|(key, _)| key == "CLAUDE_CONFIG_DIR"),
            "{env:?}"
        );
    }

    #[test]
    fn the_prelude_is_the_box_then_the_banner() {
        use crate::accounts::status::{Account, Identity};
        let accounts = vec![Account {
            number: 1,
            id: "id-1".into(),
            identity: Identity::LoggedIn("clubria@proton.me".into()),
        }];
        let text = prelude(&accounts, false);

        let box_line = text.find("Your Claude Code accounts:").unwrap();
        let banner_line = text.find("Clubria environment active").unwrap();
        // The banner says how to leave, so it reads last, closest to the prompt.
        assert!(box_line < banner_line, "{text}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml shell::`
Expected: FAIL — `cannot find function 'prelude' in this scope`.

- [ ] **Step 3: Add the prelude and drop the export**

In `src/shell/mod.rs`, delete the whole `if let Some(profile) = ctx.config.primary_account() { … }` block from `environment`, and add:

```rust
/// Everything printed when the environment shell starts: the account box, then
/// the banner.
///
/// One string so that each shell's existing `banner_command` — and the
/// `[[ -t 1 ]]` guard inside it that keeps this out of captured output — covers
/// both without any of them learning what an account is.
pub fn prelude(accounts: &[crate::accounts::status::Account], colour: bool) -> String {
    format!(
        "{}\n\n{}",
        crate::accounts::render::accounts_box(accounts, colour),
        banner(colour)
    )
}
```

In `spawn`, build the prelude before the match and pass it in:

```rust
pub async fn spawn(ctx: &mut Ctx) -> Result<i32> {
    let shell = Shell::detect();
    let env = environment(ctx);

    let accounts = crate::accounts::status::read_all(ctx).await;
    let prelude = prelude(&accounts, ctx.ui.colour());

    let (args, extra_env) = match &shell {
        Shell::Zsh => zsh::prepare(ctx, &prelude).await?,
        Shell::Bash => bash::prepare(ctx, &prelude).await?,
        Shell::Fish => fish::prepare(ctx, &prelude).await?,
        // riabuild generates no startup file for a shell it does not know, so
        // there is nothing inside it to print this. The parent says it instead
        // — and only here, so it is still said once.
        Shell::Other(_) => {
            ctx.ui.info(&prelude);
            (Vec::new(), Vec::new())
        }
    };
    // …unchanged from here…
```

- [ ] **Step 4: Thread it through the three shells**

In each of `zsh.rs`, `bash.rs` and `fish.rs`, change the signature to `pub async fn prepare(ctx: &Ctx, prelude: &str) -> Result<super::ShellLaunch>` and replace `&banner_command(&super::banner(colour))` with `&banner_command(prelude)`.

`colour` is still needed for `prompt_command`, so leave `let colour = ctx.ui.colour();` where it is. In `bash.rs`, which has no prompt colour to thread, remove the `colour` binding if it becomes unused.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml`
Expected: PASS. The per-shell rcfile tests pass a literal like `"echo hi"` for the banner and keep working unchanged.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/src/shell
git commit -m "Open every environment shell with the account box"
```

---

### Task 9: The provisioning task manages accounts

**Files:**
- Rename: `src/tasks/claude_profiles.rs` → `src/tasks/claude_accounts.rs` (`git mv`)
- Modify: `src/tasks/mod.rs` (module list, `registry()`)
- Modify: `src/tasks/claude_trust.rs:94` (`depends_on`)

**Interfaces:**
- Consumes: `accounts::{new_id, looks_like_id, MAX}` (Task 2), `accounts::status::{read, Identity}` (Task 5), `Ctx::claude()` (Task 4).
- Produces: `tasks::claude_accounts::ClaudeAccounts` with `id()` of `"claude_accounts"`.

- [ ] **Step 1: Rename the file and move the id helpers out**

```bash
git mv riabuild-cli/src/tasks/claude_profiles.rs riabuild-cli/src/tasks/claude_accounts.rs
```

In `src/tasks/mod.rs`, replace `pub mod claude_profiles;` with `pub mod claude_accounts;` and `Box::new(claude_profiles::ClaudeProfiles)` with `Box::new(claude_accounts::ClaudeAccounts)`. In `src/tasks/claude_trust.rs`, change `depends_on` from `&["claude_profiles", "project"]` to `&["claude_accounts", "project"]`.

Delete `new_profile_id`, `looks_like_profile_id`, `existing_profile`, and their two unit tests from the renamed file — Task 2 already owns them. Update `claude_trust.rs`'s test import from `crate::tasks::claude_profiles::new_profile_id` to `crate::accounts::new_id`.

- [ ] **Step 2: Write the failing tests**

Replace the `mod tests` in `src/tasks/claude_accounts.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
    use std::sync::Arc;

    const VERSION: &str = "claude --version";
    const STATUS: &str = "claude auth status --json";

    fn installed() -> FakeRunner {
        FakeRunner::new().with(VERSION, 0, "2.1.223 (Claude Code)", "")
    }

    fn signed_in() -> FakeRunner {
        installed().with(
            STATUS,
            0,
            r#"{"loggedIn":true,"email":"clubria@proton.me"}"#,
            "",
        )
    }

    /// A ctx with one account on disk and Claude Code installed and signed in.
    async fn ready() -> (Ctx, tempfile::TempDir, String) {
        let (mut ctx, home) = ctx_with(FakeRunner::new()).await;
        let id = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
            .await
            .unwrap();
        ctx.config.claude_accounts = vec![id.clone()];
        ctx.runner = Arc::new(signed_in());
        (ctx, home, id)
    }

    #[tokio::test]
    async fn a_missing_claude_is_detected() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not installed"), "{status:?}");
    }

    #[tokio::test]
    async fn an_old_claude_is_detected() {
        let runner = FakeRunner::new().with(VERSION, 0, "1.9.0 (Claude Code)", "");
        let (ctx, _home) = ctx_with(runner).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("older than"), "{status:?}");
    }

    #[tokio::test]
    async fn a_machine_with_no_account_is_detected() {
        let (ctx, _home) = ctx_with(installed()).await;
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("no Claude Code account"), "{status:?}");
    }

    #[tokio::test]
    async fn a_deleted_account_directory_is_noticed() {
        let (mut ctx, _home) = ctx_with(installed()).await;
        tokio::fs::create_dir_all(ctx.paths.claude_dir()).await.unwrap();
        ctx.config.claude_accounts = vec![accounts::new_id()];
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("directory is missing"), "{status:?}");
    }

    #[tokio::test]
    async fn a_directory_nothing_recorded_is_noticed() {
        let (mut ctx, _home, _id) = ready().await;
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&accounts::new_id()))
            .await
            .unwrap();
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not registered"), "{status:?}");
    }

    #[tokio::test]
    async fn a_signed_out_primary_is_drift() {
        let (mut ctx, _home, _id) = ready().await;
        ctx.runner = Arc::new(installed().with(STATUS, 1, r#"{"loggedIn":false}"#, ""));
        let status = ClaudeAccounts.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("account 1 is not signed in"), "{status:?}");
    }

    #[tokio::test]
    async fn a_provisioned_machine_is_satisfied() {
        let (ctx, _home, _id) = ready().await;
        assert_eq!(ClaudeAccounts.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn applying_creates_the_first_account() {
        let (mut ctx, _home) = ctx_with(signed_in()).await;
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts.len(), 1);
        assert_eq!(ClaudeAccounts.check(&ctx).await.unwrap(), Status::Satisfied);
    }

    #[tokio::test]
    async fn a_directory_nothing_recorded_is_adopted_rather_than_abandoned() {
        // The rescue this exists for: config.json lost, but the login and a
        // year of session history are still sitting in the directory.
        let (mut ctx, _home) = ctx_with(signed_in()).await;
        let orphan = accounts::new_id();
        tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&orphan))
            .await
            .unwrap();

        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, vec![orphan]);
    }

    #[tokio::test]
    async fn an_account_whose_directory_vanished_is_dropped() {
        let (mut ctx, _home, id) = ready().await;
        let gone = accounts::new_id();
        ctx.config.claude_accounts.push(gone.clone());

        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, vec![id]);
    }

    #[tokio::test]
    async fn applying_twice_is_safe() {
        let (mut ctx, _home) = ctx_with(signed_in()).await;
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        let first = ctx.config.claude_accounts.clone();
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, first);
    }

    #[tokio::test]
    async fn an_abandoned_sign_in_is_not_treated_as_success() {
        // Claude Code exits non-zero when the browser is closed. A task that
        // ignored that would report a machine that is ready and is not.
        let (mut ctx, _home) = ctx_with(
            installed()
                .with(STATUS, 1, r#"{"loggedIn":false}"#, "")
                .with("claude auth login", 1, "", ""),
        )
        .await;
        let error = ClaudeAccounts.apply(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("signing you in to Claude Code"), "{error}");
    }

    #[tokio::test]
    async fn a_signed_in_account_is_never_sent_through_a_browser() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();
        ClaudeAccounts.apply(&mut ctx).await.unwrap();
        assert!(
            !runner.calls().iter().any(|call| call.contains("auth login")),
            "{:?}",
            runner.calls()
        );
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml claude_accounts`
Expected: FAIL — `cannot find type 'ClaudeAccounts'`.

- [ ] **Step 4: Rewrite the task**

Replace everything above the test module in `src/tasks/claude_accounts.rs`:

```rust
//! Task 7 — the developer's Claude Code accounts.
//!
//! riabuild creates the account directories and never writes into anyone's
//! `settings.json`. Org policy is layered at launch by the `claude-<n>`
//! launchers instead — see `org_settings` for why a recurring deep-merge is the
//! wrong shape.
//!
//! Account 1 is the one this task insists on: it must exist, and it must be
//! signed in. riabuild's job is "running Claude Code against our codebase", and
//! a signed-out Claude Code is not that. Accounts 2 upward are the developer's
//! own business — the account box reports them and this task ignores them.

use super::{Ctx, Status, Task, TaskId};
use crate::accounts::{self, status::Identity};
use crate::runner::RunOptions;
use crate::ui::Failure;
use crate::version;
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MIN_VERSION: &str = "2.1.223";

pub struct ClaudeAccounts;

/// Every account directory actually on disk, oldest first.
///
/// Oldest first so that adoption keeps a developer's original account as
/// account 1, which is the one their editor and their muscle memory point at.
async fn ids_on_disk(claude_dir: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(claude_dir).await else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, String)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !accounts::looks_like_id(&name) {
            continue;
        }
        found.push((meta.modified().unwrap_or(UNIX_EPOCH), name));
    }
    found.sort();
    found.into_iter().map(|(_, name)| name).collect()
}

#[async_trait]
impl Task for ClaudeAccounts {
    fn id(&self) -> TaskId {
        "claude_accounts"
    }

    fn title(&self) -> &str {
        "Claude Code accounts"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // Claude Code is installed with the Node riabuild owns, so the
        // toolchain has to exist first.
        &["toolchain"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let claude = ctx.claude();
        let reported = ctx
            .runner
            .run(&claude, &["--version"], &RunOptions::default())
            .await?;
        if !reported.ok() {
            return Ok(Status::needs("Claude Code is not installed"));
        }
        if !version::at_least(reported.trimmed(), MIN_VERSION) {
            return Ok(Status::needs(format!(
                "Claude Code is older than {MIN_VERSION}"
            )));
        }

        let ids = &ctx.config.claude_accounts;
        let Some(primary) = ids.first() else {
            return Ok(Status::needs("no Claude Code account yet"));
        };
        for id in ids {
            let dir = ctx.paths.claude_profile_dir(id);
            if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
                return Ok(Status::needs("a Claude Code account directory is missing"));
            }
        }
        // A directory nothing recorded is drift in the other direction: real
        // sessions and a real login that no riabuild command can reach.
        for found in ids_on_disk(&ctx.paths.claude_dir()).await {
            if !ids.contains(&found) {
                return Ok(Status::needs("a Claude Code account is not registered"));
            }
        }

        match accounts::status::read(ctx, primary).await {
            Identity::LoggedIn(_) => Ok(Status::Satisfied),
            Identity::LoggedOut => Ok(Status::needs("account 1 is not signed in")),
            Identity::Unknown(why) => Ok(Status::needs(format!(
                "riabuild could not tell whether account 1 is signed in: {why}"
            ))),
        }
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let claude = ctx.claude();
        if !ctx
            .runner
            .run(&claude, &["--version"], &RunOptions::default())
            .await?
            .ok()
        {
            install_claude(ctx).await?;
        }

        let claude_dir = ctx.paths.claude_dir();
        tokio::fs::create_dir_all(&claude_dir).await?;

        let mut kept = Vec::new();
        for id in ctx.config.claude_accounts.clone() {
            if tokio::fs::try_exists(claude_dir.join(&id))
                .await
                .unwrap_or(false)
            {
                kept.push(id);
            }
        }
        for found in ids_on_disk(&claude_dir).await {
            if !kept.contains(&found) && kept.len() < accounts::MAX {
                kept.push(found);
            }
        }
        if kept.is_empty() {
            let id = accounts::new_id();
            tokio::fs::create_dir_all(claude_dir.join(&id)).await?;
            kept.push(id);
        }
        ctx.config.claude_accounts = kept;
        ctx.config.save(ctx.paths.as_ref()).await?;

        let Some(primary) = ctx.config.claude_accounts.first().cloned() else {
            return Ok(());
        };
        if !matches!(
            accounts::status::read(ctx, &primary).await,
            Identity::LoggedIn(_)
        ) {
            sign_in(ctx, &primary).await?;
        }
        Ok(())
    }
}

/// The one browser round trip provisioning makes for Claude Code.
///
/// Mirrors `github_cli::sign_in`, including checking the exit code: a developer
/// who abandons the browser must not leave riabuild convinced this machine is
/// ready, with the only symptom a later failure that says nothing about a
/// sign-in.
async fn sign_in(ctx: &mut Ctx, id: &str) -> Result<()> {
    ctx.ui
        .note("Opening your browser to sign in to Claude Code…");
    let claude = ctx.claude();
    let dir = ctx.paths.claude_profile_dir(id);
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };

    let code = ctx
        .runner
        .run_interactive(&claude, &["auth", "login"], &options)
        .await?;
    if code != 0 {
        return Err(Failure::new(
            "signing you in to Claude Code",
            "Run `riabuild` again and finish the Claude Code sign-in in your browser.",
        )
        .command("claude auth login")
        .detail(format!("that command exited with status {code}"))
        .into());
    }
    Ok(())
}
```

Keep `install_claude` exactly as it is, changing only its `ctx.ui.note` text if it mentions profiles.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml`
Expected: PASS. `engine`'s DAG tests may reference `claude_profiles` by name — update any such string to `claude_accounts`.

- [ ] **Step 6: Commit**

```bash
git add -A riabuild-cli/src
git commit -m "Provision Claude Code accounts, and sign the first one in"
```

---

### Task 10: Every account trusts the checkout

**Files:**
- Modify: `src/tasks/claude_trust.rs` (`check`, `apply`, module comment, tests)

**Interfaces:**
- Consumes: `UserConfig.claude_accounts` (Task 1).
- Produces: no new public surface — `ClaudeTrust` now iterates accounts.

- [ ] **Step 1: Write the failing tests**

In `src/tasks/claude_trust.rs`, change the `ready()` helper to create **two** accounts and return both ids, then add:

```rust
    #[tokio::test]
    async fn one_trusted_account_is_not_enough() {
        // claude-2 would open the trust modal on first launch and hold the
        // org's settings back as untrusted — the exact dialog this task exists
        // to prevent, just one account over.
        let (mut ctx, _home, ids, _dir) = ready().await;
        write_file(&config_file(&ctx, &ids[0]), r#"{"numStartups":1}"#).await;
        ClaudeTrust.apply(&mut ctx).await.unwrap();

        // Now break only the second account's trust.
        write_file(&config_file(&ctx, &ids[1]), r#"{"numStartups":1}"#).await;
        let status = ClaudeTrust.check(&ctx).await.unwrap();
        assert!(format!("{status:?}").contains("not trusted"), "{status:?}");
        assert!(format!("{status:?}").contains('2'), "{status:?}");
    }

    #[tokio::test]
    async fn applying_trusts_every_account() {
        let (mut ctx, _home, ids, dir) = ready().await;
        ClaudeTrust.apply(&mut ctx).await.unwrap();

        let key = dir.to_string_lossy().into_owned();
        for id in &ids {
            let text = tokio::fs::read_to_string(config_file(&ctx, id)).await.unwrap();
            let root: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(root["projects"][&key]["hasTrustDialogAccepted"], json!(true), "{id}");
        }
        assert_eq!(ClaudeTrust.check(&ctx).await.unwrap(), Status::Satisfied);
    }
```

Update every other test in the file to the new `ready()` return shape.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml claude_trust`
Expected: FAIL — trust is written for one account, so `applying_trusts_every_account` finds no `projects` key in the second.

- [ ] **Step 3: Loop over the accounts**

In `check`, replace the single-profile lookup with a loop, reporting the account number:

```rust
    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        if ctx.config.claude_accounts.is_empty() {
            return Ok(Status::needs("no Claude Code account yet"));
        }
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory yet"));
        };
        let keys = trust_keys(&dir).await;
        let shown = contract_tilde(&dir, &ctx.paths.home());

        for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
            let file = ctx.paths.claude_config_file(id);
            let Ok(text) = tokio::fs::read_to_string(&file).await else {
                return Ok(Status::needs(format!(
                    "account {} has no Claude Code config yet",
                    index + 1
                )));
            };
            let Ok(root) = serde_json::from_str::<Value>(&text) else {
                // Claude Code cannot start against this, so the machine is
                // broken whatever the trust key says.
                return Ok(Status::needs(format!(
                    "the Claude Code config for account {} is not valid JSON",
                    index + 1
                )));
            };
            if !keys.iter().all(|key| is_trusted(&root, key)) {
                return Ok(Status::needs(format!(
                    "{shown} is not trusted by account {} yet",
                    index + 1
                )));
            }
        }

        Ok(Status::Satisfied)
    }
```

Note the fix carried along: the old loop said `if !is_trusted(…) { return … }` inside `for key in …`, which is the same as `all`, but the new form states it once.

In `apply`, hoist the checks out and loop the write:

```rust
    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        if ctx.config.claude_accounts.is_empty() {
            return Err(Failure::new(
                "trusting the checkout",
                "Run `riabuild` again — a Claude Code account has to exist first.",
            )
            .into());
        }
        let Some(dir) = ctx.project_dir() else {
            return Err(Failure::new(
                "trusting the checkout",
                "Run `riabuild` again — the checkout has to exist first.",
            )
            .into());
        };
        let keys = trust_keys(&dir).await;

        for id in ctx.config.claude_accounts.clone() {
            trust_one(ctx, &id, &keys).await?;
        }
        Ok(())
    }
```

Move the existing body — create the parent, `load_or_reset`, edit `projects`, staged write and rename — into:

```rust
/// Writes the trust key into one account's config, preserving every key it does
/// not own. Claude Code may be running against this file right now, so the new
/// content lands whole or not at all.
async fn trust_one(ctx: &mut Ctx, id: &str, keys: &[String]) -> Result<()> {
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml claude_trust`
Expected: PASS.

- [ ] **Step 5: Update the module comment and commit**

The header still says "Only the riabuild-owned profile is touched". Make it say every riabuild-owned account is, and that the developer's own `~/.claude.json` is still never touched.

```bash
git add riabuild-cli/src/tasks/claude_trust.rs
git commit -m "Trust the checkout in every Claude Code account"
```

---

### Task 11: `riabuild claude` — list, new, delete, primary

**Files:**
- Create: `src/accounts/command.rs`
- Modify: `src/accounts/mod.rs` (add `pub mod command;`)
- Modify: `src/cli.rs` (`Command::Claude`, `ClaudeAction`)
- Modify: `src/main.rs` (dispatch)

**Interfaces:**
- Consumes: everything from Tasks 2, 5, 6, 7.
- Produces: `accounts::command::run(&mut Ctx, Option<ClaudeAction>) -> Result<i32>`.

- [ ] **Step 1: Add the command-line surface and its tests**

In `src/cli.rs`, add to `enum Command`:

```rust
    /// Manage your Claude Code accounts.
    Claude {
        #[command(subcommand)]
        action: Option<ClaudeAction>,
    },
```

and below it:

```rust
#[derive(Debug, Subcommand)]
pub enum ClaudeAction {
    /// List your Claude Code accounts.
    List,
    /// Add an account and sign it in.
    New,
    /// Remove an account. Later accounts move up a number.
    Delete {
        /// Which account, as shown by `riabuild claude`.
        #[arg(value_name = "NUMBER")]
        number: usize,
        /// Remove it without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Make an account the one `claude` runs.
    Primary {
        #[arg(value_name = "NUMBER")]
        number: usize,
    },
}
```

Add to `cli.rs`'s tests:

```rust
    #[test]
    fn bare_claude_lists_the_accounts() {
        let cli = Cli::parse_from(["riabuild", "claude"]);
        assert!(matches!(cli.command, Some(Command::Claude { action: None })));
    }

    #[test]
    fn deleting_an_account_takes_a_number_and_can_skip_the_prompt() {
        let cli = Cli::parse_from(["riabuild", "claude", "delete", "3", "--yes"]);
        let Some(Command::Claude {
            action: Some(ClaudeAction::Delete { number, yes }),
        }) = cli.command
        else {
            panic!("expected claude delete");
        };
        assert_eq!(number, 3);
        assert!(yes);
    }
```

- [ ] **Step 2: Write the failing behaviour tests**

Create `src/accounts/command.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;
    use crate::ui::Ui;
    use std::sync::Arc;

    const STATUS: &str = "claude auth status --json";

    fn signed_in() -> FakeRunner {
        FakeRunner::new().with(
            STATUS,
            0,
            r#"{"loggedIn":true,"email":"clubria@proton.me"}"#,
            "",
        )
    }

    /// A ctx with `count` accounts on disk, all signed in.
    async fn with_accounts(count: usize) -> (Ctx, tempfile::TempDir, Vec<String>) {
        let (mut ctx, home) = ctx_with(signed_in()).await;
        let mut ids = Vec::new();
        for _ in 0..count {
            let id = accounts::new_id();
            tokio::fs::create_dir_all(ctx.paths.claude_profile_dir(&id))
                .await
                .unwrap();
            ids.push(id);
        }
        ctx.config.claude_accounts = ids.clone();
        (ctx, home, ids)
    }

    #[tokio::test]
    async fn deleting_the_only_account_is_refused() {
        let (mut ctx, _home, ids) = with_accounts(1).await;
        let error = delete(&mut ctx, 1, true).await.unwrap_err().to_string();
        assert!(error.contains("only Claude Code account"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
        assert!(
            tokio::fs::try_exists(ctx.paths.claude_profile_dir(&ids[0]))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn deleting_signs_out_before_removing_the_directory() {
        // The keychain item is named for a hash of the directory's path, so
        // removing the directory first orphans a credential permanently.
        let (mut ctx, _home, ids) = with_accounts(2).await;
        let runner = Arc::new(signed_in());
        ctx.runner = runner.clone();

        delete(&mut ctx, 2, true).await.unwrap();

        let logouts: Vec<String> = runner
            .calls()
            .into_iter()
            .filter(|call| call.contains("auth logout"))
            .collect();
        assert_eq!(logouts.len(), 1, "{:?}", runner.calls());
        assert!(
            !tokio::fs::try_exists(ctx.paths.claude_profile_dir(&ids[1]))
                .await
                .unwrap()
        );
        assert_eq!(ctx.config.claude_accounts, vec![ids[0].clone()]);
    }

    #[tokio::test]
    async fn deleting_with_nobody_to_ask_refuses_rather_than_assuming() {
        let (mut ctx, _home, ids) = with_accounts(2).await;
        // ctx_with builds a quiet, non-interactive Ui.
        let error = delete(&mut ctx, 2, false).await.unwrap_err().to_string();
        assert!(error.contains("asking whether to delete"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn declining_leaves_the_account_alone() {
        let (mut ctx, _home, ids) = with_accounts(2).await;
        ctx.ui = Ui::scripted(["n"]);
        delete(&mut ctx, 2, false).await.unwrap();
        assert_eq!(ctx.config.claude_accounts, ids);
    }

    #[tokio::test]
    async fn making_an_account_primary_reorders_and_rewrites_the_launchers() {
        let (mut ctx, _home, ids) = with_accounts(3).await;
        primary(&mut ctx, 3).await.unwrap();

        assert_eq!(
            ctx.config.claude_accounts,
            vec![ids[2].clone(), ids[0].clone(), ids[1].clone()]
        );
        let script = tokio::fs::read_to_string(ctx.paths.bin_dir().join("claude"))
            .await
            .unwrap();
        assert!(script.contains(ids[2].as_str()), "{script}");
    }

    #[tokio::test]
    async fn a_sign_in_that_did_not_take_adds_no_account() {
        // The browser was closed. Anything left behind would show as an
        // account permanently "(logged out)" that nobody chose to create.
        let (mut ctx, _home, ids) = with_accounts(1).await;
        ctx.runner = Arc::new(
            FakeRunner::new()
                .with(STATUS, 1, r#"{"loggedIn":false}"#, "")
                .with("claude auth login", 1, "", ""),
        );

        let error = new(&mut ctx).await.unwrap_err().to_string();
        assert!(error.contains("adding a Claude Code account"), "{error}");
        assert_eq!(ctx.config.claude_accounts, ids);

        let mut entries = tokio::fs::read_dir(ctx.paths.claude_dir()).await.unwrap();
        let mut count = 0;
        while let Ok(Some(_)) = entries.next_entry().await {
            count += 1;
        }
        assert_eq!(count, 1, "the abandoned directory was left behind");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Add `pub mod command;` to `src/accounts/mod.rs`, then run:
`cargo test --manifest-path riabuild-cli/Cargo.toml accounts::command`
Expected: FAIL — `cannot find function 'delete' in this scope`.

- [ ] **Step 4: Write the commands**

Put this above the test module in `src/accounts/command.rs`:

```rust
//! `riabuild claude` — the account list, and the four things done to it.

use crate::accounts;
use crate::accounts::render;
use crate::accounts::status::{self, Identity};
use crate::cli::ClaudeAction;
use crate::paths::contract_tilde;
use crate::runner::RunOptions;
use crate::shims;
use crate::tasks::Ctx;
use crate::ui::Failure;
use anyhow::Result;

pub async fn run(ctx: &mut Ctx, action: Option<ClaudeAction>) -> Result<i32> {
    match action.unwrap_or(ClaudeAction::List) {
        ClaudeAction::List => list(ctx).await,
        ClaudeAction::New => new(ctx).await,
        ClaudeAction::Delete { number, yes } => delete(ctx, number, yes).await,
        ClaudeAction::Primary { number } => primary(ctx, number).await,
    }
}

async fn list(ctx: &Ctx) -> Result<i32> {
    let found = status::read_all(ctx).await;
    ctx.ui.info("");
    ctx.ui
        .info(&render::accounts_box(&found, ctx.ui.colour()));
    Ok(0)
}

/// Adds an account and signs it in — and only keeps it if that worked.
///
/// No Claude Code session is opened: signing in is the whole job, and the
/// developer starts a session with `claude-<n>` when they want one.
async fn new(ctx: &mut Ctx) -> Result<i32> {
    let id = accounts::new_id();
    let number = accounts::add(&mut ctx.config, id.clone())?;
    let dir = ctx.paths.claude_profile_dir(&id);
    tokio::fs::create_dir_all(&dir).await?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    shims::write_all(ctx).await?;

    ctx.ui
        .info(&format!("Signing in account {number} — finish it in your browser."));
    let claude = ctx.claude();
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };
    ctx.runner
        .run_interactive(&claude, &["auth", "login"], &options)
        .await?;

    // Asked rather than inferred from the exit code: the machine's own answer
    // is the one that decides whether an account exists.
    if !matches!(status::read(ctx, &id).await, Identity::LoggedIn(_)) {
        accounts::remove(&mut ctx.config, number)?;
        ctx.config.save(ctx.paths.as_ref()).await?;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        shims::write_all(ctx).await?;
        return Err(Failure::new(
            "adding a Claude Code account",
            "Run `riabuild claude new` again and finish the sign-in in your browser.",
        )
        .detail("the sign-in did not complete, so no account was added")
        .into());
    }

    list(ctx).await
}

async fn delete(ctx: &mut Ctx, number: usize, assume_yes: bool) -> Result<i32> {
    if ctx.config.claude_accounts.len() <= 1 {
        return Err(Failure::new(
            "deleting your only Claude Code account",
            "Add another with `riabuild claude new` first.",
        )
        .detail("the next run would only create an empty one and ask you to sign in again")
        .into());
    }

    let id = accounts::id_of(&ctx.config, number)?;
    let named = match status::read(ctx, &id).await {
        Identity::LoggedIn(email) => email,
        _ => format!("account {number}"),
    };

    if !assume_yes {
        // Checked before asking, because `ask` returns None both for "they
        // chose the default" and for "there was nobody to ask", and an
        // irreversible delete has to tell those apart.
        if !ctx.ui.interactive() {
            return Err(Failure::new(
                format!("asking whether to delete Claude Code account {number}"),
                "re-run as `riabuild claude delete <number> --yes` if you meant to remove it unattended",
            )
            .detail("riabuild has no terminal to ask on, and will not assume yes.")
            .into());
        }
        ctx.ui.info("");
        ctx.ui.info(&format!("  Delete account {number} — {named}?"));
        ctx.ui
            .info("  Its Claude Code sessions, history and login are removed.");
        // `Ui::confirm` defaults to yes, which is right for "shall I install
        // this" and wrong here: an empty answer must decline.
        let answer = ctx.ui.ask("  Confirm [y/N]");
        let confirmed = answer
            .is_some_and(|answer| matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"));
        if !confirmed {
            ctx.ui.info("Left alone.");
            return Ok(0);
        }
    }

    // Signing out first is load-bearing: on macOS the keychain item is named
    // for a hash of the config directory's path, so removing the directory
    // first orphans a credential nothing can ever reach again. A logout that
    // fails is not fatal — the directory still has to go.
    let dir = ctx.paths.claude_profile_dir(&id);
    let claude = ctx.claude();
    let options = RunOptions {
        env: vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        )],
        ..Default::default()
    };
    let _ = ctx.runner.run(&claude, &["auth", "logout"], &options).await;
    ctx.ui.note(&format!("Signed out {named}"));

    let shown = contract_tilde(&dir, &ctx.paths.home());
    tokio::fs::remove_dir_all(&dir).await.map_err(|error| {
        Failure::new(
            format!("removing {shown}"),
            "check what still has a file open there, then run it again",
        )
        .detail(error.to_string())
    })?;
    ctx.ui.note(&format!("Removed {shown}"));

    accounts::remove(&mut ctx.config, number)?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    shims::write_all(ctx).await?;
    if number <= ctx.config.claude_accounts.len() {
        ctx.ui
            .note(&format!("Account {} is now account {number}", number + 1));
    }

    list(ctx).await
}

async fn primary(ctx: &mut Ctx, number: usize) -> Result<i32> {
    accounts::promote(&mut ctx.config, number)?;
    ctx.config.save(ctx.paths.as_ref()).await?;
    // Rewritten in place, so shells that are already open pick this up with no
    // further action — which is the reason the environment no longer exports
    // CLAUDE_CONFIG_DIR.
    shims::write_all(ctx).await?;
    list(ctx).await
}
```

- [ ] **Step 5: Dispatch it from main**

In `src/main.rs`, add to the `match cli.command` block alongside the other early returns:

```rust
        Some(Command::Claude { action }) => {
            return accounts::command::run(&mut ctx, action).await;
        }
```

`riabuild claude` needs no riabuild session: it manages local directories and talks only to Claude Code, so it must not go through `connect`.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml`
Expected: PASS. Then `cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 7: Commit**

```bash
git add riabuild-cli/src
git commit -m "Add riabuild claude: list, new, delete, primary"
```

---

### Task 12: Documentation, the reset warning, and the pull request

**Files:**
- Modify: `src/reset.rs` (`warnings`)
- Modify: `README.md`, `riabuild-cli/CLAUDE.md`, `docs/superpowers/specs/2026-08-04-riabuild-design.md`
- Modify: any doc comment still naming the `c` launcher

- [ ] **Step 1: Write the failing test for the reset warning**

`reset` names the Claude Code history because it is the one thing in the tree that cannot be reconstructed. With accounts, it should say how many. In `src/reset.rs`'s tests:

```rust
    #[tokio::test]
    async fn the_plan_counts_the_accounts_a_reset_would_sign_out() {
        let (home, paths) = provisioned().await;
        for _ in 0..3 {
            let id = crate::accounts::new_id();
            tokio::fs::create_dir_all(paths.claude_profile_dir(&id))
                .await
                .unwrap();
        }
        let plan = plan(&paths).await.unwrap();
        let said = warnings(&plan, &paths.claude_dir()).await.join("\n");
        assert!(said.contains("3 Claude Code accounts"), "{said}");
        drop(home);
    }
```

Adjust the call shape to whatever `warnings` currently takes — read `src/reset.rs:148` first and keep its signature unless the count forces a change.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml reset::`
Expected: FAIL — the warning still describes a single profile.

- [ ] **Step 3: Count the accounts in the warning**

Change the Claude Code line in `warnings` to count directories passing `accounts::looks_like_id` and use `ui::plural`, so one account reads "1 Claude Code account" and three read "3 Claude Code accounts". Keep the existing behaviour of saying nothing when there are none.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --manifest-path riabuild-cli/Cargo.toml reset::`
Expected: PASS.

- [ ] **Step 5: Retire `c` from the documentation**

Run: `grep -rn '\bc\b' README.md riabuild-cli/CLAUDE.md docs/superpowers/specs/2026-08-04-riabuild-design.md riabuild-cli/src --include='*.rs' --include='*.md' | grep -iE 'launcher|shim|`c`'`

Fix each hit:
- `README.md` — the command to start Claude Code is `claude`, and `riabuild claude` manages accounts. Add the four commands.
- `docs/superpowers/specs/2026-08-04-riabuild-design.md` — the disk layout line `bin/  pnpm  c` becomes `bin/  pnpm  claude  claude-1…N`, and the task 7 row becomes `claude_accounts`, described as "at least one account directory exists, account 1 is signed in, `claude --version` ≥ floor". Add a line under it pointing at this spec as the superseding design.
- `riabuild-cli/CLAUDE.md` — the layout table and any mention of the `c` launcher.
- `src/tasks/claude_statusline.rs` and `src/tasks/org_settings.rs` doc comments — both name the `c` launcher.

- [ ] **Step 6: Full verification**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -- --ignored   # on a machine with Claude Code installed
```

Expected: all pass. The `--ignored` run needs the three smoke tests from the spec; if they were not added while implementing Tasks 5 and 7, add them now to `src/shims/mod.rs` beside `claude_config_dir_smoke`:

```rust
    /// Pins the behaviour every account depends on: `CLAUDE_CONFIG_DIR` scopes
    /// the *login*, not just the settings. If this stops holding, two accounts
    /// share one sign-in and the whole feature is a lie.
    #[tokio::test]
    #[ignore = "requires Claude Code installed; pins undocumented behaviour"]
    async fn auth_status_is_scoped_to_the_config_dir() {
        use crate::runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let fresh = home.path().join("fresh");
        tokio::fs::create_dir_all(&fresh).await.unwrap();

        let output = runner
            .run(
                "claude",
                &["auth", "status", "--json"],
                &RunOptions {
                    env: vec![(
                        "CLAUDE_CONFIG_DIR".into(),
                        fresh.to_string_lossy().into_owned(),
                    )],
                    ..Default::default()
                },
            )
            .await
            .expect("claude auth status");

        assert!(
            output.stdout.contains(r#""loggedIn": false"#)
                || output.stdout.contains(r#""loggedIn":false"#),
            "a fresh config dir reported a sign-in: {output:?}"
        );

        // The developer's own, for contrast: it must report an email, which is
        // the field the account box reads.
        let mine = runner
            .run("claude", &["auth", "status", "--json"], &RunOptions::default())
            .await
            .expect("claude auth status");
        assert!(mine.stdout.contains("email"), "{mine:?}");
    }

    /// Every launcher passes `--settings` unconditionally, so `claude-2 auth
    /// login` — which the account box tells developers to run — depends on a
    /// global flag being accepted ahead of a subcommand.
    #[tokio::test]
    #[ignore = "requires Claude Code installed; pins undocumented behaviour"]
    async fn the_settings_flag_survives_a_subcommand() {
        use crate::runner::{CommandRunner, RealRunner, RunOptions};
        let runner = RealRunner;
        let Some(_) = runner.which("claude") else {
            panic!("claude is not installed; this test needs it");
        };

        let home = tempfile::TempDir::new().unwrap();
        let settings = home.path().join("settings.json");
        tokio::fs::write(&settings, "{}").await.unwrap();

        let output = runner
            .run(
                "claude",
                &[
                    "--settings",
                    &settings.to_string_lossy(),
                    "auth",
                    "status",
                    "--json",
                ],
                &RunOptions::default(),
            )
            .await
            .expect("claude --settings auth status");
        assert!(output.stdout.contains("loggedIn"), "{output:?}");
    }
```

- [ ] **Step 7: Open the pull request and watch CI**

```bash
git add -A
git commit -m "Retire the c launcher from the documentation"
git push -u origin HEAD
gh pr create --fill
gh pr checks --watch
```

The task is not finished while checks are queued, running, or failing. If CI fails, fixing it is part of this task.

---

## Self-Review

**Spec coverage.** Every section of the spec maps to a task: data model and migration → 1; registry and the cap → 2; concurrent identity lookup → 5 (with 3 and 4 as its prerequisites); the box and its conditional hints → 6; shims, pruning and the recursion guard → 7; dropping the `CLAUDE_CONFIG_DIR` export and printing the box at shell start → 8; the provisioning task, adoption and the primary sign-in → 9; per-account trust → 10; the four subcommands → 11; the reset warning, the documentation and the three `#[ignore]`d smoke tests → 12.

**Known deviations from the spec, deliberate:**
- The spec says `read_one`; the code calls it `ask`, with `read` as the single-account public entry point. Task 9 and Task 11 both consume `status::read`.
- The spec's shim example writes `REAL`; the plan uses `claude_binary`, which reads better in a script a developer may open.

**Type consistency.** `Account`/`Identity` are defined in Task 5 and used unchanged in 6, 8, 9 and 11. `accounts::remove` returns `Result<String>` and every caller discards the id except `promote`. `launcher_script` takes four arguments in Task 7 and is called with four in the same task; nothing else calls it. `prepare` gains a `&str` in Task 8 for all three shells and `spawn` is the only caller.

**Ordering.** Tasks 3 and 4 are prerequisites of 5; 5 and 6 of 8 and 11; 2 of 7, 9 and 11; 1 of everything. The tree compiles and the suite passes at the end of every task, which is what makes each one reviewable on its own.

---

## Corrections during execution

The provenance record: where the shipped code deliberately differs from this plan, and why.
Written for whoever reads the plan later and finds the tree disagreeing with it. The full
ledger is `.superpowers/sdd/2026-08-06-claude-accounts/progress.md`.

- **A missing-binary guard in `claude_accounts`'s `check()` and `apply()`.** The plan ran
  `ctx.claude()` unguarded. `RealRunner::run` returns `Err` for a binary that is not there,
  the engine reports `NeverRun` without calling `check()`, so `apply()` ran first and `?`
  propagated the spawn error — `install_claude` was never reached and a fresh laptop could
  not be provisioned at all. `github_cli` and `toolchain` both `try_exists` first; the plan
  dropped that when it moved from `which("claude")` to an absolute path. Invisible to the
  suite, because `FakeRunner` answers where `RealRunner` errors.
- **Adoption orders by creation time, not `mtime`.** Claude Code writes into a config
  directory every session, so `mtime` ranks the *least used* account first — the inverse of
  the intent. Now `created()` (APFS `birthtime`, `statx` `btime`) with an `mtime` fallback.
- **The cap is a `Failure`, not a silent no-op.** An unregistered directory on a machine
  already at nine accounts made `apply()` do nothing, which the engine reports as "did not
  take effect" — wedging every later run with nothing actionable. It now names the directory
  and says what to do.
- **`Unknown` is not signed out in `apply()`.** The plan collapsed an unreadable status into
  signed-out, which opened a browser on every run of a machine whose `claude auth status`
  could not be read.
- **`loggedIn` is a three-way match.** `!= Some(Bool(true))` made
  `{"error":"could not reach the server"}` mean "signed out" — the one claim this feature
  promises never to invent. Absent or non-boolean is now `Unknown`.
- **`MIN_VERSION` raised from `2.0.0` to `2.1.223`.** The three Claude Code behaviours this
  rests on were only ever verified against that build, and `install_claude` is unpinned, so
  the floor upgrades a developer rather than blocking one.
- **`--check` is honoured by `new`, `delete` and `primary`.** It was already plumbed to
  `ctx.dry_run`, and `riabuild --check claude delete 3` really deleted account 3.
- **The generated launcher neutralises a non-absolute `claude`.** `Ctx::claude()` can return
  the bare name, and `[ ! -x "claude" ]` then tests a path relative to the *current
  directory* — a checkout holding an executable called `claude` made the launcher `exec`
  itself forever. A `case` guard now runs before the `-x` test.
- **`install_claude` names its prefix.** `npm install -g` puts a binary where the Node that
  *interprets* npm lives, and `bin/npm` is a `#!/usr/bin/env node` script — so on a machine
  with any system Node, Claude Code installed beside that one and `Ctx::claude()` never found
  it. A `prefix` line in a developer's `~/.npmrc` would have done the same. Now `--prefix`
  names riabuild's Node tree on the command line and `PATH` is prefixed with riabuild's own
  `bin` for that call. The retired `claude_profiles` hid this by resolving the binary with
  `which("claude")`; the first `e2e/run.sh` run on the pull request found it.
- **Two tasks the plan lacked.** *9b*: drop the retired `claude_profiles` record from
  `state.json`, which the spec required and no brief implemented. *11b*: teach `e2e/run.sh`
  about accounts — it asserted the `claude_profiles` state key, the retired `claude_profile`
  config field, an executable `bin/c`, and a `CLAUDE_CONFIG_DIR` export this plan removes.
  `.github/workflows/e2e.yml` runs `on: pull_request`, so without it the PR gate was
  permanently red for no real defect.
- **The `c` shim is deleted, not merely unwritten.** `prune` removes `bin/c` on every
  `write_all`, so a machine provisioned before this branch does not keep a launcher that
  points at a model of the world that no longer exists.

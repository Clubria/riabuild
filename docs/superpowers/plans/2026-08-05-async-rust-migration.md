# Async Migration Implementation Plan

> **Completed — historical record, do not execute.** Shipped in #13, 2026-08-06. The
> unchecked `- [ ]` boxes below are how the plan was written and not work outstanding, and
> the instruction to an agentic worker to implement it task-by-task that stood here has
> been removed: acting on it would rebuild something that already ships. See
> [`README.md`](README.md) for the index, and the design spec for what the code does now.

**Goal:** Move `riabuild-cli` to async Rust on a current-thread tokio runtime, and record an invariant in `riabuild-cli/CLAUDE.md` that keeps it there.

**Architecture:** `main` becomes `#[tokio::main(flavor = "current_thread")]`. The three IO traits (`CommandRunner`, `Keychain`, `Task`) gain `#[async_trait]`, which keeps them dyn-compatible so `Ctx` and the whole test suite keep their current shape. `ureq` is replaced by `reqwest`, `std::fs` by `tokio::fs`, `std::process` by `tokio::process`. Execution order does not change: the DAG stays strictly sequential.

**Tech Stack:** Rust 2024 edition, tokio (current_thread), async-trait, reqwest + rustls + rustls-platform-verifier.

**Spec:** [`docs/superpowers/specs/2026-08-05-async-rust-migration-design.md`](../specs/2026-08-05-async-rust-migration-design.md)

## Global Constraints

- **Every task must leave `cargo build` green.** The commit sequence below is ordered so each step compiles on its own. Async is viral; if a task leaves the crate half-converted, the boundary was drawn in the wrong place.
- **The test count must not drop. 136 tests pass today; 136 must pass at the end.** A deleted or `#[ignore]`d test is a migration failure, not a cleanup.
- **No behaviour change** other than the two called out in the spec: TLS trust roots move to the OS store, and the login callback stops polling. Task output order on the terminal must stay byte-identical.
- **Do not bump `version = "0.0.0"` in `Cargo.toml`.** riabuild is versioned from the git tag. This is a standing invariant in `riabuild-cli/CLAUDE.md`.
- **`cargo clippy --all-targets -- -D warnings` must stay clean**, including the `clippy::unwrap_used` deny added in `Cargo.toml` earlier today.
- Run from `riabuild-cli/`: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- **All work goes through a PR and is not finished until PR CI has completed** (root `CLAUDE.md`).

---

## File Structure

| File | Change |
|---|---|
| `riabuild-cli/Cargo.toml` | Add tokio, async-trait, reqwest; drop ureq |
| `riabuild-cli/src/main.rs` | `async fn main`, await the flow functions |
| `riabuild-cli/src/runner.rs` | `#[async_trait] CommandRunner`, `tokio::process` |
| `riabuild-cli/src/keychain.rs` | `#[async_trait] Keychain` |
| `riabuild-cli/src/tasks/mod.rs` | `#[async_trait] Task` |
| `riabuild-cli/src/tasks/*.rs` (10 files) | `async fn check`/`apply`, await runner+keychain |
| `riabuild-cli/src/api/mod.rs` | `reqwest::Client`, async `get_json`/`post_json`/`me` |
| `riabuild-cli/src/api/auth.rs` | `tokio::net::TcpListener`, `timeout` replaces poll loop |
| `riabuild-cli/src/api/org.rs`, `secrets.rs` | await the client |
| `riabuild-cli/src/download.rs` | `reqwest` fetch; extraction stays sync |
| `riabuild-cli/src/config.rs`, `shims/mod.rs`, `shell/*.rs` | `tokio::fs` |
| `riabuild-cli/src/update.rs` | await download + fs |
| `riabuild-cli/src/tasks/engine.rs` | `topological_order` returns waves |
| `riabuild-cli/src/testing.rs` | async fixture helpers |
| `riabuild-cli/CLAUDE.md` | The invariant |

Unchanged: `paths.rs` (pure path computation), `ui.rs` (stdio), `cli.rs`, `version.rs`.

---

### Task 1: Runtime and dependencies

Adds the runtime with no async call sites yet. `async fn main` with a synchronous body compiles fine, so this task stands alone and proves the dependency set builds before anything is rewritten against it.

**Files:**
- Modify: `riabuild-cli/Cargo.toml`
- Modify: `riabuild-cli/src/main.rs:31`

**Interfaces:**
- Consumes: nothing
- Produces: a current-thread tokio runtime wrapping `main`; `reqwest`, `async-trait`, `tokio` available to later tasks

- [x] **Step 1: Record the baseline binary size**

```bash
cd riabuild-cli
cargo build --release
ls -l target/release/riabuild | awk '{print $5}'
```

**Measured on 2026-08-05 at commit `fb2b42a`: 3,088,136 bytes (3.0M).**

This goes in the PR body as the before-size. It is the spec's binary-size risk mitigation and cannot be reconstructed once the dependencies change, which is why it is measured before anything else.

- [ ] **Step 2: Swap the dependencies**

In `riabuild-cli/Cargo.toml`, remove the `ureq` line and add:

```toml
async-trait = "0.1"
reqwest = { version = "0.12", default-features = false, features = [
    "json",
    "rustls-tls-manual-roots",
    "rustls-tls-native-roots",
    "charset",
    "http2",
] }
tokio = { version = "1", default-features = false, features = [
    "rt",
    "macros",
    "fs",
    "process",
    "net",
    "io-util",
    "time",
] }
```

Note `rt` without `rt-multi-thread`: the runtime is current-thread by construction, so a stray `Runtime::new()` cannot silently spawn a worker pool.

- [ ] **Step 3: Make main async**

In `riabuild-cli/src/main.rs`, change line 31 from `fn main() {` to:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
```

Leave the body alone. `run(cli)` is still synchronous and still returns `Result<i32>`.

- [ ] **Step 4: Verify it builds and tests still pass**

```bash
cargo build
cargo test 2>&1 | tail -3
```

Expected: build succeeds; `test result: ok. 136 passed`.

- [ ] **Step 5: Commit**

```bash
git add riabuild-cli/Cargo.toml riabuild-cli/src/main.rs
git commit -m "Add a current-thread tokio runtime"
```

---

### Task 2: The three IO traits go async

The structural task. `async fn` in a trait is not dyn-compatible, and `Ctx` holds all three behind `Arc<dyn …>`/`Box<dyn …>`, so `#[async_trait]` is what keeps the test seam intact. Everything downstream of a trait method becomes async in this task, up to and including `run`/`provision`/`connect` in `main.rs`.

**Files:**
- Modify: `riabuild-cli/src/runner.rs:40-52` (trait), `:69-113` (RealRunner), `:128+` (FakeRunner)
- Modify: `riabuild-cli/src/keychain.rs:18-24` and both impls
- Modify: `riabuild-cli/src/tasks/mod.rs:72-82`
- Modify: all 10 files in `riabuild-cli/src/tasks/`
- Modify: `riabuild-cli/src/tasks/engine.rs:78-99` (`status_for`), `:101+` (`run_all`)
- Modify: `riabuild-cli/src/main.rs` — `run`, `provision`, `connect`, `open_shell`, `logout`, `print_env`
- Modify: `riabuild-cli/src/testing.rs`

**Interfaces:**
- Consumes: the tokio runtime from Task 1
- Produces:
  - `async fn CommandRunner::run(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<CommandOutput>`
  - `async fn CommandRunner::run_interactive(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32>`
  - `fn CommandRunner::which(&self, program: &str) -> Option<PathBuf>` — **stays sync**
  - `async fn Keychain::get(&self) -> Result<Option<String>>`, `set(&self, token: &str) -> Result<()>`, `delete(&self) -> Result<()>`
  - `fn Keychain::describe(&self) -> &'static str` — **stays sync**
  - `async fn Task::check(&self, ctx: &Ctx) -> Result<Status>`, `async fn Task::apply(&self, ctx: &mut Ctx) -> Result<()>`
  - `async fn engine::run_all(tasks: &[Box<dyn Task>], ctx: &mut Ctx) -> Result<Outcome>`
  - `async fn engine::status_for(task: &dyn Task, ctx: &Ctx, applied: &HashSet<TaskId>) -> Result<Status>`

- [ ] **Step 1: Convert the `CommandRunner` trait and `RealRunner`**

In `riabuild-cli/src/runner.rs`, replace `use std::process::{Command, Stdio};` with `use std::process::Stdio;` and `use tokio::process::Command;`, add `use async_trait::async_trait;`, then:

```rust
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str], options: &RunOptions)
        -> Result<CommandOutput>;

    /// Replaces this process's stdio with the child's — used for the
    /// environment shell and for anything that prompts the developer.
    async fn run_interactive(&self, program: &str, args: &[&str], options: &RunOptions)
        -> Result<i32>;

    /// Resolves a program on `PATH`, so `check()` can distinguish "not
    /// installed" from "installed but wrong version". Reads `PATH` and stats
    /// candidates — cheap enough that making it async would infect every
    /// `check()` for no gain.
    fn which(&self, program: &str) -> Option<PathBuf>;
}
```

`RealRunner::build` keeps its shape but now returns `tokio::process::Command`. The impl becomes:

```rust
#[async_trait]
impl CommandRunner for RealRunner {
    async fn run(&self, program: &str, args: &[&str], options: &RunOptions)
        -> Result<CommandOutput>
    {
        let mut command = RealRunner::build(program, args, options);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        command.stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))?;

        if let Some(input) = &options.stdin {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().context("stdin was piped")?;
            stdin.write_all(input.as_bytes()).await?;
            // Dropping the handle closes the pipe; without this the child can
            // block forever waiting for EOF. `infisical export` does exactly that.
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .with_context(|| format!("`{program}` did not finish"))?;

        Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    async fn run_interactive(&self, program: &str, args: &[&str], options: &RunOptions)
        -> Result<i32>
    {
        let status = RealRunner::build(program, args, options)
            .status()
            .await
            .with_context(|| format!("could not start `{program}`"))?;
        Ok(status.code().unwrap_or(1))
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    }
}
```

Two things changed beyond adding `async`. `.expect("stdin was piped")` became `.context(…)?` because `clippy::unwrap_used` is denied. And the explicit `drop(stdin)` is new: `std::process` dropped the handle at the end of the `if let` block, but the async version holds it across an `.await`, and a child reading to EOF would hang without the close.

- [ ] **Step 2: Convert `FakeRunner`**

Add `#[async_trait]` to the `impl CommandRunner for FakeRunner` block and `async` to its `run`/`run_interactive`. The `Mutex<Vec<String>>` call log stays `std::sync::Mutex` — the guard is never held across an `.await`. Do not convert it to `tokio::sync::Mutex`.

- [ ] **Step 3: Convert `Keychain`**

In `riabuild-cli/src/keychain.rs`:

```rust
#[async_trait]
pub trait Keychain: Send + Sync {
    async fn get(&self) -> Result<Option<String>>;
    async fn set(&self, token: &str) -> Result<()>;
    async fn delete(&self) -> Result<()>;
    /// Shown in diagnostics so a developer knows where the token lives.
    fn describe(&self) -> &'static str;
}
```

Add `#[async_trait]` to `impl Keychain for SecurityCliKeychain` and `impl Keychain for MemoryKeychain`; `await` the `self.runner.run(…)` calls inside `SecurityCliKeychain`.

- [ ] **Step 4: Convert `Task`**

In `riabuild-cli/src/tasks/mod.rs`:

```rust
#[async_trait]
pub trait Task: Send + Sync {
    fn id(&self) -> TaskId;
    fn title(&self) -> &str;
    /// Forced-rerun escape hatch for drift `check()` genuinely cannot observe.
    /// `check()` is authoritative; bumping this to paper over a weak check is a
    /// bug in the check.
    fn version(&self) -> u32;
    fn depends_on(&self) -> &[TaskId];
    async fn check(&self, ctx: &Ctx) -> Result<Status>;
    async fn apply(&self, ctx: &mut Ctx) -> Result<()>;
}
```

`Ctx` is unchanged. Then add `#[async_trait]` to each of the 10 task impls in `riabuild-cli/src/tasks/` (`toolchain.rs`, `env_local.rs`, `project.rs`, `claude_profiles.rs`, `github_cli.rs`, `infisical_cli.rs`, `login.rs`, `org_settings.rs`, `repo_status.rs`, plus the `Fake` impl in `engine.rs`'s test module), mark `check`/`apply` `async`, and `.await` every `ctx.runner.run(…)` and `ctx.keychain.…(…)` call.

- [ ] **Step 5: Convert the engine and main**

`status_for` and `run_all` become `async fn` and `.await` the task methods. Execution order is unchanged — still a `for` loop over the flat order.

In `main.rs`, make `run`, `provision`, `connect`, `open_shell`, `logout`, and `print_env` `async fn`, and `.await` them. `main`'s `match run(cli)` becomes `match run(cli).await`.

- [ ] **Step 6: Convert the tests**

Every `#[test]` that calls a trait method becomes `#[tokio::test]` and its body `.await`s. Leave pure-logic tests (`version.rs`, `topological_order`, `ui.rs` rendering) as plain `#[test]` — making them async is noise.

- [ ] **Step 7: Verify**

```bash
cargo build
cargo test 2>&1 | tail -3
```

Expected: `test result: ok. 136 passed`. If the count is lower, a test was dropped — find it before continuing.

- [ ] **Step 8: Commit**

```bash
git add riabuild-cli/src
git commit -m "Make the CommandRunner, Keychain and Task traits async"
```

---

### Task 3: HTTP moves to reqwest

**Files:**
- Modify: `riabuild-cli/src/api/mod.rs:101-190`
- Modify: `riabuild-cli/src/api/org.rs`, `riabuild-cli/src/api/secrets.rs`
- Modify: `riabuild-cli/src/download.rs:105-122`
- Modify: `riabuild-cli/src/update.rs`

**Interfaces:**
- Consumes: async `Task`/`Ctx` call sites from Task 2
- Produces:
  - `async fn ApiClient::get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T>`
  - `async fn ApiClient::post_json<T: DeserializeOwned>(&self, path: &str, body: serde_json::Value) -> Result<T>`
  - `async fn ApiClient::me(&self) -> Result<Member>`
  - `async fn download::fetch_bytes(url: &str) -> Result<Vec<u8>>`
  - `async fn download::fetch_text(url: &str) -> Result<String>`
  - `fn download::sha256_hex(bytes: &[u8]) -> String` and both `extract_*_tarball` — **stay sync**

- [ ] **Step 1: Rebuild `ApiClient` on reqwest**

`ApiClient` gains a `client: reqwest::Client` field built once in `new()`. `#[derive(Debug, Clone)]` still works — `reqwest::Client` is both, and cloning it shares the connection pool rather than copying it.

```rust
impl ApiClient {
    pub fn new(version: impl Into<String>) -> Self {
        let version = version.into();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(format!("riabuild/{version}"))
            .use_rustls_tls()
            .tls_built_in_native_certs(true)
            .build()
            .unwrap_or_default();
        Self { api_url: api_url(), token: None, version, client }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .request(method, format!("{}{path}", self.api_url))
            .header("x-riabuild-cli-version", &self.version);
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        interpret(self.request(reqwest::Method::GET, path).send().await, path).await
    }

    pub async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        interpret(
            self.request(reqwest::Method::POST, path).json(&body).send().await,
            path,
        )
        .await
    }

    /// `GET /api/v1/me`
    pub async fn me(&self) -> Result<Member> {
        #[derive(Deserialize)]
        struct Envelope {
            member: Member,
        }
        Ok(self.get_json::<Envelope>("/api/v1/me").await?.member)
    }
}
```

`unwrap_or_default()` on the builder rather than `.unwrap()`: the deny added earlier today forbids the unwrap, and a client that failed to build should not panic a provisioner during startup.

- [ ] **Step 2: Rewrite `interpret` for reqwest's error model**

This is the one place the shape genuinely differs. `ureq` signalled HTTP failure through `Err(Error::Status(..))`; `reqwest` returns `Ok(response)` and expects you to inspect `.status()`. Getting this wrong would silently treat every 4xx as success.

```rust
async fn interpret<T: serde::de::DeserializeOwned>(
    result: Result<reqwest::Response, reqwest::Error>,
    path: &str,
) -> Result<T> {
    let response = result.with_context(|| format!("riabuild could not reach {path}"))?;
    let status = response.status();

    if status.is_success() {
        return response
            .json::<T>()
            .await
            .with_context(|| format!("riabuild could not read the reply from {path}"));
    }

    // A structured error is the server explaining itself; anything else is a
    // proxy or an outage, and gets a generic shape.
    let status = status.as_u16();
    match response.json::<ErrorEnvelope>().await {
        Ok(envelope) => {
            let mut error = envelope.error;
            error.status = status;
            Err(error.into())
        }
        Err(_) => Err(ApiError {
            status,
            code: "upstream_error".into(),
            message: format!("riabuild.clubria.com replied with HTTP {status}."),
            action: "Try again in a minute; if it persists, tell your team lead.".into(),
        }
        .into()),
    }
}
```

- [ ] **Step 3: Convert `download.rs`**

```rust
pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .use_rustls_tls()
        .tls_built_in_native_certs(true)
        .build()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("could not download {url}"))?
        .error_for_status()
        .with_context(|| format!("could not download {url}"))?;

    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("download of {url} was cut short"))?;

    if bytes.len() > 400 * 1024 * 1024 {
        return Err(anyhow!("{url} is larger than riabuild will download"));
    }
    Ok(bytes.to_vec())
}

pub async fn fetch_text(url: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&fetch_bytes(url).await?).into_owned())
}
```

`sha256_hex`, `extract_tarball`, `extract_node_tarball`, and `extract_pnpm_tarball` stay synchronous — they are CPU work over an in-memory `&[u8]`, not IO. The checksum is still verified against the complete buffer before extraction; do not stream to disk.

- [ ] **Step 4: Await at the call sites**

`org.rs`, `secrets.rs`, `update.rs`, and the tasks that download toolchains now `.await` these. The compiler lists every one.

- [ ] **Step 5: Verify**

```bash
cargo build
cargo test 2>&1 | tail -3
```

Expected: `test result: ok. 136 passed`.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/src
git commit -m "Replace ureq with reqwest on the platform TLS verifier"
```

---

### Task 4: The login callback stops polling

The clearest win in the migration, and worth its own commit so it is visible in the history.

**Files:**
- Modify: `riabuild-cli/src/api/auth.rs:22-25`, `:140-201`, `:240`

**Interfaces:**
- Consumes: async `ApiClient` from Task 3
- Produces: `async fn wait_for_code(listener: &tokio::net::TcpListener, expected_state: &str) -> Result<String>`

- [ ] **Step 1: Swap the listener type**

Replace `use std::net::{TcpListener, TcpStream};` with `use tokio::net::{TcpListener, TcpStream};`. The bind at line 240 becomes `TcpListener::bind("127.0.0.1:0").await`, keeping its existing `Failure` mapping verbatim.

- [ ] **Step 2: Replace the poll loop with one timeout**

```rust
/// Waits for the browser to come back. Rejects any callback whose `state` is not
/// the one this process generated.
async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    let wait = async {
        loop {
            let (mut stream, _) = listener.accept().await?;

            let mut line = String::new();
            let mut reader = BufReader::new(&mut stream);
            // A connection that opens and then says nothing must not hold the
            // whole login hostage; the outer timeout would still fire, but this
            // keeps one stalled probe from blocking a real callback behind it.
            if timeout(Duration::from_secs(5), reader.read_line(&mut line))
                .await
                .is_err()
            {
                continue;
            }

            match parse_callback(line.trim_end()) {
                Some((code, state)) if state == expected_state => {
                    respond(
                        &mut stream,
                        "You are signed in.",
                        "riabuild has what it needs. You can close this tab.",
                    )
                    .await;
                    return Ok(code);
                }
                Some(_) => {
                    respond(
                        &mut stream,
                        "That did not come from riabuild.",
                        "The sign-in was not the one this terminal started. Run <code>riabuild login</code> again.",
                    )
                    .await;
                    return Err(anyhow!(
                        "the browser came back with a sign-in riabuild did not start"
                    ));
                }
                None => {
                    // Favicon requests and stray probes land here.
                    respond(&mut stream, "riabuild", "Nothing to do here.").await;
                }
            }
        }
    };

    timeout(LOGIN_TIMEOUT, wait)
        .await
        .map_err(|_| anyhow!("no reply from the browser within three minutes"))?
}
```

Imports become `use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};` and `use tokio::time::{Duration, timeout};`. `Instant` is no longer needed.

The `timeout` must wrap the whole loop, not a single `accept`. Stray probes `continue`, and a per-accept timeout would hand each one a fresh three-minute budget.

- [ ] **Step 3: Make `respond` async**

`respond` becomes `async fn` and its `write_all`/`flush` become `.await`ed `AsyncWriteExt` calls. It keeps swallowing errors — a browser that hangs up before reading the courtesy page must not fail a login that already succeeded.

- [ ] **Step 4: Delete the dead knobs**

`listener.set_nonblocking(true)` and `stream.set_read_timeout(…)` are gone: tokio's listener is non-blocking internally and the read timeout is now explicit. Confirm no `std::thread::sleep` remains in the file.

```bash
grep -n "thread::sleep\|set_nonblocking\|set_read_timeout\|Instant" riabuild-cli/src/api/auth.rs
```

Expected: no output.

- [ ] **Step 5: Verify and commit**

```bash
cargo test 2>&1 | tail -3
git add riabuild-cli/src/api/auth.rs
git commit -m "Wait for the login callback with a timeout, not a poll loop"
```

---

### Task 5: Filesystem call sites

Mechanical, and the largest by count: ~50 sites across 12 files.

**Files:**
- Modify: `riabuild-cli/src/config.rs`, `download.rs`, `shims/mod.rs`, `shell/{bash,zsh,fish}.rs`, `main.rs`, `testing.rs`, `tasks/{org_settings,project,claude_profiles,env_local,toolchain}.rs`

**Interfaces:**
- Consumes: async call sites from Tasks 2–4
- Produces: no signature changes beyond functions becoming `async fn` where they now await

- [ ] **Step 1: Apply the transformation**

Every one of these is `tokio::fs::<same name>(…).await`, with the same arguments and the same error type:

| From | To |
|---|---|
| `std::fs::read_to_string(p)?` | `tokio::fs::read_to_string(p).await?` |
| `std::fs::read(p)?` | `tokio::fs::read(p).await?` |
| `std::fs::write(p, c)?` | `tokio::fs::write(p, c).await?` |
| `std::fs::create_dir_all(p)?` | `tokio::fs::create_dir_all(p).await?` |
| `std::fs::remove_file(p)?` | `tokio::fs::remove_file(p).await?` |
| `std::fs::remove_dir_all(p)?` | `tokio::fs::remove_dir_all(p).await?` |
| `std::fs::rename(a, b)?` | `tokio::fs::rename(a, b).await?` |
| `std::fs::metadata(p)?` | `tokio::fs::metadata(p).await?` |
| `std::fs::set_permissions(p, m)?` | `tokio::fs::set_permissions(p, m).await?` |

Two that are **not** mechanical:

- `path.exists()` → `tokio::fs::try_exists(path).await.unwrap_or(false)`. `Path::exists` is an inherent sync method that swallows errors; `try_exists` distinguishes "absent" from "cannot tell", and `.unwrap_or(false)` preserves today's behaviour exactly.
- `path.is_file()` / `path.is_dir()` → `tokio::fs::metadata(path).await.map(|m| m.is_file()).unwrap_or(false)`.

Leave alone: `std::fs::Permissions` and `PermissionsExt` (types, not IO), and the `tar` crate's synchronous extraction over in-memory bytes.

- [ ] **Step 2: Confirm nothing was missed**

```bash
grep -rn "std::fs::" riabuild-cli/src/ | grep -v "std::fs::Permissions"
```

Expected: no output.

- [ ] **Step 3: Verify and commit**

```bash
cargo clippy --all-targets -- -D warnings
cargo test 2>&1 | tail -3
git add riabuild-cli/src
git commit -m "Move filesystem access to tokio::fs"
```

---

### Task 6: Preserve the dependency waves

Groundwork only. Execution stays sequential and output stays byte-identical; this stops the graph from throwing away structure it already computes.

**Files:**
- Modify: `riabuild-cli/src/tasks/engine.rs:19-75` (`topological_order`), `:101+` (`run_all`)
- Test: `riabuild-cli/src/tasks/engine.rs` test module

**Interfaces:**
- Produces: `fn topological_order(tasks: &[Box<dyn Task>]) -> Result<Vec<Vec<usize>>>` — each inner `Vec` is one wave of tasks whose dependencies are all satisfied by earlier waves

- [ ] **Step 1: Write the failing test**

Add to the `engine.rs` test module:

```rust
#[test]
fn independent_tasks_land_in_the_same_wave() {
    // `a` and `b` depend on nothing; `c` depends on both. Two waves.
    let tasks: Vec<Box<dyn Task>> = vec![
        Box::new(Fake::new("a", &[])),
        Box::new(Fake::new("b", &[])),
        Box::new(Fake::new("c", &["a", "b"])),
    ];

    let waves = topological_order(&tasks).unwrap();

    assert_eq!(waves.len(), 2, "a and b are independent and belong together");
    assert_eq!(waves[0], vec![0, 1]);
    assert_eq!(waves[1], vec![2]);
}
```

Match `Fake`'s actual constructor in the existing test module — if it is built with struct literal syntax rather than `Fake::new`, use that form.

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo test independent_tasks_land_in_the_same_wave 2>&1 | tail -5
```

Expected: FAIL — `topological_order` returns `Vec<usize>`, so this does not compile.

- [ ] **Step 3: Return waves instead of a flat order**

In `topological_order`, change `let mut order = Vec::with_capacity(tasks.len());` to `let mut waves: Vec<Vec<usize>> = Vec::new();`, and replace the drain block at lines 67-71 with:

```rust
        for position in &ready {
            remaining.remove(position);
            done.insert(tasks[*position].id());
        }
        waves.push(ready);
```

Return `Ok(waves)`. Both error paths — the duplicate-id check and the cycle check — are untouched.

The `BTreeSet` iteration that makes `ready` deterministic is load-bearing and must stay: the same graph has to produce the same order every run, or a developer's terminal output shuffles between invocations.

- [ ] **Step 4: Flatten at the one consumer**

In `run_all`, change `for position in order {` to:

```rust
    for position in order.into_iter().flatten() {
```

Sequential execution, identical order, identical output.

- [ ] **Step 5: Verify**

```bash
cargo test 2>&1 | tail -3
```

Expected: `test result: ok. 137 passed` — the 136 existing plus the new wave test. Any existing test asserting on `topological_order`'s return type needs updating to the nested shape; that is a signature change, not a behaviour change.

- [ ] **Step 6: Commit**

```bash
git add riabuild-cli/src/tasks/engine.rs
git commit -m "Keep the dependency waves topological_order already computes"
```

---

### Task 7: The invariant, and the size measurement

**Files:**
- Modify: `riabuild-cli/CLAUDE.md` (Invariants section)

**Interfaces:**
- Consumes: everything above

- [ ] **Step 1: Add the invariant**

In `riabuild-cli/CLAUDE.md`, in the **Invariants** section, immediately after the "Every external process goes through `CommandRunner`" paragraph:

```markdown
**All IO is async.** riabuild runs on a current-thread tokio runtime. Filesystem work
goes through `tokio::fs`, HTTP through `reqwest`, and subprocesses through
`tokio::process` — never `std::fs` or `std::process`. Mixing a blocking call into the
runtime thread stalls every other future on it, and the symptom is a provisioner that
hangs on someone else's laptop with no output and no error.

The exception is **stdio**. `ui.rs` writes with `println!`/`eprintln!`, and
`run_interactive` hands the terminal to a child process — that is a handoff, not IO
riabuild performs. Async stdout buys nothing for line-at-a-time terminal output.

Three things are synchronous because they are not IO, and are not exceptions to
anything: `paths.rs` computes paths without touching the disk, `CommandRunner::which`
stats `PATH` candidates, and tarball extraction is CPU work over an in-memory buffer.

Note that `tokio::fs` is `std::fs` on a blocking threadpool — there is no portable async
file API. "Current-thread" describes the reactor, not the process.
```

- [ ] **Step 2: Measure the release binary**

```bash
cd riabuild-cli
cargo build --release
ls -l target/release/riabuild | awk '{print $5}'
```

Compare against the baseline recorded in Task 1, Step 1. The delta goes in the PR body. If it is large enough to be worth reconsidering `reqwest`, say so explicitly rather than absorbing it quietly — the spec names this as the migration's main risk.

- [ ] **Step 3: Full verification**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test 2>&1 | tail -3
```

Expected: fmt clean, clippy clean, `137 passed`.

- [ ] **Step 4: Confirm the runtime is really current-thread**

```bash
grep -n "rt-multi-thread\|flavor" riabuild-cli/Cargo.toml riabuild-cli/src/main.rs
```

Expected: `flavor = "current_thread"` in `main.rs`, and no `rt-multi-thread` anywhere.

- [ ] **Step 5: Commit and open the PR**

```bash
git add riabuild-cli/CLAUDE.md
git commit -m "Record the always-async invariant"
git push -u origin <branch>
gh pr create --fill
gh pr checks --watch
```

The PR body must state the before/after binary size and call out the TLS trust-root change. **The task is not done until CI is green** (root `CLAUDE.md`).

---

## Self-Review

**Spec coverage.** Every section maps to a task: runtime → 1; HTTP, downloads, trust roots → 3; traits and the sync exemptions → 2; loopback listener → 4; filesystem → 5; engine waves → 6; the invariant → 7. Testing constraints are in Global Constraints and re-asserted per task. The binary-size risk is measured in 1 and reported in 7. Deferred items (Ctx split, lint enforcement, streaming downloads) have no tasks, correctly.

**Placeholders.** None. The ~50 filesystem conversions are given as a complete mechanical mapping plus an exhaustive file list and a `grep` that proves completion, rather than transcribed individually.

**Type consistency.** `CommandRunner::run`/`run_interactive` async and `which` sync, consistent across Tasks 2 and 5. `ApiClient::get_json`/`post_json`/`me` async in Task 3 and awaited in 3–4. `topological_order` returns `Vec<Vec<usize>>` in Task 6 and is flattened at its one consumer in the same task. `fetch_bytes`/`fetch_text` async; `sha256_hex` and `extract_*_tarball` sync throughout.

**Known count change.** The test count is 136 through Tasks 1–5 and 137 after Task 6 adds the wave test. Task 6 flags that any existing assertion on `topological_order`'s return type needs the nested shape.

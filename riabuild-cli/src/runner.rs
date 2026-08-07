//! Every external process riabuild starts goes through here.
//!
//! This is the single decision that makes the rest of the crate testable: with
//! it, each `check()` is a pure unit test against canned `gh`, `git`, `node` and
//! `claude` output. Without it, every test needs a real machine in a real state,
//! and the suite gets abandoned.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout with trailing newline removed — what a `--version` check wants.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim()
    }
}

/// Subprocess output whose stdout is not assumed to be text.
///
/// `CommandOutput` exists for the `--version` and status checks that make up
/// most of riabuild, and its lossy `String` conversion is right for those. The
/// clipboard channel moves PNGs, where a single replacement character is a
/// corrupt image, so it reads through here instead. stderr stays a `String`:
/// it is diagnostics, it is always text, and every caller puts it in a message.
#[derive(Debug, Clone)]
pub struct BytesOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl BytesOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Fed to the child's stdin. Used to pipe brokered secrets without them ever
    /// appearing in a process argument list, where `ps` would show them, and to
    /// hand a clipboard write to `xclip -i`.
    ///
    /// Bytes rather than a `String` for the same reason `run_bytes` exists: a
    /// `String` cannot represent a PNG at all, so an image write would not be
    /// merely lossy, it would be unconstructible.
    pub stdin: Option<Vec<u8>>,
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput>;

    /// Like `run`, but stdout is returned as raw bytes.
    ///
    /// Used by the clipboard channel, where stdout is a PNG rather than a
    /// version string, and `from_utf8_lossy` would replace every byte that is
    /// not valid UTF-8 with U+FFFD.
    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput>;

    /// Feeds stdin to a command that forks a child to outlive it, and waits
    /// only for the process actually started.
    ///
    /// `xclip -i` and `wl-copy` fork into the background to *serve* the
    /// selection they were given, and that fork inherits whatever stdout it was
    /// handed. `run` and `run_bytes` finish by reading stdout to EOF, which for
    /// these two arrives only when the selection is replaced — so a clipboard
    /// write through either would hang for as long as the copy stayed current.
    ///
    /// The cost is that stderr is unavailable: the fork holds that pipe too, so
    /// the only diagnostic a write can carry is its exit status.
    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32>;

    /// Replaces this process's stdio with the child's — used for the
    /// environment shell and for anything that prompts the developer.
    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32>;

    /// Resolves a program on `PATH`, so `check()` can distinguish "not
    /// installed" from "installed but wrong version".
    ///
    /// Stays synchronous: it reads `PATH` and stats the candidates, which is
    /// cheap enough that making it async would infect every `check()` for no
    /// gain.
    fn which(&self, program: &str) -> Option<PathBuf>;
}

pub struct RealRunner;

impl RealRunner {
    fn build(program: &str, args: &[&str], options: &RunOptions) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        if let Some(cwd) = &options.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &options.env {
            command.env(key, value);
        }
        command
    }
}

#[async_trait]
impl CommandRunner for RealRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
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
            stdin.write_all(input).await?;
            // Closing the pipe is what tells the child there is no more input.
            // The blocking version got this free when the `if let` block ended;
            // here the handle would otherwise live until the end of the
            // function, and a child reading to EOF — `infisical export` does —
            // would wait forever.
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

    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput> {
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
            stdin.write_all(input).await?;
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .with_context(|| format!("`{program}` did not finish"))?;

        Ok(BytesOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32> {
        let mut command = RealRunner::build(program, args, options);
        // Null rather than piped: a pipe handed to the fork is exactly what
        // would keep this call waiting for a selection nobody is going to
        // replace.
        command.stdout(Stdio::null()).stderr(Stdio::null());
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
            stdin.write_all(input).await?;
            // The fork does not take ownership of the selection until it has
            // read the content to EOF, so closing this is what completes the
            // copy rather than merely tidying up.
            drop(stdin);
        }

        let status = child
            .wait()
            .await
            .with_context(|| format!("`{program}` did not finish"))?;
        Ok(status.code().unwrap_or(1))
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
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

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
/// One scripted response, and the conditions it answers to.
struct Stub {
    invocation: String,
    /// Environment entries that must all be present for this stub to apply.
    /// Empty means "any environment", which is what `with` produces.
    env: Vec<(String, String)>,
    output: CommandOutput,
    /// Raw stdout, for a command whose output is not text. `None` falls back to
    /// `output.stdout`, so a test that only cares about the exit code can stub
    /// with `with` and still be read through `run_bytes`.
    bytes: Option<Vec<u8>>,
}

#[cfg(test)]
/// Scripted `CommandRunner` for tests.
///
/// Each `Stub` is matched by `"program arg1 arg2"` prefix; the longest matching
/// prefix wins, so a test can stub `gh auth status` and `gh --version`
/// independently.
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
    /// What each call was given on stdin. A clipboard write is *only* its
    /// stdin, so without this a test could assert the invocation and still not
    /// know whether the bytes survived.
    inputs: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
}

#[cfg(test)]
impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

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
            bytes: None,
        });
        let program = invocation.split_whitespace().next().unwrap_or_default();
        if !self.available.iter().any(|p| p == program) {
            self.available.push(program.to_string());
        }
        self
    }

    /// Scripts a command whose stdout is binary.
    ///
    /// One stub, like every other, so `which`, the exit code and the env
    /// matching all resolve through exactly the same path as `with` — only
    /// stdout differs.
    pub fn with_bytes(mut self, invocation: &str, code: i32, stdout: &[u8], stderr: &str) -> Self {
        self = self.with(invocation, code, "", stderr);
        if let Some(stub) = self.responses.last_mut() {
            stub.bytes = Some(stdout.to_vec());
        }
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The stdin every call was given, as `(invocation, bytes)`.
    pub fn inputs(&self) -> Vec<(String, Vec<u8>)> {
        self.inputs.lock().unwrap().clone()
    }

    /// The bytes piped into the first call whose invocation contains `needle`.
    pub fn input_for(&self, needle: &str) -> Option<Vec<u8>> {
        self.inputs()
            .into_iter()
            .find(|(invocation, _)| invocation.contains(needle))
            .map(|(_, bytes)| bytes)
    }

    fn record(&self, program: &str, args: &[&str], options: &RunOptions) -> String {
        let invocation = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.calls.lock().unwrap().push(invocation.clone());
        if let Some(input) = &options.stdin {
            self.inputs
                .lock()
                .unwrap()
                .push((invocation.clone(), input.clone()));
        }
        invocation
    }

    /// The invocation a stub is matched against.
    ///
    /// The program is reduced to its file name, because riabuild runs the tools
    /// it owns by absolute path — `~/.riabuild/gh/2.97.0/bin/gh`, under a
    /// per-test tempdir. Without this every stub would have to be built from a
    /// path the test does not care about, and `with("gh --version", …)` would
    /// stop meaning "when gh is asked its version".
    ///
    /// `calls` still records the full path, so a test can assert riabuild ran
    /// *its* gh rather than whatever is on `PATH`.
    fn stub_key(program: &str, args: &[&str]) -> String {
        let name = Path::new(program)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string());
        format!("{name} {}", args.join(" ")).trim_end().to_string()
    }

    /// Finds a stub for an invocation, by full program path or by file name.
    ///
    /// Both, because tasks run some binaries by absolute path and others by
    /// name, and a test should be able to say whichever it means. `toolchain`
    /// stubs the exact `~/.riabuild/node/<version>/bin/node` it is asserting
    /// about; `github_cli` says `gh --version` and does not care where gh is.
    fn resolve(&self, program: &str, args: &[&str], options: &RunOptions) -> Option<CommandOutput> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.stubbed(&full, options)
            .or_else(|| self.stubbed(&FakeRunner::stub_key(program, args), options))
    }

    fn stubbed(&self, invocation: &str, options: &RunOptions) -> Option<CommandOutput> {
        self.matching(invocation, options)
            .map(|stub| stub.output.clone())
    }

    /// The stub an invocation selects, if any.
    ///
    /// Shared by the text and byte lookups so a binary stub can never be
    /// selected by different rules from the text one beside it.
    fn matching(&self, invocation: &str, options: &RunOptions) -> Option<&Stub> {
        self.responses
            .iter()
            .filter(|stub| {
                let name_matches = invocation == stub.invocation
                    || invocation.starts_with(&format!("{} ", stub.invocation));
                name_matches
                    && stub
                        .env
                        .iter()
                        .all(|(key, value)| options.env.iter().any(|(k, v)| k == key && v == value))
            })
            // Most specific wins: the longest command, then the most
            // environment entries. `max_by_key` keeps the last of equal
            // candidates, so a later identical stub replaces an earlier one —
            // which is what the map this replaced did.
            //
            // Command length is compared before env-pair count, so a longer
            // env-less stub outranks a shorter env-scoped one: `with("claude
            // auth status --json")` beats `with_env("claude auth", &[("CLAUDE_
            // CONFIG_DIR", "/one")])` for `claude auth status --json` run in
            // `/one`. That is fine for the account use case, where every
            // account is asked the identical command string, but it means env
            // specificity only breaks ties between stubs that already match on
            // the same invocation length.
            .max_by_key(|stub| (stub.invocation.len(), stub.env.len()))
    }

    /// The byte-stub twin of `resolve`, matched by exactly the same rules so a
    /// binary stub cannot be selected differently from a text one.
    fn resolve_bytes(&self, program: &str, args: &[&str], options: &RunOptions) -> Option<Vec<u8>> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.matching(&full, options)
            .or_else(|| self.matching(&FakeRunner::stub_key(program, args), options))
            .and_then(|stub| stub.bytes.clone())
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
}

#[cfg(test)]
#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
        self.record(program, args, options);
        Ok(self.lookup(program, args, options))
    }

    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput> {
        self.record(program, args, options);

        let text = self.lookup(program, args, options);
        // A test that only cares about the exit code can stub with `with` and
        // still be read through `run_bytes`.
        let stdout = self
            .resolve_bytes(program, args, options)
            .unwrap_or_else(|| text.stdout.into_bytes());

        Ok(BytesOutput {
            code: text.code,
            stdout,
            stderr: text.stderr,
        })
    }

    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32> {
        self.record(program, args, options);
        Ok(self.lookup(program, args, options).code.unwrap_or(0))
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        self.record(program, args, options);
        // A stub's exit code applies here too: interactive commands fail as
        // well — a developer who abandons a device-code prompt leaves `gh`
        // exiting non-zero — and a task that ignores that reports a sign-in
        // it never got. Unstubbed commands still succeed, so tests that only
        // care about which commands ran need not script every prompt.
        Ok(self
            .resolve(program, args, options)
            .and_then(|output| output.code)
            .unwrap_or(0))
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.available
            .iter()
            .any(|p| p == program)
            .then(|| PathBuf::from(format!("/usr/bin/{program}")))
    }
}

#[cfg(test)]
mod bytes_tests {
    use super::*;

    /// A PNG is not valid UTF-8. Read through `run`, its bytes come back
    /// mangled into replacement characters; `run_bytes` is what makes the
    /// clipboard channel possible at all.
    #[tokio::test]
    async fn binary_stdout_survives_the_runner() {
        // PNG magic, then a byte that is illegal as UTF-8 on its own.
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF];
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, &png, "");

        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();

        assert!(out.ok());
        assert_eq!(out.stdout, png);
    }

    /// The bug this whole method exists to avoid, pinned against the real
    /// runner so nobody "simplifies" the clipboard backends back onto `run`.
    #[tokio::test]
    async fn the_same_bytes_through_run_would_have_been_corrupted() {
        let png = [0x89u8, b'P', b'N', b'G', 0xFF];
        let emit = ["-c", r"printf '\211PNG\377'"];

        let lossy = RealRunner
            .run("sh", &emit, &RunOptions::default())
            .await
            .unwrap();
        let raw = RealRunner
            .run_bytes("sh", &emit, &RunOptions::default())
            .await
            .unwrap();

        assert_eq!(raw.stdout, png);
        assert_ne!(lossy.stdout.as_bytes(), png);
        assert!(
            lossy.stdout.contains('\u{FFFD}'),
            "expected replacement characters, got {:?}",
            lossy.stdout
        );
    }

    #[tokio::test]
    async fn an_unstubbed_command_fails_the_same_way_as_run() {
        let runner = FakeRunner::new();
        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(out.code, Some(127));
        assert!(out.stdout.is_empty());
        assert!(out.stderr.contains("no stub"), "{}", out.stderr);
    }

    #[tokio::test]
    async fn bytes_calls_are_recorded_like_every_other_call() {
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, b"hi", "");
        runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(runner.calls(), vec!["xclip -o".to_string()]);
    }

    /// The real runner is exercised through a shell builtin every supported
    /// platform has, so this stays a unit test rather than a fixture.
    #[tokio::test]
    async fn the_real_runner_returns_raw_bytes() {
        let out = RealRunner
            .run_bytes(
                "sh",
                &["-c", r"printf '\211PNG\377'"],
                &RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout, [0x89u8, b'P', b'N', b'G', 0xFF]);
    }
}

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
            .run(
                "claude",
                &["auth", "status", "--json"],
                &in_dir("/elsewhere"),
            )
            .await
            .unwrap();
        assert_eq!(other.code, Some(1));
    }

    #[tokio::test]
    async fn an_invocation_naming_no_account_matches_no_account_specific_stub() {
        // Pins the direction of the match: requiring an env pair is a real
        // requirement, not merely a tie-breaker. A caller that names no
        // `CLAUDE_CONFIG_DIR` at all must come away empty-handed even though
        // two stubs exist for this exact command — each scoped to a different
        // account. If this ever passed, the account-lookup feature could go
        // green in its own tests while the production code never actually
        // threaded `CLAUDE_CONFIG_DIR` through to `claude auth status --json`
        // — every account would silently be answered by whichever stub ranks
        // first.
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

        let unscoped = runner
            .run(
                "claude",
                &["auth", "status", "--json"],
                &RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(unscoped.code, Some(127), "{unscoped:?}");
        assert!(
            unscoped
                .stderr
                .contains("fake runner: no stub for `claude auth status --json`"),
            "{unscoped:?}"
        );
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

    /// A binary stub is selected by exactly the same rules as a text one, so
    /// the byte lookup cannot quietly diverge from the command that ran.
    #[tokio::test]
    async fn a_byte_stub_can_be_scoped_to_an_environment_variable_too() {
        let runner = FakeRunner::new()
            .with_bytes("xclip -o", 0, b"\x89PNG\xFF", "")
            .with_env("xclip -o", &[("DISPLAY", ":1")], 1, "", "no display");

        let scoped = runner
            .run_bytes("xclip", &["-o"], &in_env("DISPLAY", ":1"))
            .await
            .unwrap();
        assert_eq!(scoped.code, Some(1), "the env-scoped stub should win");

        let plain = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(plain.stdout, b"\x89PNG\xFF");
    }

    fn in_env(key: &str, value: &str) -> RunOptions {
        RunOptions {
            env: vec![(key.to_string(), value.to_string())],
            ..Default::default()
        }
    }
}

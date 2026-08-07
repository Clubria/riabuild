//! Every external process riabuild starts goes through here.
//!
//! This is the single decision that makes the rest of the crate testable: with
//! it, each `check()` is a pure unit test against canned `gh`, `git`, `node` and
//! `claude` output. Without it, every test needs a real machine in a real state,
//! and the suite gets abandoned.

use anyhow::{Context, Result};
use async_trait::async_trait;
#[cfg(test)]
use std::collections::HashMap;
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
    /// appearing in a process argument list, where `ps` would show them.
    pub stdin: Option<String>,
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
            stdin.write_all(input.as_bytes()).await?;
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
            stdin.write_all(input.as_bytes()).await?;
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
/// Scripted `CommandRunner` for tests.
///
/// Keys are `"program arg1 arg2"` prefixes; the longest matching prefix wins, so
/// a test can stub `gh auth status` and `gh --version` independently.
#[derive(Default)]
pub struct FakeRunner {
    responses: HashMap<String, CommandOutput>,
    byte_responses: HashMap<String, Vec<u8>>,
    available: Vec<String>,
    pub calls: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, invocation: &str, code: i32, stdout: &str, stderr: &str) -> Self {
        self.responses.insert(
            invocation.to_string(),
            CommandOutput {
                code: Some(code),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            },
        );
        let program = invocation.split_whitespace().next().unwrap_or_default();
        if !self.available.iter().any(|p| p == program) {
            self.available.push(program.to_string());
        }
        self
    }

    /// Scripts a command whose stdout is binary.
    ///
    /// Registers a text stub too, so `which` and the exit code resolve through
    /// exactly the same path as `with` and only stdout differs.
    pub fn with_bytes(mut self, invocation: &str, code: i32, stdout: &[u8], stderr: &str) -> Self {
        self.byte_responses
            .insert(invocation.to_string(), stdout.to_vec());
        self.with(invocation, code, "", stderr)
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
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
    fn resolve(&self, program: &str, args: &[&str]) -> Option<CommandOutput> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.stubbed(&full)
            .or_else(|| self.stubbed(&FakeRunner::stub_key(program, args)))
    }

    fn stubbed(&self, invocation: &str) -> Option<CommandOutput> {
        let mut best: Option<(&String, &CommandOutput)> = None;
        for (key, value) in &self.responses {
            if invocation == key || invocation.starts_with(&format!("{key} ")) {
                let better = best.map(|(k, _)| key.len() > k.len()).unwrap_or(true);
                if better {
                    best = Some((key, value));
                }
            }
        }
        best.map(|(_, output)| output.clone())
    }

    /// The byte-stub twin of `stubbed`, with the same longest-prefix rule.
    fn stubbed_bytes(&self, invocation: &str) -> Option<Vec<u8>> {
        let mut best: Option<(&String, &Vec<u8>)> = None;
        for (key, value) in &self.byte_responses {
            if invocation == key || invocation.starts_with(&format!("{key} ")) {
                let better = best.map(|(k, _)| key.len() > k.len()).unwrap_or(true);
                if better {
                    best = Some((key, value));
                }
            }
        }
        best.map(|(_, bytes)| bytes.clone())
    }

    fn resolve_bytes(&self, program: &str, args: &[&str]) -> Option<Vec<u8>> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.stubbed_bytes(&full)
            .or_else(|| self.stubbed_bytes(&FakeRunner::stub_key(program, args)))
    }

    fn lookup(&self, program: &str, args: &[&str]) -> CommandOutput {
        self.resolve(program, args)
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
        _options: &RunOptions,
    ) -> Result<CommandOutput> {
        let invocation = format!("{program} {}", args.join(" "));
        self.calls
            .lock()
            .unwrap()
            .push(invocation.trim_end().to_string());
        Ok(self.lookup(program, args))
    }

    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        _options: &RunOptions,
    ) -> Result<BytesOutput> {
        let invocation = format!("{program} {}", args.join(" "));
        self.calls
            .lock()
            .unwrap()
            .push(invocation.trim_end().to_string());

        let text = self.lookup(program, args);
        // A test that only cares about the exit code can stub with `with` and
        // still be read through `run_bytes`.
        let stdout = self
            .resolve_bytes(program, args)
            .unwrap_or_else(|| text.stdout.into_bytes());

        Ok(BytesOutput {
            code: text.code,
            stdout,
            stderr: text.stderr,
        })
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        _options: &RunOptions,
    ) -> Result<i32> {
        let invocation = format!("{program} {}", args.join(" "));
        self.calls
            .lock()
            .unwrap()
            .push(invocation.trim_end().to_string());
        // A stub's exit code applies here too: interactive commands fail as
        // well — a developer who abandons a device-code prompt leaves `gh`
        // exiting non-zero — and a task that ignores that reports a sign-in
        // it never got. Unstubbed commands still succeed, so tests that only
        // care about which commands ran need not script every prompt.
        Ok(self
            .resolve(program, args)
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

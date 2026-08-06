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

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
        });
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
    fn resolve(&self, program: &str, args: &[&str], options: &RunOptions) -> Option<CommandOutput> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.stubbed(&full, options)
            .or_else(|| self.stubbed(&FakeRunner::stub_key(program, args), options))
    }

    fn stubbed(&self, invocation: &str, options: &RunOptions) -> Option<CommandOutput> {
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
            .max_by_key(|stub| (stub.invocation.len(), stub.env.len()))
            .map(|stub| stub.output.clone())
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
        let invocation = format!("{program} {}", args.join(" "));
        self.calls
            .lock()
            .unwrap()
            .push(invocation.trim_end().to_string());
        Ok(self.lookup(program, args, options))
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
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

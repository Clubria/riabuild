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
use std::sync::Arc;
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
    /// appearing in a process argument list, where `ps` would show them. Bytes,
    /// not `String`: nothing piped through here (a tarball, a token) is
    /// guaranteed to be UTF-8.
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
    /// Fragment stubs: match anywhere in the invocation, not just the front —
    /// for `ssh <options…> host <the real command>`, where the part that
    /// distinguishes one remote invocation from another is the tail, not the
    /// program name every one of them shares.
    contains: Vec<(String, CommandOutput)>,
    available: Vec<String>,
    pub calls: std::sync::Mutex<Vec<String>>,
    /// Invocation and the environment it was given, so a test can assert a task
    /// ran against the right configuration directory and not merely that it ran.
    #[allow(clippy::type_complexity)]
    pub recorded: std::sync::Mutex<Vec<(String, Vec<(String, String)>)>>,
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

    /// Stubs on a fragment appearing anywhere in the invocation, for commands
    /// whose distinguishing part is not at the front — `ssh … host uname -sm`.
    pub fn containing(mut self, fragment: &str, code: i32, stdout: &str, stderr: &str) -> Self {
        self.contains.push((
            fragment.to_string(),
            CommandOutput {
                code: Some(code),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            },
        ));
        let program = fragment.split_whitespace().next().unwrap_or_default();
        if !program.is_empty() && !self.available.iter().any(|p| p == program) {
            self.available.push(program.to_string());
        }
        self
    }

    /// The environment the first matching invocation was run with.
    pub fn env_of(&self, prefix: &str) -> Vec<(String, String)> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .find(|(invocation, _)| invocation.starts_with(prefix))
            .map(|(_, env)| env.clone())
            .unwrap_or_default()
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

        // A fragment match beats a prefix match when it is longer: a longer
        // match is a more specific stub, and a prefix stub on `"ssh"` must not
        // silently answer for every remote invocation a fragment stub scripts.
        let by_fragment = self
            .contains
            .iter()
            .filter(|(fragment, _)| invocation.contains(fragment.as_str()))
            .max_by_key(|(fragment, _)| fragment.len());
        if let Some((fragment, output)) = by_fragment
            && best
                .map(|(key, _)| fragment.len() > key.len())
                .unwrap_or(true)
        {
            return Some(output.clone());
        }

        best.map(|(_, output)| output.clone())
    }

    fn lookup(&self, invocation: &str) -> CommandOutput {
        self.stubbed(invocation).unwrap_or(CommandOutput {
            code: Some(127),
            stdout: String::new(),
            stderr: format!("fake runner: no stub for `{invocation}`"),
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
        let invocation = invocation.trim_end().to_string();
        self.calls.lock().unwrap().push(invocation.clone());
        self.recorded
            .lock()
            .unwrap()
            .push((invocation.clone(), options.env.clone()));
        Ok(self.lookup(&invocation))
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        let invocation = format!("{program} {}", args.join(" "));
        let invocation = invocation.trim_end().to_string();
        self.calls.lock().unwrap().push(invocation.clone());
        self.recorded
            .lock()
            .unwrap()
            .push((invocation.clone(), options.env.clone()));
        // A stub's exit code applies here too: interactive commands fail as
        // well — a developer who abandons a device-code prompt leaves `gh`
        // exiting non-zero — and a task that ignores that reports a sign-in
        // it never got. Unstubbed commands still succeed, so tests that only
        // care about which commands ran need not script every prompt.
        Ok(self
            .stubbed(&invocation)
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

/// A `CommandRunner` that adds a fixed environment to every command.
///
/// This is why `github_cli` cannot authenticate the wrong developer on a shared
/// server. `GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` are not something each task
/// remembers to pass — the runner every task already holds carries them, so a
/// task that forgets is not a thing anyone can write.
///
/// The scope's own keys — `RIABUILD_ROOT`, `GH_CONFIG_DIR`, `GIT_CONFIG_GLOBAL` —
/// are applied *after* the caller's `RunOptions.env`, so they are not
/// overridable: `std::process::Command::env()` (see `RealRunner::build`)
/// overwrites on a repeated key, and the scope's entries are the ones applied
/// last. A task cannot escape its namespace even by naming one of these keys
/// itself — accidentally (a copy-pasted env vector from another task) or
/// otherwise. Every other variable a caller sets — `env_local`'s
/// `INFISICAL_TOKEN`, for instance — has nothing here to collide with, so it
/// reaches the child untouched. See the precedence tests below, including one
/// that pins the collision case: it is written to fail if the merge order were
/// ever put back the other way around.
#[allow(dead_code)] // consumed by Task 19
pub struct ScopedRunner {
    inner: Arc<dyn CommandRunner>,
    env: Vec<(String, String)>,
}

#[allow(dead_code)] // consumed by Task 19
impl ScopedRunner {
    pub fn new(inner: Arc<dyn CommandRunner>, env: Vec<(String, String)>) -> Self {
        Self { inner, env }
    }

    fn merge(&self, options: &RunOptions) -> RunOptions {
        let mut merged = options.clone();
        let mut env = options.env.clone();
        env.extend(self.env.iter().cloned());
        merged.env = env;
        merged
    }
}

#[async_trait]
impl CommandRunner for ScopedRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
        self.inner.run(program, args, &self.merge(options)).await
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        self.inner
            .run_interactive(program, args, &self.merge(options))
            .await
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.inner.which(program)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scoped_runner_puts_its_environment_on_every_command() {
        let fake = Arc::new(FakeRunner::new().with("gh auth status", 0, "", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/run/user/1000/riabuild-gh".into())],
        );

        scoped
            .run("gh", &["auth", "status"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("gh auth status"),
            vec![(
                "GH_CONFIG_DIR".to_string(),
                "/run/user/1000/riabuild-gh".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn a_caller_can_still_add_its_own_environment() {
        // `env_local` passes INFISICAL_TOKEN this way. The scope adds to that,
        // never replaces it.
        let fake = Arc::new(FakeRunner::new().with("infisical export", 0, "A=b\n", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())],
        );

        scoped
            .run(
                "infisical",
                &["export"],
                &RunOptions {
                    env: vec![("INFISICAL_TOKEN".into(), "st.secret".into())],
                    ..Default::default()
                },
            )
            .await
            .expect("runs");

        let env = fake.env_of("infisical export");
        assert!(env.contains(&("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())));
        assert!(env.contains(&("INFISICAL_TOKEN".to_string(), "st.secret".to_string())));
    }

    #[tokio::test]
    async fn an_interactive_command_is_scoped_too() {
        // `gh auth login` is interactive, and it is exactly the command that must
        // not write into another developer's configuration directory.
        let fake = Arc::new(FakeRunner::new().with("gh auth login", 0, "", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())],
        );

        scoped
            .run_interactive("gh", &["auth", "login"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("gh auth login"),
            vec![("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())]
        );
    }

    #[tokio::test]
    async fn a_caller_cannot_override_a_namespace_key() {
        // A task cannot escape its namespace even by naming one of the scope's
        // own keys itself — accidentally (a copy-pasted env vector from another
        // task) or otherwise. Both entries still reach the inner runner (this
        // type does not deduplicate), but `std::process::Command::env` (see
        // `RealRunner::build`) overwrites on a repeated key with whichever call
        // came last, and the scope's entry is appended after the caller's in
        // `merge()` — so it is the scope's value the real child process sees.
        // This is written to fail if that merge order were ever put back the
        // other way around: see "Prove it bites" in the Task 8 report.
        let fake = Arc::new(FakeRunner::new().with("gh auth status", 0, "", ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/run/user/1000/riabuild-gh".into())],
        );

        scoped
            .run(
                "gh",
                &["auth", "status"],
                &RunOptions {
                    env: vec![("GH_CONFIG_DIR".into(), "/tmp/some-other-place".into())],
                    ..Default::default()
                },
            )
            .await
            .expect("runs");

        let env = fake.env_of("gh auth status");
        assert_eq!(
            env,
            vec![
                (
                    "GH_CONFIG_DIR".to_string(),
                    "/tmp/some-other-place".to_string()
                ),
                (
                    "GH_CONFIG_DIR".to_string(),
                    "/run/user/1000/riabuild-gh".to_string()
                ),
            ],
            "the scope's entry must be last, since std::process::Command::env() lets the last call for a key win"
        );
    }

    #[tokio::test]
    async fn the_scoped_environment_reaches_a_real_child_process_not_just_the_struct() {
        // Everything above goes through `FakeRunner`, which proves the merged
        // environment is threaded through the call, not merely stored on
        // `ScopedRunner`. This test closes the last gap by running a real
        // process and reading its actual environment back out of its stdout.
        let scoped = ScopedRunner::new(
            Arc::new(RealRunner),
            vec![(
                "RIABUILD_SCOPED_RUNNER_TEST".into(),
                "namespaced-value".into(),
            )],
        );

        let output = scoped
            .run("env", &[], &RunOptions::default())
            .await
            .expect("runs");

        assert!(
            output
                .stdout
                .lines()
                .any(|line| line == "RIABUILD_SCOPED_RUNNER_TEST=namespaced-value"),
            "child environment did not contain the scoped variable:\n{}",
            output.stdout
        );
    }

    #[tokio::test]
    async fn a_child_receives_bytes_that_are_not_valid_utf8() {
        // A gzip header is not valid UTF-8, and Task 17 streams a whole binary
        // through here. Asserting a field returns what was just assigned to
        // it would prove nothing about the code that could get
        // bytes-versus-UTF-8 wrong; this runs a real child instead.
        //
        // The check goes through `wc -c` rather than `cat` + a byte-length
        // comparison on `stdout`: `CommandOutput::stdout` is a lossy-decoded
        // `String` (`String::from_utf8_lossy`), and every invalid byte in
        // these six becomes a 3-byte U+FFFD replacement on the way back out
        // — echoing the input and measuring the echo would be measuring the
        // lossy decoder, not what the child actually received on stdin.
        // `wc -c` reports the byte count as plain ASCII digits, which round-
        // trips through that same lossy decoding unchanged, so it proves the
        // six raw bytes reached the child intact without depending on stdout
        // being representable as UTF-8 at all.
        let bytes = vec![0x1f, 0x8b, 0x08, 0x00, 0xff, 0xfe];
        let output = RealRunner
            .run(
                "wc",
                &["-c"],
                &RunOptions {
                    stdin: Some(bytes.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("wc runs");
        assert_eq!(output.trimmed(), bytes.len().to_string());
    }

    #[tokio::test]
    async fn a_fragment_stub_can_answer_for_the_end_of_a_command() {
        let fake = FakeRunner::new()
            .with("ssh", 1, "", "unmatched")
            .containing("uname -sm", 0, "Linux x86_64\n", "");

        let output = fake
            .run(
                "ssh",
                &["-p", "22", "ada@box", "uname -sm"],
                &RunOptions::default(),
            )
            .await
            .expect("runs");
        assert_eq!(output.trimmed(), "Linux x86_64");
    }
}

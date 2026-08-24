//! The `CommandRunner` the fake presents to the code under test.

use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

use crate::CommandRunner;
use crate::child::{ChildHandle, ChildReader, ChildWriter, PipedChildHandle};
use crate::fake::child::FakeChild;
use crate::fake::{Ending, FakePipes, FakeRunner};
use crate::options::RunOptions;
use crate::output::{BytesOutput, CommandOutput};

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

    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>> {
        let invocation = self.record(program, args, options);
        let key = FakeRunner::stub_key(program, args);
        // Unscripted spawns end the way unstubbed commands do, rather than
        // hanging: a test that forgot to script the second attempt should read
        // "no stub" in a failed assertion, not time out.
        let ending = self
            .next_child(&invocation)
            .or_else(|| self.next_child(&key))
            .unwrap_or_else(|| {
                Ending::Alone(CommandOutput {
                    code: Some(127),
                    stdout: String::new(),
                    stderr: format!("fake runner: no stub for `{key}`"),
                })
            });

        let child = Arc::new(FakeChild {
            invocation,
            exit: match ending {
                Ending::Alone(output) => Some(output),
                Ending::OnlyWhenKilled => None,
            },
            stdin: std::sync::Mutex::new(None),
            stdout: std::sync::Mutex::new(None),
            far: std::sync::Mutex::new(None),
            killed: std::sync::Mutex::new(false),
            stopped: tokio::sync::Notify::new(),
        });
        // Kept here as well as handed out, so `killed()` can answer after the
        // code under test has dropped its handle.
        self.spawned.lock().unwrap().push(child.clone());
        Ok(Box::new(child))
    }

    async fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<()> {
        // Recorded and nothing else, which is the whole of what a test can
        // check about a detached child: the real one is deliberately unwaitable
        // and unkillable from here, so there is no handle to hand back and no
        // outcome to script. What a caller must get right is *what it asked
        // for* — the argv, the directory and the environment — and `calls()`
        // is where that is asserted.
        self.record(program, args, options);
        Ok(())
    }

    async fn spawn_piped(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn PipedChildHandle>> {
        let invocation = self.record(program, args, options);
        let key = FakeRunner::stub_key(program, args);
        // A piped child defaults to staying up rather than to exiting 127. The
        // transport it stands in for is a session that lives as long as the
        // shell, so "no stub" here means "an ssh that stayed connected and said
        // nothing", which is what a test driving the pipe by hand wants.
        let ending = self
            .next_child(&invocation)
            .or_else(|| self.next_child(&key))
            .unwrap_or(Ending::OnlyWhenKilled);

        // 64 KB each way, matching a real pipe's buffer closely enough that a
        // test can hit backpressure the same way production would.
        let (their_stdin, our_stdin) = tokio::io::duplex(64 * 1024);
        let (mut their_stdout, our_stdout) = tokio::io::duplex(64 * 1024);

        let exit = match ending {
            Ending::Alone(output) => Some(output),
            Ending::OnlyWhenKilled => None,
        };

        // A child scripted to *exit* has a closed pipe, so its stdout is
        // delivered here and the far end is dropped rather than handed back.
        // Keyed on the ending rather than on whether any stdout was scripted:
        // a stub that exits saying nothing is the commonest way to script a
        // failure, and holding its pipe open would leave a reader looping until
        // EOF waiting for ever. That is a hang rather than a failure, and hangs
        // are precisely what this workspace's macOS CI job exists to catch —
        // after one cost a release twenty-five minutes of a runner building
        // nothing.
        //
        // `OnlyWhenKilled` keeps its pipes, because that stub is the live
        // transport a test drives by hand through `pipes`.
        let far = match &exit {
            Some(output) => {
                use tokio::io::AsyncWriteExt;
                // Small enough for the buffer above, so this cannot block.
                let _ = their_stdout.write_all(output.stdout.as_bytes()).await;
                let _ = their_stdout.flush().await;
                drop(their_stdout);
                drop(their_stdin);
                None
            }
            None => Some(FakePipes {
                to_riabuild: their_stdout,
                from_riabuild: their_stdin,
            }),
        };

        let child = Arc::new(FakeChild {
            invocation,
            exit,
            stdin: std::sync::Mutex::new(Some(Box::new(our_stdin) as ChildWriter)),
            stdout: std::sync::Mutex::new(Some(Box::new(our_stdout) as ChildReader)),
            far: std::sync::Mutex::new(far),
            killed: std::sync::Mutex::new(false),
            stopped: tokio::sync::Notify::new(),
        });

        self.spawned.lock().unwrap().push(child.clone());
        Ok(Box::new(child))
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
mod tests {
    use crate::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    fn in_dir(dir: &str) -> RunOptions {
        RunOptions {
            env: vec![("CLAUDE_CONFIG_DIR".to_string(), dir.to_string())],
            ..Default::default()
        }
    }

    fn in_env(key: &str, value: &str) -> RunOptions {
        RunOptions {
            env: vec![(key.to_string(), value.to_string())],
            ..Default::default()
        }
    }

    #[test]
    fn a_command_riabuild_runs_lands_at_the_root_when_it_names_no_directory() {
        assert_eq!(directory_for_riabuild(None), Path::new("/"));
    }

    #[test]
    fn a_named_directory_is_still_where_the_command_runs() {
        // The rule removes the *inherited* directory, not the chosen one:
        // `infisical export` still has to run in the checkout.
        let named = Path::new("/home/ada/clubria");
        assert_eq!(directory_for_riabuild(Some(named)), named);
    }

    /// The wiring, against a real child rather than the rule in isolation.
    ///
    /// `cargo test` runs with the crate directory as its working directory, so a
    /// child that inherited would print that path. Printing `/` is only possible
    /// if `for_riabuild` actually reached the `Command`. Without it this whole
    /// module could hold a correct rule that nothing applied.
    #[tokio::test]
    async fn a_real_child_does_not_inherit_the_directory_riabuild_was_started_in() {
        let output = RealRunner
            .run("pwd", &[], &RunOptions::default())
            .await
            .expect("pwd");
        assert!(output.ok(), "{output:?}");
        assert_eq!(output.trimmed(), "/", "the child inherited");
    }

    #[tokio::test]
    async fn a_real_child_still_runs_where_it_was_told_to() {
        let dir = tempfile::TempDir::new().unwrap();
        // Resolved on both sides: on macOS `/var` is a symlink to `/private/var`
        // and `pwd` answers with the resolved path, so comparing against the
        // tempdir's own path would fail there and nowhere else.
        let wanted = std::fs::canonicalize(dir.path()).unwrap();
        let output = RealRunner
            .run(
                "pwd",
                &[],
                &RunOptions {
                    cwd: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
            )
            .await
            .expect("pwd");
        assert_eq!(Path::new(output.trimmed()), wanted);
    }

    /// The exception, asserted rather than assumed.
    ///
    /// A developer handed the environment shell somewhere other than where they
    /// were standing would be riabuild moving them without being asked, so the
    /// handoff keeps inheriting. `run_interactive` returns only an exit code —
    /// it gives its stdout away — so the check has to be the child's own.
    #[tokio::test]
    async fn a_handoff_still_inherits() {
        let code = RealRunner
            .run_interactive("sh", &["-c", r#"[ "$PWD" != / ]"#], &RunOptions::default())
            .await
            .expect("sh");
        assert_eq!(code, 0, "the handoff was moved to the root");
    }

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
    async fn the_byte_and_forking_commands_are_scoped_too() {
        // The two methods the laptop channel added. A clipboard read and a
        // clipboard write are ordinary children of a namespaced session, so a
        // route through the trait that skipped `merge()` would be an unscoped
        // command nobody would notice until two developers shared a server.
        let fake = Arc::new(
            FakeRunner::new()
                .with_bytes("xclip -o", 0, b"copied", "")
                .with("xclip -i", 0, "", ""),
        );
        let scoped = ScopedRunner::new(fake.clone(), vec![("DISPLAY".into(), ":17".into())]);

        scoped
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .expect("runs");
        scoped
            .run_forking("xclip", &["-i"], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.env_of("xclip -o"),
            vec![("DISPLAY".to_string(), ":17".to_string())]
        );
        assert_eq!(
            fake.env_of("xclip -i"),
            vec![("DISPLAY".to_string(), ":17".to_string())]
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
    async fn the_fake_records_piped_bytes_and_reports_none_when_nothing_was_piped() {
        // The accessor every "a secret travels on stdin" test now leans on. It
        // has to distinguish the two cases, not merely return something: a
        // `stdin_of` that answered `Some` unconditionally would make all four
        // of those tests green again for exactly the reason they were written.
        let fake = FakeRunner::new()
            .with("security", 0, "", "")
            .with("id", 0, "", "");
        fake.run(
            "security",
            &["add-generic-password"],
            &RunOptions {
                stdin: Some(b"piped-secret".to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect("runs");
        fake.run("id", &[], &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!(
            fake.stdin_text_of("security").as_deref(),
            Some("piped-secret")
        );
        assert_eq!(fake.stdin_of("id"), None);
        assert_eq!(fake.stdin_of("never-run"), None);
    }

    #[tokio::test]
    async fn the_stdin_of_a_call_is_also_reachable_by_fragment() {
        // `input_for` is the clipboard channel's reader: `wl-copy` is invoked
        // with the payload's type in front of nothing the test can predict, so
        // it looks the call up by a fragment rather than by a prefix. Both
        // accessors read the one recording, so neither can go stale while the
        // other still works.
        let fake = FakeRunner::new().with("wl-copy", 0, "", "");
        fake.run_forking(
            "wl-copy",
            &["--type", "image/png"],
            &RunOptions {
                stdin: Some(vec![0x89, b'P', b'N', b'G']),
                ..Default::default()
            },
        )
        .await
        .expect("runs");

        assert_eq!(
            fake.input_for("image/png"),
            Some(vec![0x89, b'P', b'N', b'G'])
        );
        assert_eq!(fake.inputs().len(), 1);
        assert_eq!(fake.input_for("xclip"), None);
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

    #[tokio::test]
    async fn a_queued_response_is_consumed_before_the_standing_stub() {
        // The "first call fails, second succeeds" shape: a probe run before and
        // after the action a test is asserting changed the server's state. Once
        // the queue empties, the standing stub answers again.
        let fake = FakeRunner::new()
            .with("ssh box cat /token", 0, "standing", "")
            .then("ssh box cat /token", 1, "before", "")
            .then("ssh box cat /token", 0, "after", "");
        let args = ["box", "cat", "/token"];

        let first = fake
            .run("ssh", &args, &RunOptions::default())
            .await
            .expect("runs");
        let second = fake
            .run("ssh", &args, &RunOptions::default())
            .await
            .expect("runs");
        let third = fake
            .run("ssh", &args, &RunOptions::default())
            .await
            .expect("runs");

        assert_eq!((first.trimmed(), first.code), ("before", Some(1)));
        assert_eq!((second.trimmed(), second.code), ("after", Some(0)));
        assert_eq!(
            third.trimmed(),
            "standing",
            "an exhausted queue falls through to the standing stub"
        );
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

    /// The bug this pins: `write_all(stdin)` finished before anything read the
    /// child's output.
    ///
    /// A child that prints while it reads fills its 64 KB stdout, blocks there,
    /// and stops draining the stdin riabuild is still writing — after which
    /// neither side moves again. `remote::install` pushes a whole riabuild
    /// binary through this path, which is a great deal more than one buffer.
    ///
    /// The bound is part of the assertion: a regression here is a hang, and a
    /// hang has to present as a red test rather than as a slow one.
    #[tokio::test]
    async fn a_child_that_prints_while_it_reads_is_fed_and_read_at_the_same_time() {
        let input = vec![b'x'; 1024 * 1024];
        let echoed = tokio::time::timeout(
            Duration::from_secs(20),
            RealRunner.run_bytes(
                "cat",
                &[],
                &RunOptions {
                    stdin: Some(input.clone()),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("a megabyte through `cat` must not deadlock")
        .expect("cat runs");

        assert_eq!(echoed.code, Some(0));
        assert_eq!(echoed.stdout.len(), input.len());
    }

    /// The other half of it: the child's own report survives a write that could
    /// not be delivered.
    ///
    /// A child that gives up closes its stdin on the way out, so a write
    /// part-way through a megabyte fails with `Broken pipe`. Returning *that*
    /// discarded the exit code and the stderr the developer needed in favour of
    /// the symptom — and every caller builds its message out of exactly those
    /// two.
    #[tokio::test]
    async fn a_child_that_refuses_its_input_still_reports_its_own_exit() {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            RealRunner.run(
                "sh",
                &["-c", "echo refused >&2; exit 3"],
                &RunOptions {
                    stdin: Some(vec![b'x'; 1024 * 1024]),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("a refused write must not hang")
        .expect("the child's own status, not `Broken pipe`");

        assert_eq!(output.code, Some(3), "{output:?}");
        assert_eq!(output.stderr.trim(), "refused");
    }

    #[test]
    fn a_call_that_names_no_bound_still_has_one() {
        // The whole of the fix: the bound is at the layer every subprocess
        // already goes through, and `RunOptions::default()` is how almost every
        // call site in the workspace is spelled. A derived `None` here would
        // leave all of them waiting for ever on a current-thread runtime.
        assert_eq!(RunOptions::default().timeout, Some(DEFAULT_TIMEOUT));
    }

    #[tokio::test]
    async fn a_child_that_will_never_finish_is_given_up_on_at_the_bound() {
        let started = std::time::Instant::now();
        let error = RealRunner
            .run(
                "sleep",
                &["30"],
                &RunOptions {
                    timeout: Some(Duration::from_millis(200)),
                    ..Default::default()
                },
            )
            .await
            .expect_err("a child that outlasts its bound is a failure, not a wait");

        let message = format!("{error:#}");
        assert!(message.contains("did not finish within"), "{message}");
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "the wait was not bounded"
        );
    }

    #[tokio::test]
    async fn a_call_that_wants_no_bound_can_say_so() {
        // The escape hatch, so that "the runner bounds every call" never
        // becomes a reason to reach around the runner.
        let output = RealRunner
            .run(
                "sh",
                &["-c", "exit 0"],
                &RunOptions {
                    timeout: None,
                    ..Default::default()
                },
            )
            .await
            .expect("sh");
        assert!(output.ok(), "{output:?}");
    }

    /// The method the hand-written decorators forgot, asserted on the base they
    /// are now built from.
    ///
    /// A decorator that omits a method does not fail to compile: it falls
    /// through to the trait's default, which for `spawn_piped` refuses.
    /// `Delegating` is the only place that knows what the whole of
    /// `CommandRunner` is, so a method added to it later reaches every wrapper
    /// without any of them being edited.
    #[tokio::test]
    async fn a_scope_reaches_the_piped_child_too() {
        let fake = Arc::new(FakeRunner::new());
        let scoped = ScopedRunner::new(fake.clone(), vec![("GH_CONFIG_DIR".into(), "/ns".into())]);

        let child = scoped
            .spawn_piped("ssh", &["build-01"], &RunOptions::default())
            .await
            .expect("a delegated `spawn_piped` reaches the wrapped runner");
        child.kill().await.expect("kills");

        assert_eq!(
            fake.env_of("ssh build-01"),
            vec![("GH_CONFIG_DIR".to_string(), "/ns".to_string())]
        );
    }
}

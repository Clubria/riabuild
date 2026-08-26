//! The `CommandRunner` that starts real processes.
//!
//! What is here is the two questions a spawn has to answer before it is safe:
//! which directory the child lands in, and how long riabuild is prepared to
//! wait for it.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

use crate::CommandRunner;
use crate::child::{ChildHandle, PipedChildHandle};
#[cfg(unix)]
use crate::options::should_subdue;
use crate::options::{RunOptions, directory_for_riabuild};
use crate::output::{BytesOutput, CommandOutput};

pub struct RealRunner;

impl RealRunner {
    /// A command riabuild runs to learn or change something itself.
    ///
    /// It never lands in a directory riabuild did not choose: with no `cwd` it
    /// runs at [`FILESYSTEM_ROOT`](crate::options::FILESYSTEM_ROOT). Every such call already either names its
    /// directory or carries an absolute path — `git -C`, `gh repo clone <slug>
    /// <dir>`, `dpkg -S /usr/bin/riabuild` — so there is nothing here for whose
    /// benefit riabuild's own working directory could be the right answer, and
    /// plenty that reads it and answers wrongly because of it.
    fn for_riabuild(program: &str, args: &[&str], options: &RunOptions) -> Command {
        let mut command = Self::build(program, args, options);
        command.current_dir(directory_for_riabuild(options.cwd.as_deref()));
        command
    }

    /// A command handed to the developer, which inherits.
    ///
    /// The whole of the exception, and named at the call site so that it reads
    /// as one. `run_interactive` gives away the terminal — to the environment
    /// shell, to `ssh`, to `gh auth login` — and a developer handed a shell
    /// somewhere other than where they were standing would be riabuild moving
    /// them without being asked.
    fn for_the_developer(program: &str, args: &[&str], options: &RunOptions) -> Command {
        Self::build(program, args, options)
    }

    /// Starts a child riabuild is going to wait out.
    ///
    /// `output` is what stdout and stderr become — piped for the two methods
    /// that read them, null for `run_forking`, whose child hands both to a
    /// fork that outlives it. stdin is piped only where there is something to
    /// feed it.
    fn start(
        program: &str,
        args: &[&str],
        options: &RunOptions,
        output: fn() -> Stdio,
    ) -> Result<Child> {
        let mut command = Self::for_riabuild(program, args, options);
        command.stdout(output()).stderr(output());
        command.stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        // What makes `RunOptions::timeout` a bound on the *process* rather
        // than merely on riabuild's patience: the expired wait drops the child
        // with it, and a child dropped this way is signalled and reaped. A
        // `gh` left blocked on a prompt would otherwise run for the rest of
        // the session, holding the config directory riabuild is about to
        // rewrite.
        command.kill_on_drop(true);
        command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))
    }

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

/// Waits a captured child out, under the bound the call named.
///
/// The kill is `kill_on_drop`, set by `RealRunner::start`: dropping the expired
/// future drops the child with it. Nothing here has to signal anything, and
/// nothing is left to be reaped by a later run.
async fn capture(
    child: Child,
    program: &str,
    options: &RunOptions,
) -> Result<std::process::Output> {
    let waiting = feed_and_wait(child, program, options.stdin.as_deref());
    let Some(bound) = options.timeout else {
        return waiting.await;
    };
    match tokio::time::timeout(bound, waiting).await {
        Ok(finished) => finished,
        Err(_elapsed) => anyhow::bail!(
            "`{program}` did not finish within {} seconds",
            bound.as_secs()
        ),
    }
}

/// Feeds the child its stdin *while* reading what it says back.
///
/// Writing the whole of stdin first is a deadlock on any transfer larger than a
/// pipe buffer: a child that prints while it reads fills its 64 KB stdout,
/// blocks there, and stops draining the stdin riabuild is still writing —
/// after which neither side moves again. `remote::install` pushes a whole
/// riabuild binary through here, so this is not a theoretical size.
///
/// The two halves are joined rather than raced: a child's exit is not a reason
/// to abandon the write, and a finished write is not a reason to stop waiting.
async fn feed_and_wait(
    mut child: Child,
    program: &str,
    input: Option<&[u8]>,
) -> Result<std::process::Output> {
    let pipe = child.stdin.take();
    let feeding = async {
        let (Some(mut pipe), Some(bytes)) = (pipe, input) else {
            return Ok(());
        };
        use tokio::io::AsyncWriteExt;
        pipe.write_all(bytes).await?;
        // Closing the pipe is what tells the child there is no more input. A
        // child reading to EOF — `infisical export` does, and so does the `tar`
        // on the far end of an install — would otherwise wait on a handle this
        // future is still holding. `xclip` and `wl-copy` go further and do not
        // own the selection until they have read it, so this is what completes
        // a clipboard write rather than merely tidying up after one.
        drop(pipe);
        Ok::<(), std::io::Error>(())
    };

    let (written, finished) = tokio::join!(feeding, child.wait_with_output());
    let output = finished.with_context(|| format!("`{program}` did not finish"))?;

    // A failed write is reported only where the child itself claims nothing
    // went wrong. A child that gives up — a bad argument, a refused token —
    // closes its stdin on the way out, so the write riabuild was part-way
    // through fails with `Broken pipe`; returning *that* would discard the exit
    // code and the stderr the developer needs in favour of the symptom.
    match written {
        Err(error) if output.status.success() => {
            Err(error).with_context(|| format!("could not write to `{program}`'s stdin"))
        }
        _ => Ok(output),
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
        let child = RealRunner::start(program, args, options, Stdio::piped)?;
        let output = capture(child, program, options).await?;

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
        let child = RealRunner::start(program, args, options, Stdio::piped)?;
        let output = capture(child, program, options).await?;

        Ok(BytesOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32> {
        // Null rather than piped: a pipe handed to the fork is exactly what
        // would keep this call waiting for a selection nobody is going to
        // replace. `wait_with_output` reads neither half when there is no half
        // to read, so this waits for the process riabuild actually started.
        let child = RealRunner::start(program, args, options, Stdio::null)?;
        let output = capture(child, program, options).await?;
        Ok(output.status.code().unwrap_or(1))
    }

    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>> {
        let command = RealRunner::for_riabuild(program, args, options);
        Ok(Box::new(crate::child::RealChild::spawn(command, program)?))
    }

    async fn spawn_piped(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn PipedChildHandle>> {
        let command = RealRunner::for_riabuild(program, args, options);
        Ok(Box::new(crate::child::RealChild::spawn_piped(
            command, program,
        )?))
    }

    async fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<()> {
        let mut command = RealRunner::for_riabuild(program, args, options);
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        // Not set, unlike every other spawn in this file. The child is meant to
        // outlive the handle, and `kill_on_drop` would end it on the next line.
        command.kill_on_drop(false);

        // SAFETY: `setsid(2)` is async-signal-safe, and this closure runs in the
        // forked child between `fork` and `exec` — where only async-signal-safe
        // calls are permitted. It allocates nothing and touches no lock. Its
        // only failure is `EPERM`, when the caller already leads a process
        // group, which means the child is already in a session of its own and
        // there is nothing to fix; ignoring it is correct rather than lax.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `{program}`"))?;
        // Dropping a `tokio::process::Child` without waiting leaves a zombie
        // until this process exits. `try_wait` in a detached spawn would race
        // the child's own startup, so the reaping is handed to a task that
        // simply waits: it costs nothing, and it ends when the child does
        // whether or not anybody is still watching.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        let mut command = RealRunner::for_the_developer(program, args, options);

        // The handoff `CLAUDE.md` describes is still the default, and still the
        // rule for every site that leaves `subdued` unset. Where riabuild does
        // perform the IO it does so through `AsyncFd` on the current-thread
        // runtime — see `pty.rs`.
        #[cfg(unix)]
        if let Some(theme) = should_subdue(crate::pty::available(), options.subdued) {
            return crate::pty::run(command, theme, program).await;
        }

        let status = command
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
mod spawn_tests {
    use crate::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// The whole reason `spawn` exists: the call returns while the child is
    /// still running, and stderr is still there to be read when it finally
    /// exits — which is all `supervisor::diagnose` has to work from.
    #[tokio::test]
    async fn a_spawned_child_is_handed_back_before_it_has_finished() {
        let handle = RealRunner
            .spawn(
                "sh",
                &["-c", "printf 'refused the forward' >&2; exit 3"],
                &RunOptions::default(),
            )
            .await
            .expect("spawns");

        let output = handle.wait().await.expect("waits");
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stderr, "refused the forward");
    }

    /// The teardown path, and the reason `wait` takes `&self`: the handle is
    /// still usable while a wait on it is outstanding, which is what lets the
    /// supervisor kill a tunnel that has gone quiet instead of waiting out an
    /// exit that is never coming.
    #[tokio::test]
    async fn a_child_can_be_killed_while_a_wait_on_it_is_outstanding() {
        let handle = RealRunner
            .spawn("sleep", &["30"], &RunOptions::default())
            .await
            .expect("spawns");

        tokio::select! {
            _ = handle.wait() => panic!("`sleep 30` should still be running"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        handle.kill().await.expect("kills");
        let output = handle.wait().await.expect("waits");
        assert_eq!(output.code, None, "a killed process exits by signal");
    }

    /// A ping timeout fires without knowing whether ssh has meanwhile exited
    /// on its own, so a second kill has to be a no-op rather than an error the
    /// supervisor has to special-case.
    #[tokio::test]
    async fn killing_a_child_that_has_already_gone_is_not_an_error() {
        let handle = RealRunner
            .spawn("true", &[], &RunOptions::default())
            .await
            .expect("spawns");

        handle.wait().await.expect("waits");
        handle.kill().await.expect("first kill");
        handle.kill().await.expect("second kill");
    }

    /// Without `kill_on_drop`, a handle dropped anywhere above the supervisor
    /// leaves an ssh alive holding the remote socket, the next attempt cannot
    /// bind it, and the channel comes up permanently dead. Asserted through a
    /// file the child only creates if it outlived the handle.
    #[tokio::test]
    async fn a_dropped_handle_does_not_leave_the_child_running() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let marker = dir.path().join("still-running");
        let script = format!("sleep 0.3; : > {}", marker.display());

        let handle = RealRunner
            .spawn("sh", &["-c", &script], &RunOptions::default())
            .await
            .expect("spawns");
        drop(handle);

        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(
            tokio::fs::metadata(&marker).await.is_err(),
            "the child outlived its handle"
        );
    }

    /// A spawned child is as much a namespaced process as any other, and the
    /// longest-lived one riabuild starts — an unscoped tunnel would point at
    /// the wrong developer's configuration for the whole session.
    #[tokio::test]
    async fn a_spawned_child_is_scoped_too() {
        let fake = Arc::new(FakeRunner::new().spawning("ssh", 0, ""));
        let scoped = ScopedRunner::new(
            fake.clone(),
            vec![("GH_CONFIG_DIR".into(), "/tmp/gh".into())],
        );

        scoped
            .spawn("ssh", &["-N"], &RunOptions::default())
            .await
            .expect("spawns");

        assert_eq!(
            fake.env_of("ssh -N"),
            vec![("GH_CONFIG_DIR".to_string(), "/tmp/gh".to_string())]
        );
    }

    /// The backoff schedule is a property of the *sequence* of failures, so
    /// the fake has to be able to tell the first attempt from the third.
    #[tokio::test]
    async fn successive_spawns_get_successive_scripted_endings() {
        let fake = FakeRunner::new()
            .spawning("ssh", 255, "Connection refused")
            .spawning("ssh", 255, "Bad remote forwarding specification")
            .spawning("ssh", 0, "");
        let args = ["-N", "-R", "/run/sock:/tmp/sock", "ada@box"];

        let mut endings = Vec::new();
        for _ in 0..3 {
            let handle = fake
                .spawn("ssh", &args, &RunOptions::default())
                .await
                .expect("spawns");
            let output = handle.wait().await.expect("waits");
            endings.push((output.code, output.stderr));
        }

        assert_eq!(endings[0], (Some(255), "Connection refused".to_string()));
        assert_eq!(
            endings[1],
            (Some(255), "Bad remote forwarding specification".to_string())
        );
        assert_eq!(endings[2], (Some(0), String::new()));
    }

    /// The tunnel that came up fine and then went quiet. A ping-timeout test
    /// needs a child that is still there to be torn down; one that exits on
    /// its own puts the supervisor on the rebuild-after-clean-exit path
    /// instead, which is a different behaviour entirely.
    #[tokio::test]
    async fn a_child_scripted_to_stay_up_resolves_only_once_it_is_killed() {
        let fake = FakeRunner::new().spawning_until_killed("ssh");
        let handle = fake
            .spawn("ssh", &["-N"], &RunOptions::default())
            .await
            .expect("spawns");

        tokio::select! {
            _ = handle.wait() => panic!("a live child must not resolve on its own"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }

        handle.kill().await.expect("kills");
        let output = handle.wait().await.expect("waits");
        assert_eq!(output.code, None);
    }

    /// `calls()` can never show a teardown, because a kill is not an
    /// invocation — so without `killed()` a supervisor that leaked every
    /// wedged ssh it replaced would pass a rebuild test unchanged.
    #[tokio::test]
    async fn the_fake_records_which_children_were_started_and_which_were_killed() {
        let fake = FakeRunner::new()
            .spawning_until_killed("ssh")
            .spawning_until_killed("ssh");

        let first = fake
            .spawn("ssh", &["-N", "one"], &RunOptions::default())
            .await
            .expect("spawns");
        let second = fake
            .spawn("ssh", &["-N", "two"], &RunOptions::default())
            .await
            .expect("spawns");
        first.kill().await.expect("kills");
        drop(first);
        drop(second);

        assert_eq!(fake.spawns(), vec!["ssh -N one", "ssh -N two"]);
        assert_eq!(fake.killed(), vec!["ssh -N one"]);
    }

    /// A test that forgot to script an attempt should read "no stub" in a
    /// failed assertion rather than time out waiting on a child nobody ever
    /// told how to end.
    #[tokio::test]
    async fn an_unscripted_spawn_ends_the_way_an_unstubbed_command_does() {
        let fake = FakeRunner::new();
        let handle = fake
            .spawn("ssh", &["-N"], &RunOptions::default())
            .await
            .expect("spawns");
        let output = handle.wait().await.expect("waits");

        assert_eq!(output.code, Some(127));
        assert!(output.stderr.contains("no stub"), "{}", output.stderr);
    }
}

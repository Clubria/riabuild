//! One turn, from the inside.
//!
//! This is what `riabuild internal agent-turn` runs, detached, after a window
//! asked a session something. It is the only riabuild process that exists while
//! an agent is working, and it exists for exactly three reasons — none of which
//! a third-party binary could be asked to do for itself:
//!
//! 1. **It holds the lock.** Liveness has to be answerable by any window that
//!    opens later, and the only honest answer is a lock held by a live process.
//! 2. **It appends the spool.** The harness writes its NDJSON to a pipe; this
//!    copies it to `events.ndjson`, which is what a window reads both live and
//!    tomorrow.
//! 3. **It records the thread id.** The id arrives in the stream and is the one
//!    thing without which the next turn starts a new conversation instead of
//!    continuing this one.
//!
//! It is not a supervisor. It runs one turn, writes down what happened, and
//! exits — so nothing has to be reaped, restarted or cleaned up after a reboot.

use std::path::Path;

use anyhow::{Context, Result};
use riabuild_harness::{Event, Reader};
use riabuild_paths::filelock::FileLock;
use riabuild_runner::{CommandRunner, RunOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::store::{Record, Store};

/// Runs the turn whose prompt is waiting in `prompt_file`.
///
/// `org_settings` is the team's Claude Code settings file, or `None` where this
/// machine has none cached. It is resolved by the caller for the reason the
/// binary is — both move with a riabuild upgrade, and neither may be recorded
/// on a session written last week — and it is checked for existence there
/// rather than here, so that a turn on an unprovisioned machine names no file
/// instead of naming one that is not on disk.
///
/// Returns the harness's exit code.
pub async fn run(
    runner: &dyn CommandRunner,
    store: &Store,
    id: &str,
    program: &str,
    org_settings: Option<&Path>,
    prompt_file: &Path,
) -> Result<i32> {
    // Waits rather than refusing. Two prompts sent while a turn is in flight are
    // two of these, and queueing is what a developer means by sending them: a
    // second turn that gave up would silently drop a message they watched being
    // accepted.
    let _lock = FileLock::acquire(&store.lock_path(id), || {}).await?;

    // Re-read *inside* the lock. While this was waiting, the turn ahead of it
    // may have learned the thread id, and starting without it opens a second
    // conversation rather than continuing the one on screen.
    let record = store
        .read(id)
        .await
        .with_context(|| format!("session {id} has no record"))?;
    // Checked here as well as in `one_turn`, because this is the arm that can
    // say *which* session — a record naming a harness this riabuild has never
    // heard of was written by a newer one, and the developer needs to know that
    // rather than seeing a turn that silently did nothing.
    if record.harness().is_none() {
        anyhow::bail!("session {id} names a harness this riabuild does not know");
    }

    let prompt = tokio::fs::read_to_string(prompt_file)
        .await
        .with_context(|| format!("no prompt at {}", prompt_file.display()))?;

    let outcome = one_turn(runner, store, &record, program, org_settings, &prompt).await;

    // Whatever happened, the prompt is not run twice. A wrapper that failed and
    // left the file behind would replay that turn on the next window's tick.
    let _ = tokio::fs::remove_file(prompt_file).await;

    match outcome {
        Ok(code) => Ok(code),
        Err(error) => {
            // The one place a failure can be said. This wrapper's stderr goes
            // nowhere — it was started detached — and the spool holds only the
            // harness's own wire format, so without this the developer sees a
            // session that simply never did anything.
            note_trouble(store, id, &format!("{error:#}")).await;
            Err(error)
        }
    }
}

async fn one_turn(
    runner: &dyn CommandRunner,
    store: &Store,
    record: &Record,
    program: &str,
    org_settings: Option<&Path>,
    prompt: &str,
) -> Result<i32> {
    let Some(kind) = record.harness() else {
        anyhow::bail!("unknown harness");
    };
    let settings = org_settings.map(|path| path.to_string_lossy().into_owned());
    let args = kind.argv(record.thread.as_deref(), prompt, settings.as_deref());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();

    let mut env = Vec::new();
    if let Some(home) = &record.home {
        // The whole of why resume works across windows. A turn run without it
        // reads a different profile's store, finds no session, and quietly
        // starts a new conversation under the same pane.
        env.push((kind.home_env().to_string(), home.display().to_string()));
    }

    let child = runner
        .spawn_piped(
            program,
            &borrowed,
            &RunOptions {
                cwd: Some(record.cwd.clone()),
                env,
                ..RunOptions::default()
            },
        )
        .await
        .with_context(|| format!("could not start {}", kind.label()))?;

    let stdout = child
        .take_stdout()
        .context("the harness was started without a readable stdout")?;

    let mut spool = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store.spool_path(&record.id))
        .await
        .context("could not open the session's spool")?;

    let mut reader = Reader::new(kind);
    let mut thread = record.thread.clone();
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // Written before it is decoded, so a line this riabuild cannot read is
        // still on disk for one that can. The spool is the harness's bytes, not
        // riabuild's opinion of them.
        spool.write_all(line.as_bytes()).await?;
        spool.write_all(b"\n").await?;
        spool.flush().await?;

        for event in reader.read(&line) {
            if let Event::Ready {
                thread: Some(named),
                ..
            } = event
            {
                thread = Some(named);
            }
        }
    }

    let finished = child.wait().await?;

    // Written after the stream has ended, so the record is only updated with an
    // id the harness actually confirmed.
    let mut updated = store
        .read(&record.id)
        .await
        .unwrap_or_else(|_| record.clone());
    if thread.is_some() {
        updated.thread = thread;
    }
    updated.updated = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(updated.updated);
    store.write(&updated).await?;

    // A harness that exited non-zero without saying anything in its own stream
    // would otherwise be invisible: the pane would show a turn that ended with
    // no reply and no reason.
    let code = finished.code.unwrap_or(-1);
    if code != 0 {
        let detail = finished.stderr.trim();
        let detail = if detail.is_empty() {
            format!("{} exited {code}", kind.label())
        } else {
            format!("{} exited {code}: {detail}", kind.label())
        };
        note_trouble(store, &record.id, &detail).await;
    }
    Ok(code)
}

/// Appends one line riabuild wants a window to show.
async fn note_trouble(store: &Store, id: &str, text: &str) {
    let flat = text.replace('\n', " ");
    let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store.trouble_path(id))
        .await
    else {
        return;
    };
    let _ = file.write_all(format!("{flat}\n").as_bytes()).await;
    let _ = file.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Account;
    use riabuild_harness::Kind;
    use riabuild_runner::FakeRunner;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(dir.path().join("agents"));
        (dir, store)
    }

    async fn queued(store: &Store, id: &str, prompt: &str) -> std::path::PathBuf {
        let pending = store.pending_dir(id);
        tokio::fs::create_dir_all(&pending).await.unwrap();
        let file = pending.join("one.txt");
        tokio::fs::write(&file, prompt).await.unwrap();
        file
    }

    #[tokio::test]
    async fn a_turn_appends_the_harnesss_own_bytes_and_learns_the_thread_id() {
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        let file = queued(&store, &record.id, "hello").await;

        let stream = "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-1\",\"model\":\"m\"}\n";
        let runner = FakeRunner::new().piping("/opt/claude", stream, 0);

        run(&runner, &store, &record.id, "/opt/claude", None, &file)
            .await
            .unwrap();

        // The spool is the bytes, verbatim, so a later window decodes exactly
        // what this turn saw.
        let spool = store.spool(&record.id).await.unwrap();
        assert!(spool.contains("\"session_id\":\"sess-1\""), "{spool}");

        // and the id is recorded, which is the whole of how the next turn
        // continues this conversation rather than starting another
        let after = store.read(&record.id).await.unwrap();
        assert_eq!(after.thread.as_deref(), Some("sess-1"));

        // and the prompt is consumed, so a later tick cannot replay the turn
        assert!(!tokio::fs::try_exists(&file).await.unwrap());
    }

    #[tokio::test]
    async fn a_resumed_turn_passes_the_thread_and_the_profile_home() {
        let (_dir, store) = store();
        let mut record = store
            .create(
                &Account::new(Kind::Claude, 2, Some("/r/claude/abc".into())),
                Path::new("/work"),
            )
            .await
            .unwrap();
        record.thread = Some("sess-1".into());
        store.write(&record).await.unwrap();
        let file = queued(&store, &record.id, "again").await;

        let runner = FakeRunner::new().piping("/opt/claude", "{}\n", 0);
        run(&runner, &store, &record.id, "/opt/claude", None, &file)
            .await
            .unwrap();

        let calls = runner.calls();
        assert!(calls[0].contains("--resume sess-1"), "{}", calls[0]);
        // Without the home the harness reads a different profile's store, finds
        // no session, and starts over with nothing saying so.
        let env = runner.env_of("/opt/claude");
        assert!(
            env.contains(&("CLAUDE_CONFIG_DIR".to_string(), "/r/claude/abc".to_string())),
            "{env:?}"
        );
    }

    /// Org policy reaches a session `riabuild agents` started.
    ///
    /// This is the seam the feature lives on: every interactive Claude Code gets
    /// the team's settings from its account launcher, and a turn has no launcher
    /// in front of it — so before the file was passed here, the model the org
    /// chose and a lead's `permissions.deny` applied to `claude` and to nothing
    /// in this window.
    #[tokio::test]
    async fn a_turn_hands_claude_code_the_teams_settings() {
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        let file = queued(&store, &record.id, "hello").await;

        let runner = FakeRunner::new().piping("/opt/claude", "{}\n", 0);
        run(
            &runner,
            &store,
            &record.id,
            "/opt/claude",
            Some(Path::new("/r/org-settings.json")),
            &file,
        )
        .await
        .unwrap();

        let calls = runner.calls();
        assert!(
            calls[0].contains("--settings /r/org-settings.json"),
            "{}",
            calls[0]
        );
    }

    #[tokio::test]
    async fn a_harness_that_failed_says_so_where_a_window_can_see_it() {
        // The wrapper is detached, so its stderr goes nowhere, and the spool
        // holds only the vendor's wire format — a line riabuild wrote there
        // would decode to nothing. Without `errors.log` a harness that exits
        // without explaining itself is a session that sits idle for ever with
        // nothing on screen saying why.
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Grok, 1, None), Path::new("/work"))
            .await
            .unwrap();
        let file = queued(&store, &record.id, "hello").await;

        let runner = FakeRunner::new().spawning("/opt/grok", 2, "not signed in");
        let code = run(&runner, &store, &record.id, "/opt/grok", None, &file)
            .await
            .unwrap();
        assert_eq!(code, 2);

        let trouble = tokio::fs::read_to_string(store.trouble_path(&record.id))
            .await
            .unwrap();
        assert!(trouble.contains("exited 2"), "{trouble}");
        assert!(trouble.contains("not signed in"), "{trouble}");
        // and the prompt is still consumed, so it is not retried for ever
        assert!(!tokio::fs::try_exists(&file).await.unwrap());
    }

    #[tokio::test]
    async fn the_lock_is_held_for_the_whole_turn_and_given_back_after() {
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Codex, 1, None), Path::new("/work"))
            .await
            .unwrap();
        let file = queued(&store, &record.id, "hello").await;

        let runner = FakeRunner::new().piping("/opt/codex", "{\"type\":\"turn.completed\"}\n", 0);
        assert!(!store.running(&record.id).await);
        run(&runner, &store, &record.id, "/opt/codex", None, &file)
            .await
            .unwrap();
        // Released, so the next window reads this session as idle rather than as
        // permanently busy.
        assert!(!store.running(&record.id).await);
    }
}

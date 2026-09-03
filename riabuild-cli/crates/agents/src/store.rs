//! Where sessions live between windows.
//!
//! A session is a directory under `<root>/agents/<id>/`, and it holds four
//! things:
//!
//! | File | What |
//! |---|---|
//! | `meta.json` | the record: harness, thread id, profile home, checkout, title |
//! | `events.ndjson` | every turn's stdout, appended in order, exactly as the harness wrote it |
//! | `turn.lock` | held by the running turn, and by nothing else |
//! | `pending/*.txt` | prompts waiting for a turn to pick them up |
//! | `errors.log` | what riabuild itself could not do, which the spool cannot hold |
//!
//! # Why the spool is the harness's own bytes
//!
//! `events.ndjson` is not riabuild's event model — it is the raw NDJSON the
//! harness produced, appended across turns. Replaying it through
//! `riabuild_harness::Reader` yields precisely the events the window saw live,
//! because it is the same decoder over the same bytes. Storing decoded events
//! instead would have meant a second format to version, and a reopened session
//! that could disagree with the one that was on screen.
//!
//! `errors.log` is the other half of that. The spool holds one harness's wire
//! format and nothing else, so riabuild has nowhere in it to say "this binary
//! would not start" — and a detached wrapper has no stderr anybody is reading.
//! Without a second file, a harness that fails to launch produces a session that
//! sits idle with no explanation at all.
//!
//! # Liveness is a lock, never a pid
//!
//! Whether a turn is running is answered by trying to take `turn.lock`. If it is
//! free, nothing is running. This is the same decision `remote::channel::lease`
//! made and for the same reasons, which `CLAUDE.md` sets out: a pid in a file is
//! a claim somebody has to check, a marker outlives the process that wrote it,
//! and `kill -0` on a recycled pid answers about the wrong process. The kernel
//! releases an `flock` when the holder exits however it exits — including on a
//! reboot, which is exactly the case this feature has to get right.
//!
//! The lock is held by `riabuild internal agent-turn`, not by the harness: the
//! harness is a third-party binary that knows nothing about riabuild, and the
//! wrapper is a riabuild process whose whole life is that one turn.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use riabuild_harness::Kind;

use crate::account::Account;
use riabuild_paths::Paths;
use riabuild_paths::filelock::FileLock;
use riabuild_runner::{CommandRunner, RunOptions};
use serde::{Deserialize, Serialize};

/// The most sessions kept per checkout.
///
/// Sessions are never deleted by finishing, only by ageing out, so without a cap
/// this directory grows for as long as the developer keeps working. Fifty is far
/// more than a list is readable at and small enough that the oldest are
/// genuinely stale.
const KEEP: usize = 50;

/// How many pasted images are kept, over the whole store.
///
/// A smaller number than [`KEEP`] because the units are not comparable: a
/// session record is a few hundred bytes of JSON, and a pasted screenshot is a
/// few megabytes. Unbounded, this is the one directory riabuild writes that
/// grows with how much a developer works rather than with how much riabuild
/// has to remember.
const KEEP_IMAGES: usize = 20;

/// What riabuild remembers about one session.
///
/// Deliberately small, and deliberately not the transcript: the transcript is
/// the spool, and the harness's own store has it too. Everything here is what
/// riabuild cannot get back by asking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    /// The harness, by [`Kind::tag`] rather than as a serialised enum — a
    /// record has to survive a riabuild that reorders its variants.
    pub kind: String,
    /// What this session resumes under. `None` until the harness has said.
    #[serde(default)]
    pub thread: Option<String>,
    /// Which of that harness's nine sign-ins made this session, 1-based.
    ///
    /// Beside `home` and not instead of it: the home is what a turn runs under
    /// and the number is what a developer calls it. A record written before the
    /// window offered a choice was made under the first account, which is what
    /// the default says.
    #[serde(default = "first_account")]
    pub account: usize,
    /// The profile directory the session was created under.
    ///
    /// Stored rather than recomputed, because it is what resume depends on: if
    /// the primary Claude account changes between turns, a recomputed home
    /// would point at a different store and the session would silently start
    /// over as a new conversation.
    #[serde(default)]
    pub home: Option<PathBuf>,
    pub cwd: PathBuf,
    /// The first prompt, one line, for the list.
    #[serde(default)]
    pub title: String,
    pub created: u64,
    pub updated: u64,
}

/// What a record with no account named was made under, which is the only
/// account `riabuild agents` could reach when those records were written.
fn first_account() -> usize {
    1
}

impl Record {
    pub fn harness(&self) -> Option<Kind> {
        Kind::from_tag(&self.kind)
    }

    /// `claude-2`, `grok-1` — the launcher's spelling, for the list.
    pub fn account_name(&self) -> String {
        format!("{}-{}", self.kind, self.account)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// A session id: 32 hex characters from the OS.
///
/// Not a counter. Two windows can create a session at the same moment, and a
/// counter would have to be read, incremented and written under a lock to avoid
/// handing both the same directory.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    // A failure here means the OS has no randomness, which is not a state this
    // can recover from — but it is also not worth taking a whole window down
    // for, so it degrades to the clock. Two sessions created in the same second
    // on a machine with no entropy is a collision nobody will ever see.
    if getrandom::fill(&mut bytes).is_err() {
        let stamp = now().to_be_bytes();
        bytes[..8].copy_from_slice(&stamp);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The name a pasted file is written under.
///
/// The clock first, so a directory listing is in the order the developer pasted
/// and the oldest is the one [`Store::prune_images`] drops. [`new_id`] after it
/// because the clock's resolution is a second: two pastes inside one second are
/// two files, and a name that was only the clock would have the second
/// overwrite the first while the compose line still pointed at it.
pub fn stamped_name() -> String {
    format!("{}-{}", now(), new_id())
}

/// The clock half of a [`stamped_name`], for ordering. Zero for a name that was
/// not written by this — a developer's own file dropped in the directory sorts
/// oldest and is dropped first, which is the safe end to be wrong at.
fn stamped_at(name: &str) -> u64 {
    name.split('-')
        .next()
        .and_then(|at| at.parse().ok())
        .unwrap_or_default()
}

/// The sessions on this machine, for one developer.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(paths: &dyn Paths) -> Self {
        Self {
            root: paths.agents_dir(),
        }
    }

    /// For a test that wants a store in a temporary directory.
    pub fn rooted_at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    pub fn record_path(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("meta.json")
    }

    /// Where the harness's stdout is appended.
    pub fn spool_path(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("events.ndjson")
    }

    pub fn lock_path(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("turn.lock")
    }

    /// Where riabuild's own failures go.
    ///
    /// Separate from the spool because the spool is one vendor's wire format:
    /// a line riabuild wrote would decode to nothing, and the developer would
    /// see a session that simply never did anything.
    pub fn trouble_path(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("errors.log")
    }

    /// Where a pasted image is written.
    ///
    /// One directory for the whole store rather than one per session, because
    /// Ctrl-V is pressed while composing and the row under the cursor may still
    /// be an *offer* — there is no session directory to put it in until the
    /// prompt that names it has been sent. Splitting it across the two cases
    /// would put half a developer's pasted images somewhere the other half is
    /// not.
    ///
    /// It sits beside the sessions and is not one: [`Store::sessions`] reads
    /// every entry here and keeps only those with a readable `meta.json`, so a
    /// directory that has none is already skipped.
    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }

    /// The queue a turn takes its prompt from.
    ///
    /// One file per prompt rather than one file overwritten. Two prompts sent
    /// while a turn is running are two waiting wrappers, and a single
    /// `prompt.txt` would have the second overwrite the first before either
    /// had read it — losing a message the developer watched being accepted.
    pub fn pending_dir(&self, id: &str) -> PathBuf {
        self.session_dir(id).join("pending")
    }

    /// Every session for one checkout, newest first.
    ///
    /// Scoped by `cwd` because a developer opening the window in a repository
    /// is asking about that repository. A record naming a directory that no
    /// longer exists is still listed: the checkout may be on a disk that is not
    /// mounted right now, and forgetting a session because of that would lose
    /// the only handle to a conversation the harness still has.
    pub async fn sessions(&self, cwd: &Path) -> Result<Vec<Record>> {
        let mut found = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            // No directory yet is the first run, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
            Err(error) => return Err(error).context("could not read the agents directory"),
        };
        while let Some(entry) = entries.next_entry().await? {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // A record this riabuild cannot read is skipped, never fatal. It is
            // either half-written by a window that died mid-create, or written
            // by a newer riabuild — and neither is a reason to refuse to show
            // the sessions that *are* readable.
            let Ok(record) = self.read(&id).await else {
                continue;
            };
            if record.cwd == cwd && record.harness().is_some() {
                found.push(record);
            }
        }
        // Newest first, so the session a developer was last in is the one the
        // window opens on.
        found.sort_by_key(|record| std::cmp::Reverse(record.updated));
        Ok(found)
    }

    pub async fn read(&self, id: &str) -> Result<Record> {
        let text = tokio::fs::read_to_string(self.record_path(id)).await?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Creates a session directory and its record.
    ///
    /// Takes the whole account rather than a harness and a home: those two are
    /// one fact, and a signature that let them be passed separately is a
    /// signature that lets a session be recorded as `claude-2` while running out
    /// of `claude-1`'s store.
    pub async fn create(&self, account: &Account, cwd: &Path) -> Result<Record> {
        let id = new_id();
        tokio::fs::create_dir_all(self.session_dir(&id))
            .await
            .with_context(|| format!("could not make a directory for session {id}"))?;
        let stamp = now();
        let record = Record {
            id,
            kind: account.kind.tag().to_string(),
            thread: None,
            account: account.number,
            home: account.home.clone(),
            cwd: cwd.to_path_buf(),
            title: String::new(),
            created: stamp,
            updated: stamp,
        };
        self.write(&record).await?;
        Ok(record)
    }

    /// Lands a record by rename, the way every other riabuild state file is
    /// written: a half-written `meta.json` is a session whose thread id is gone.
    pub async fn write(&self, record: &Record) -> Result<()> {
        let path = self.record_path(&record.id);
        let temporary = path.with_extension("json.new");
        tokio::fs::write(&temporary, serde_json::to_vec_pretty(record)?).await?;
        tokio::fs::rename(&temporary, &path).await?;
        Ok(())
    }

    /// The whole spool, for rehydrating a session.
    ///
    /// A missing file is an empty transcript rather than an error: a session
    /// created and never spoken to has one.
    pub async fn spool(&self, id: &str) -> Result<String> {
        match tokio::fs::read_to_string(self.spool_path(id)).await {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error.into()),
        }
    }

    /// Whatever has been appended past `offset`, and the new offset.
    ///
    /// Whole lines only. A turn writes into this file while it is being read, so
    /// the last line is routinely half there — returning it would hand the
    /// decoder a truncated JSON object, and the offset would advance past bytes
    /// that were never decoded.
    pub async fn spool_since(&self, id: &str, offset: u64) -> Result<(String, u64)> {
        read_since(&self.spool_path(id), offset).await
    }

    /// The same, for the errors only riabuild can report.
    pub async fn trouble_since(&self, id: &str, offset: u64) -> Result<(String, u64)> {
        read_since(&self.trouble_path(id), offset).await
    }
}

/// Whatever has been appended to `path` past `offset`, and the new offset.
///
/// Whole lines only. A turn appends while a window reads, so the last line is
/// routinely half there — returning it would hand the decoder a truncated JSON
/// object, and the offset would advance past bytes that were never decoded.
async fn read_since(path: &Path, offset: u64) -> Result<(String, u64)> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((String::new(), offset));
        }
        Err(error) => return Err(error.into()),
    };
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut fresh = Vec::new();
    file.read_to_end(&mut fresh).await?;
    let complete = match fresh.iter().rposition(|byte| *byte == b'\n') {
        Some(last) => last + 1,
        None => return Ok((String::new(), offset)),
    };
    let text = String::from_utf8_lossy(&fresh[..complete]).into_owned();
    Ok((text, offset + complete as u64))
}

impl Store {
    /// Whether a turn is running.
    ///
    /// Answered by trying to take the lock and giving it straight back. `true`
    /// where the lock could not be taken, which includes a filesystem that does
    /// not support locking — the same conservative direction `engine::run_all`
    /// takes, since claiming a session is idle when it is not would start a
    /// second turn on top of the first.
    pub async fn running(&self, id: &str) -> bool {
        match FileLock::try_acquire(&self.lock_path(id)).await {
            Ok(Some(_lock)) => false,
            Ok(None) => true,
            Err(_) => true,
        }
    }

    /// Starts a turn, detached, and returns once it has been started.
    ///
    /// What is spawned is `riabuild internal agent-turn`, not the harness. The
    /// wrapper is what holds the lock, points the harness's stdout at the spool
    /// and records the thread id afterwards — none of which a third-party binary
    /// could be asked to do, and all of which have to outlive this window.
    pub async fn start_turn(
        &self,
        runner: &dyn CommandRunner,
        riabuild: &Path,
        record: &Record,
        prompt: &str,
    ) -> Result<()> {
        let pending = self.pending_dir(&record.id);
        tokio::fs::create_dir_all(&pending).await?;
        // Named by the clock so that two prompts queued in order are picked
        // up in order, and by chance so that two queued in the same second
        // are still two files.
        let file = pending.join(format!("{}-{}.txt", now(), new_id()));
        tokio::fs::write(&file, prompt).await?;
        runner
            .spawn_detached(
                &riabuild.display().to_string(),
                &[
                    "internal",
                    "agent-turn",
                    "--session",
                    &record.id,
                    "--prompt-file",
                    &file.display().to_string(),
                ],
                &RunOptions {
                    // The checkout. A turn is about a repository, and the
                    // wrapper passes this on to the harness.
                    cwd: Some(record.cwd.clone()),
                    ..RunOptions::default()
                },
            )
            .await
    }

    /// Drops the oldest sessions past [`KEEP`], for one checkout.
    ///
    /// A running session is never removed however old it is: the directory it
    /// is writing into would go with it.
    pub async fn prune(&self, cwd: &Path) -> Result<()> {
        let sessions = self.sessions(cwd).await?;
        for record in sessions.into_iter().skip(KEEP) {
            if self.running(&record.id).await {
                continue;
            }
            let _ = tokio::fs::remove_dir_all(self.session_dir(&record.id)).await;
        }
        Ok(())
    }

    /// Drops the oldest pasted images past [`KEEP_IMAGES`].
    ///
    /// Not scoped by checkout the way [`Store::prune`] is, because the
    /// directory is not: Ctrl-V happens before the prompt that names the image
    /// exists, so nothing has yet said which repository it belongs to.
    ///
    /// A missing directory is not a failure. Most developers never paste an
    /// image, and this runs on every window.
    pub async fn prune_images(&self) -> Result<()> {
        let dir = self.images_dir();
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            return Ok(());
        };
        let mut names: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort_by_key(|name| std::cmp::Reverse(stamped_at(name)));
        for name in names.into_iter().skip(KEEP_IMAGES) {
            let _ = tokio::fs::remove_file(dir.join(name)).await;
        }
        Ok(())
    }

    /// Removes one session's directory. `riabuild agents forget`.
    pub async fn forget(&self, id: &str) -> Result<()> {
        if self.running(id).await {
            anyhow::bail!("session {id} is running");
        }
        tokio::fs::remove_dir_all(self.session_dir(id)).await?;
        Ok(())
    }
}

/// One line of a prompt, for the session list.
pub fn title_of(prompt: &str) -> String {
    const MAX: usize = 60;
    let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;
    use std::sync::Arc;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(dir.path().join("agents"));
        (dir, store)
    }

    /// A pasted image lives beside the sessions and is not one. `sessions`
    /// keeps only the directories with a readable `meta.json`, so a store with
    /// an `images/` in it still lists exactly the sessions it has.
    #[tokio::test]
    async fn the_images_directory_is_not_mistaken_for_a_session() {
        let (_dir, store) = store();
        let account = Account::new(Kind::Claude, 1, None);
        store.create(&account, Path::new("/work")).await.unwrap();
        tokio::fs::create_dir_all(store.images_dir()).await.unwrap();
        tokio::fs::write(store.images_dir().join("x.png"), b"x")
            .await
            .unwrap();

        assert_eq!(store.sessions(Path::new("/work")).await.unwrap().len(), 1);
    }

    /// The cap is the whole reason this directory is safe to write into on a
    /// keypress: a screenshot is megabytes, and nothing else riabuild stores
    /// grows with how much a developer works.
    #[tokio::test]
    async fn pasted_images_are_capped_at_the_newest() {
        let (_dir, store) = store();
        let dir = store.images_dir();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // Named the way `stamped_name` names them, oldest second first.
        for second in 0..(KEEP_IMAGES as u64 + 5) {
            tokio::fs::write(dir.join(format!("{second}-{second:032x}.png")), b"x")
                .await
                .unwrap();
        }

        store.prune_images().await.unwrap();

        let mut left = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            left.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(left.len(), KEEP_IMAGES);
        // The newest survive, which is the half a compose line might still be
        // pointing at.
        assert!(left.iter().all(|name| stamped_at(name) >= 5), "{left:?}");
    }

    /// Called on every window, and most developers have never pasted anything.
    #[tokio::test]
    async fn pruning_images_that_were_never_pasted_is_not_a_failure() {
        let (_dir, store) = store();
        assert!(store.prune_images().await.is_ok());
    }

    #[tokio::test]
    async fn a_session_remembers_which_sign_in_made_it() {
        // The number is what the list shows and the home is what the turn runs
        // under. A session made on `claude-3` that came back as `claude-1` would
        // be a developer sending work to the wrong account with the right label
        // on it.
        let (_dir, store) = store();
        let account = Account::new(Kind::Grok, 3, Some("/r/grok/3".into()));
        let record = store.create(&account, Path::new("/work")).await.unwrap();
        assert_eq!(record.account, 3);

        let read = store.read(&record.id).await.unwrap();
        assert_eq!(read.account, 3);
        assert_eq!(read.home, Some(PathBuf::from("/r/grok/3")));
        assert_eq!(read.account_name(), "grok-3");
    }

    #[tokio::test]
    async fn a_record_written_before_accounts_reads_as_the_first_one() {
        // The only account `riabuild agents` could reach when those records were
        // written. Anything else would relabel every session on disk.
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        let text = tokio::fs::read_to_string(store.record_path(&record.id))
            .await
            .unwrap();
        let older: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut older = older.as_object().unwrap().clone();
        older.remove("account");
        tokio::fs::write(
            store.record_path(&record.id),
            serde_json::to_vec(&older).unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(store.read(&record.id).await.unwrap().account, 1);
    }

    #[tokio::test]
    async fn a_session_survives_being_written_and_read_back() {
        let (_dir, store) = store();
        let cwd = Path::new("/work/repo");
        let mut record = store
            .create(&Account::new(Kind::Claude, 1, None), cwd)
            .await
            .unwrap();
        record.thread = Some("abc".into());
        record.title = "fix the bug".into();
        store.write(&record).await.unwrap();

        let read = store.read(&record.id).await.unwrap();
        assert_eq!(read.thread.as_deref(), Some("abc"));
        assert_eq!(read.harness(), Some(Kind::Claude));
        assert_eq!(read.cwd, cwd);
    }

    #[tokio::test]
    async fn sessions_are_scoped_to_the_checkout_they_were_opened_in() {
        // Opening the window in one repository must not show the agents from
        // another: the list is short and about what is in front of you.
        let (_dir, store) = store();
        store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work/one"))
            .await
            .unwrap();
        store
            .create(&Account::new(Kind::Codex, 1, None), Path::new("/work/two"))
            .await
            .unwrap();

        let one = store.sessions(Path::new("/work/one")).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].harness(), Some(Kind::Claude));
        assert!(
            store
                .sessions(Path::new("/work/three"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn no_agents_directory_at_all_is_a_first_run_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(dir.path().join("never-made"));
        assert!(store.sessions(Path::new("/work")).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_record_that_cannot_be_read_is_skipped_rather_than_fatal() {
        // Half-written by a window that died, or written by a newer riabuild.
        // Either way the sessions beside it must still be listed.
        let (_dir, store) = store();
        let good = store
            .create(&Account::new(Kind::Grok, 1, None), Path::new("/work"))
            .await
            .unwrap();
        let broken = store.session_dir("not-a-session");
        tokio::fs::create_dir_all(&broken).await.unwrap();
        tokio::fs::write(broken.join("meta.json"), "{ this is not json")
            .await
            .unwrap();

        let sessions = store.sessions(Path::new("/work")).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, good.id);
    }

    #[tokio::test]
    async fn a_record_naming_an_unknown_harness_is_skipped() {
        // A session created by a riabuild that drives a fourth harness. The row
        // is lost on a downgrade; the file is not.
        let (_dir, store) = store();
        let mut record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        record.kind = "gemini".into();
        store.write(&record).await.unwrap();
        assert!(store.sessions(Path::new("/work")).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_idle_session_is_not_running_and_a_held_lock_is() {
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        assert!(!store.running(&record.id).await);

        let held = FileLock::try_acquire(&store.lock_path(&record.id))
            .await
            .unwrap();
        assert!(held.is_some());
        assert!(store.running(&record.id).await);
        drop(held);
        // Released by the kernel, which is what makes a reboot answer correctly
        // without anything having to clean up after it.
        assert!(!store.running(&record.id).await);
    }

    #[tokio::test]
    async fn a_spool_is_read_back_whole_and_then_followed() {
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        assert_eq!(store.spool(&record.id).await.unwrap(), "");

        tokio::fs::write(store.spool_path(&record.id), "one\ntwo\n")
            .await
            .unwrap();
        let (text, offset) = store.spool_since(&record.id, 0).await.unwrap();
        assert_eq!(text, "one\ntwo\n");
        assert_eq!(offset, 8);

        // Nothing new is nothing read.
        let (again, same) = store.spool_since(&record.id, offset).await.unwrap();
        assert_eq!(again, "");
        assert_eq!(same, offset);
    }

    #[tokio::test]
    async fn a_half_written_line_is_left_for_the_next_read() {
        // A turn appends while the window reads. Handing the decoder a truncated
        // JSON object would drop that event for ever, because the offset would
        // have moved past it.
        let (_dir, store) = store();
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        tokio::fs::write(store.spool_path(&record.id), "{\"a\":1}\n{\"b\":")
            .await
            .unwrap();

        let (text, offset) = store.spool_since(&record.id, 0).await.unwrap();
        assert_eq!(text, "{\"a\":1}\n");
        assert_eq!(offset, 8);

        tokio::fs::write(store.spool_path(&record.id), "{\"a\":1}\n{\"b\":2}\n")
            .await
            .unwrap();
        let (rest, _) = store.spool_since(&record.id, offset).await.unwrap();
        assert_eq!(rest, "{\"b\":2}\n");
    }

    #[tokio::test]
    async fn starting_a_turn_detaches_the_wrapper_rather_than_the_harness() {
        // The harness is started by the wrapper, inside the turn. What this
        // spawns must be riabuild itself, or nothing holds the lock and nothing
        // records the thread id.
        let (_dir, store) = store();
        let runner = Arc::new(FakeRunner::new());
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();

        store
            .start_turn(
                runner.as_ref(),
                Path::new("/opt/riabuild/riabuild"),
                &record,
                "do the thing",
            )
            .await
            .unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].starts_with(&format!(
                "/opt/riabuild/riabuild internal agent-turn --session {} --prompt-file ",
                record.id
            )),
            "{}",
            calls[0]
        );
        // And the prompt is on disk for it to read, not in argv where `ps`
        // would show it to everyone on a shared server.
        let mut queued = tokio::fs::read_dir(store.pending_dir(&record.id))
            .await
            .unwrap();
        let file = queued.next_entry().await.unwrap().unwrap();
        let written = tokio::fs::read_to_string(file.path()).await.unwrap();
        assert_eq!(written, "do the thing");
    }

    #[tokio::test]
    async fn pruning_keeps_the_newest_and_never_removes_a_running_session() {
        let (_dir, store) = store();
        let cwd = Path::new("/work");
        let mut ids = Vec::new();
        for index in 0..(KEEP + 3) {
            let mut record = store
                .create(&Account::new(Kind::Claude, 1, None), cwd)
                .await
                .unwrap();
            // Distinct timestamps, so "newest" is well defined without sleeping.
            record.updated = 1_000 + index as u64;
            store.write(&record).await.unwrap();
            ids.push(record.id);
        }
        // The oldest of all, held open the way a running turn holds it.
        let held = FileLock::try_acquire(&store.lock_path(&ids[0]))
            .await
            .unwrap();

        store.prune(cwd).await.unwrap();
        let left = store.sessions(cwd).await.unwrap();
        // The cap, plus the running one that could not be removed.
        assert_eq!(left.len(), KEEP + 1);
        assert!(left.iter().any(|record| record.id == ids[0]));
        drop(held);
    }

    #[test]
    fn a_title_is_one_short_line_of_the_first_prompt() {
        assert_eq!(title_of("  fix\n  the   bug "), "fix the bug");
        let long = "x".repeat(200);
        assert!(title_of(&long).chars().count() <= 61);
    }

    #[test]
    fn two_session_ids_are_not_the_same() {
        // Two windows can create a session in the same instant, and a counter
        // would hand both the same directory.
        assert_ne!(new_id(), new_id());
        assert_eq!(new_id().len(), 32);
    }
}

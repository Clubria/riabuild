//! One Codex session, opened on another session's behalf.
//!
//! Everything here is `riabuild agents` machinery used from outside the window:
//! the store makes the session directory, `turn::run` holds its lock and copies
//! the harness's NDJSON into its spool, and `riabuild_harness::Reader` decodes
//! that NDJSON into events. What this module adds is the **filter** — of
//! everything a turn said, exactly one thing goes back to the caller.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use riabuild_agents::store::{self, Record, Store};
use riabuild_agents::{Account, DELEGATING_SESSION, turn};
use riabuild_harness::{Event, Kind, Reader};
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;

/// What one delegated turn is worth reporting.
///
/// Deliberately not the transcript. A `codex exec` turn reads files, runs
/// commands and reasons out loud, and all of it is already on screen in
/// `riabuild agents` and in the session's own spool — sending it back through a
/// tool result would put a whole second agent's working notes into the context
/// of the agent that asked, which is the one thing this exists to avoid.
pub struct Reply {
    /// The store id, which is what `codex_reply` takes to continue this thread.
    pub session: String,
    /// The last thing Codex said. `None` for a turn that ended without saying
    /// anything, which is a failure however it exited.
    pub said: Option<String>,
    pub input: u64,
    pub output: u64,
    /// What went wrong, in Codex's words. Reported even when a reply arrived:
    /// Codex retries a failed transport five times and says so each time, and a
    /// turn can carry those notices and still answer.
    pub trouble: Vec<String>,
}

/// Everything a delegation needs that does not change between calls.
pub struct Delegate {
    store: Store,
    /// The Codex binary, by absolute path. Resolved by the caller for the reason
    /// `internal::agent_turn` resolves it there: a versioned path moves with
    /// every riabuild upgrade, so it may be looked up but never recorded.
    codex: String,
    /// Which `CODEX_HOME` this runs under, 1-based, the same number the launcher
    /// carries — profile 3 is `codex-3`.
    profile: usize,
    home: PathBuf,
    org_settings: Option<PathBuf>,
}

impl Delegate {
    pub fn new(
        paths: &dyn Paths,
        codex: String,
        profile: usize,
        org_settings: Option<PathBuf>,
    ) -> Self {
        Self {
            store: Store::new(paths),
            codex,
            home: paths.codex_profile_dir(profile),
            profile,
            org_settings,
        }
    }

    /// The launcher a developer would type to reach this profile.
    ///
    /// `codex` and `codex-1` are the same sign-in, and the bare name is the one
    /// to print: it is what somebody who has never made a second profile has.
    fn launcher(&self) -> String {
        if self.profile == 1 {
            "codex".to_string()
        } else {
            format!("codex-{}", self.profile)
        }
    }

    /// The session that started the Claude Code this server is serving.
    ///
    /// `None` where riabuild did not start it — a `~/.riabuild/bin/claude` in a
    /// terminal — which is not an error. A delegation from one of those is a
    /// session with no parent, drawn at the top level of the rail like any
    /// other.
    async fn parent(&self) -> Option<Record> {
        let id = std::env::var(DELEGATING_SESSION).ok()?;
        self.store.read(&id).await.ok()
    }

    /// Where a delegated session works.
    ///
    /// The parent's checkout, so that parent and child are listed together —
    /// `Store::sessions` is scoped by directory, and a child recorded under
    /// another path would be invisible in the window the parent is in.
    ///
    /// With no parent it is this process's own directory, which Claude Code sets
    /// to the project it is open on when it spawns an MCP server.
    async fn cwd(&self, parent: Option<&Record>) -> PathBuf {
        match parent {
            Some(record) => record.cwd.clone(),
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Refuses a second level of delegation.
    ///
    /// Only Claude Code is given this server, so a Codex session made here has
    /// no way to reach it and this cannot fire today. It is written down anyway
    /// because the day it *can* fire is a day somebody added the MCP entry to a
    /// second harness, and the failure then is silent rather than loud: the rail
    /// draws one level, so a grandchild would be listed as a sibling of its own
    /// parent and the developer would have no way to tell which session had
    /// actually asked for it.
    fn within_depth(parent: Option<&Record>) -> Result<()> {
        match parent {
            Some(record) if record.parent.is_some() => anyhow::bail!(
                "this session is already a subagent, and riabuild allows one level of \
                 delegation. Do the work here, or ask the session that started this one."
            ),
            _ => Ok(()),
        }
    }

    /// Whether this profile has a Codex sign-in at all.
    ///
    /// Checked before a turn rather than after, because the failure otherwise is
    /// a `turn.failed` carrying OpenAI's 401 — which reaches the calling agent
    /// as an opaque tool error, is retried, and tells the developer nothing
    /// about the one thing that would fix it. riabuild signs nobody in to Codex
    /// by design, so this is the state a laptop is in until somebody does.
    ///
    /// `OPENAI_API_KEY` counts. Codex reads it in place of `auth.json`, and a
    /// machine set up that way is signed in whatever this directory holds.
    async fn signed_in(&self) -> Result<()> {
        if std::env::var("OPENAI_API_KEY").is_ok_and(|key| !key.trim().is_empty()) {
            return Ok(());
        }
        let auth = self.home.join("auth.json");
        if tokio::fs::try_exists(&auth).await.unwrap_or(false) {
            return Ok(());
        }
        anyhow::bail!(
            "Codex is not signed in on this machine, so there is nothing to delegate to. \
             Run `{} login` in a terminal, then ask again. riabuild installs Codex but \
             signs nobody in to it.",
            self.launcher()
        )
    }

    /// Opens a session and asks it the first thing.
    pub async fn start(&self, runner: &dyn CommandRunner, prompt: &str) -> Result<Reply> {
        let parent = self.parent().await;
        Self::within_depth(parent.as_ref())?;
        self.signed_in().await?;

        let cwd = self.cwd(parent.as_ref()).await;
        let account = Account::new(Kind::Codex, self.profile, Some(self.home.clone()));
        let mut record = self
            .store
            .create_under(
                &account,
                &cwd,
                parent.as_ref().map(|record| record.id.as_str()),
            )
            .await
            .context("could not open a Codex session")?;

        // The same title the window would give it, from the same function: the
        // first prompt is what tells two sessions apart on the rail, and a
        // delegated one that arrived without a developer typing it needs that
        // more than most.
        record.title = store::title_of(prompt);
        self.store.write(&record).await?;

        self.turn(runner, &record.id, prompt).await
    }

    /// Asks a session that already exists something else.
    pub async fn resume(
        &self,
        runner: &dyn CommandRunner,
        session: &str,
        prompt: &str,
    ) -> Result<Reply> {
        let record = self
            .store
            .read(session)
            .await
            .with_context(|| format!("there is no Codex session {session} on this machine"))?;
        if record.harness() != Some(Kind::Codex) {
            anyhow::bail!("session {session} is not a Codex session");
        }
        self.signed_in().await?;
        self.turn(runner, session, prompt).await
    }

    /// One turn, and what it said.
    ///
    /// The spool is read from the length it had *before* the turn rather than
    /// from zero, so a fifth question to one session returns the answer to the
    /// fifth question. Reading the whole spool would hand back every reply the
    /// thread has ever given, which is the transcript-shaped failure this whole
    /// design is avoiding, arrived at from the other direction.
    async fn turn(&self, runner: &dyn CommandRunner, id: &str, prompt: &str) -> Result<Reply> {
        let before = self.store.spool(id).await.unwrap_or_default().len() as u64;
        // Two files with two writers, so two offsets — the same reason a pane
        // keeps two. The spool is Codex's own bytes; this is riabuild saying
        // what it could not do, and a turn that fails before Codex ever starts
        // leaves a line here and nothing there at all.
        let (_, troubled_at) = self.store.trouble_since(id, 0).await.unwrap_or_default();

        let pending = self.store.pending_dir(id);
        tokio::fs::create_dir_all(&pending).await?;
        // A file rather than an argument, for the reason `Store::start_turn`
        // writes one: argv is world-readable through `ps`, and on a shared
        // server `ps` shows other developers' processes.
        let file = pending.join(format!("mcp-{}.txt", stamp()));
        tokio::fs::write(&file, prompt).await?;

        // The same wrapper the window runs, in this process rather than
        // detached. It holds the session's lock, appends the spool and writes
        // down the thread id — so a delegated session resumes, is followed live
        // by any open window, and is still there tomorrow.
        turn::run(
            runner,
            &self.store,
            id,
            &self.codex,
            self.org_settings.as_deref(),
            &file,
        )
        .await
        .with_context(|| format!("the Codex turn in session {id} could not be run"))?;

        let (written, _) = self.store.spool_since(id, before).await.unwrap_or_default();
        let (troubled, _) = self
            .store
            .trouble_since(id, troubled_at)
            .await
            .unwrap_or_default();

        let mut reply = read_back(id.to_string(), &written);
        reply.trouble.extend(
            troubled
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string),
        );
        Ok(reply)
    }
}

/// What one turn's worth of NDJSON amounts to.
///
/// Split out from the turn so it can be tested against captured bytes rather
/// than against a signed-in OpenAI account, which is the same reasoning
/// `riabuild-harness` is built on.
fn read_back(session: String, spool: &str) -> Reply {
    let mut reader = Reader::new(Kind::Codex);
    let mut reply = Reply {
        session,
        said: None,
        input: 0,
        output: 0,
        trouble: Vec::new(),
    };
    for line in spool.lines() {
        for event in reader.read(line) {
            match event {
                // The last one wins. A turn may say several things — Codex emits
                // an `agent_message` per assistant turn — and what the caller
                // asked for is the answer, which is the one at the end.
                Event::Said(text) if !text.trim().is_empty() => reply.said = Some(text),
                Event::Usage { input, output } => {
                    reply.input += input;
                    reply.output += output;
                }
                Event::Trouble(text) => reply.trouble.push(text),
                // Everything else is the working — reasoning, shell commands,
                // file edits, MCP calls of its own. It is on the spool, it is in
                // the window, and it is deliberately not in this reply.
                _ => {}
            }
        }
    }
    reply
}

/// Nanoseconds since the epoch, for a prompt file nothing else will be named.
fn stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure path of a real `codex exec --json`, captured from
    /// codex-cli 0.148.0 — the same transcript `riabuild-harness` is pinned
    /// against.
    const FAILED: &str = r#"{"type":"thread.started","thread_id":"01999c"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"error","message":"stream error: unexpected status 401 Unauthorized"}}
{"type":"turn.failed","error":{"message":"stream error: unexpected status 401 Unauthorized"}}"#;

    #[test]
    fn a_turn_that_only_failed_carries_no_reply() {
        let reply = read_back("s1".into(), FAILED);
        assert!(reply.said.is_none(), "{:?}", reply.said);
        assert!(
            reply.trouble.iter().any(|line| line.contains("401")),
            "{:?}",
            reply.trouble
        );
    }

    #[test]
    fn only_the_last_thing_said_comes_back() {
        let spool = r#"{"type":"thread.started","thread_id":"t"}
{"type":"item.completed","item":{"id":"a","type":"agent_message","text":"first"}}
{"type":"item.completed","item":{"id":"b","type":"reasoning","text":"thinking out loud"}}
{"type":"item.completed","item":{"id":"c","type":"command_execution","command":"ls","exit_code":0}}
{"type":"item.completed","item":{"id":"d","type":"agent_message","text":"second"}}
{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":5,"output_tokens":3}}"#;
        let reply = read_back("s1".into(), spool);
        assert_eq!(reply.said.as_deref(), Some("second"));
        assert_eq!(reply.input, 15);
        assert_eq!(reply.output, 3);
    }

    #[test]
    fn the_working_is_not_in_the_reply() {
        let spool = r#"{"type":"item.completed","item":{"id":"c","type":"command_execution","command":"cargo test","exit_code":0}}
{"type":"item.completed","item":{"id":"d","type":"agent_message","text":"the tests pass"}}"#;
        let reply = read_back("s1".into(), spool);
        let said = reply.said.unwrap_or_default();
        assert!(!said.contains("cargo test"), "{said}");
        assert_eq!(said, "the tests pass");
    }

    #[test]
    fn a_subagent_may_not_delegate() {
        let child = Record {
            id: "child".into(),
            kind: "codex".into(),
            thread: None,
            parent: Some("parent".into()),
            account: 1,
            home: None,
            cwd: PathBuf::from("/work"),
            title: String::new(),
            created: 0,
            updated: 0,
        };
        let refused = Delegate::within_depth(Some(&child));
        assert!(refused.is_err());
        assert!(
            format!("{:#}", refused.err().unwrap_or_else(|| anyhow::anyhow!("")))
                .contains("one level"),
        );
    }

    #[test]
    fn a_session_a_developer_started_may_delegate() {
        let root = Record {
            id: "root".into(),
            kind: "claude".into(),
            thread: None,
            parent: None,
            account: 1,
            home: None,
            cwd: PathBuf::from("/work"),
            title: String::new(),
            created: 0,
            updated: 0,
        };
        assert!(Delegate::within_depth(Some(&root)).is_ok());
        assert!(Delegate::within_depth(None).is_ok());
    }
}

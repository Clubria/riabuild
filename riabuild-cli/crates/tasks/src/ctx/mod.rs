//! `Ctx` — everything a task is allowed to touch.
//!
//! The struct itself, how a run assembles one, and the two writes that go
//! through the state lock. Its three other jobs are siblings: `connect` is
//! the pair of requests that establish who this machine belongs to,
//! `checkout` is which repository the run is about and where its tree
//! lives, and `tools` is how a task names a binary riabuild owns.

mod checkout;
mod connect;
mod tools;

use crate::scope::Scope;
use anyhow::Result;
use riabuild_api::org::OrgConfig;
use riabuild_api::{ApiClient, Member, Repo};
use riabuild_keychain::Keychain;
use riabuild_paths::Paths;
use riabuild_paths::config::{State, UserConfig};
use riabuild_runner::CommandRunner;
use riabuild_ui::Ui;
use std::sync::Arc;

/// Everything a task is allowed to touch.
pub struct Ctx {
    pub paths: Arc<dyn Paths>,
    pub runner: Arc<dyn CommandRunner>,
    pub keychain: Arc<dyn Keychain>,
    pub api: ApiClient,
    pub ui: Ui,
    pub config: UserConfig,
    pub state: State,
    pub org: Option<OrgConfig>,
    /// The repository this run is about, once something has decided which.
    ///
    /// Set from `config.active_repo`, from `--repo`, or by the picker — never by
    /// a task. Tasks read it through `Ctx::repo`, which is the only thing they
    /// may name a repository by: `org.repo_slug` is the *default* the picker
    /// offers, and reading it in a task is how a run ends up cloning one
    /// repository and provisioning another.
    pub repo: Option<Repo>,
    pub member: Option<Member>,
    /// The server this riabuild is managed from, when it is on one.
    ///
    /// The only remote-mode fact a task is allowed to branch on. Everything else
    /// arrives through `ScopedRunner` and `Paths`, precisely so tasks do not
    /// grow remote-mode branches.
    pub server: Option<String>,
    pub cli_version: String,
    /// Environment the shell will be spawned with.
    pub env: Vec<(String, String)>,
    /// Report-only findings, printed after the run. `repo_status` fills this.
    pub notes: Vec<String>,
    /// Set when the developer asked for checks only.
    pub dry_run: bool,
}

impl Ctx {
    /// Assembles the `Ctx` a run works against.
    ///
    /// Lives beside `Ctx` rather than in `main` so the one field that comes
    /// from `Scope` — `server` — is testable without standing up
    /// `RealPaths::new()`, a real `ApiClient`, or a platform keychain.
    /// `Ctx.server` is the only remote-mode fact a task is allowed to branch
    /// on (see the field's own comment), and this is the one place it is set
    /// from the environment riabuild actually found itself in — hardcoding
    /// `None` here is the regression that leaves per-developer checkout
    /// namespacing (`paths::remote_project_dir`, `Ctx::default_checkout`) dead
    /// on every server despite compiling and passing every other test. See
    /// ruling R11 in `.superpowers/sdd/2026-08-06-remote-mode/decisions.md`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope: &Scope,
        paths: Arc<dyn Paths>,
        runner: Arc<dyn CommandRunner>,
        keychain: Arc<dyn Keychain>,
        ui: Ui,
        config: UserConfig,
        state: State,
        dry_run: bool,
    ) -> Ctx {
        Ctx {
            paths,
            runner,
            keychain,
            api: ApiClient::new(riabuild_version::VERSION),
            ui,
            config,
            state,
            org: None,
            repo: None,
            member: None,
            server: scope.server.clone(),
            cli_version: riabuild_version::VERSION.to_string(),
            env: Vec::new(),
            notes: Vec::new(),
            dry_run,
        }
    }

    /// A private `Ctx` for one task of a concurrent wave.
    ///
    /// `apply()` takes `&mut Ctx` and two of those cannot exist at once, which
    /// is the whole of what stood between this engine and running a wave
    /// concurrently. A fork sidesteps it rather than weakening it: each task
    /// gets its own, and [`Ctx::absorb`] merges the result back in declaration
    /// order. Tasks are untouched — the signature they are written against is
    /// the one they still get.
    ///
    /// What a fork carries:
    ///
    /// - **`ui`** is [`Ui::buffered`], so nothing this task prints reaches the
    ///   terminal until its turn.
    /// - **`config` and `state`** are this run's snapshots, cloned. They were
    ///   already snapshots rather than the authority — every write goes through
    ///   `update_config`/`update_state`, which re-read *inside* the file lock —
    ///   so two forks writing cannot lose each other's edit. What a fork does
    ///   not see is a *sibling's* write landing mid-wave, and that is sound for
    ///   the reason the wave exists: same-wave tasks have no declared edge, and
    ///   a task that needs to see another's write is a task with a missing
    ///   `depends_on()`.
    /// - **`notes`** starts empty, so `absorb` appends exactly what this task
    ///   added rather than a second copy of the run's.
    ///
    /// And what it must not carry back, which is why only non-interactive tasks
    /// are ever forked: `org`, `member` and the token inside `api` are set by
    /// `login` alone. `ApiClient::set_token` writes a plain field, so a clone
    /// does not share it — a forked `login` would sign in and throw the session
    /// away. `login` declares `Task::interactive()` and runs against the run's
    /// own `Ctx`, so the case never arises; the assertion in
    /// `engine::wave::tests` is what keeps it that way.
    pub(crate) fn fork(&self) -> Ctx {
        Ctx {
            paths: Arc::clone(&self.paths),
            runner: Arc::clone(&self.runner),
            keychain: Arc::clone(&self.keychain),
            api: self.api.clone(),
            ui: self.ui.buffered(),
            config: self.config.clone(),
            state: self.state.clone(),
            org: self.org.clone(),
            repo: self.repo.clone(),
            member: self.member.clone(),
            server: self.server.clone(),
            cli_version: self.cli_version.clone(),
            env: self.env.clone(),
            notes: Vec::new(),
            dry_run: self.dry_run,
        }
    }

    /// Takes back what one forked task produced: its output, then its notes.
    ///
    /// Called in declaration order, which is what makes a concurrent wave read
    /// exactly like the sequential one that came before it.
    ///
    /// `config` and `state` are deliberately *not* merged here. Both were
    /// written to disk under the lock by whichever forks wrote them, and
    /// picking one fork's snapshot would discard another's; the run reloads
    /// both from disk once the wave is over, where the file is the authority it
    /// always was.
    pub(crate) fn absorb(&mut self, fork: Ctx) {
        fork.ui.flush_into(&self.ui);
        self.notes.extend(fork.notes);
    }

    /// Re-reads the two files a wave's tasks may have written, so the run's
    /// snapshots are the machine's again.
    pub(crate) async fn reload(&mut self) {
        self.config = UserConfig::load(self.paths.as_ref()).await;
        self.state = State::load(self.paths.as_ref()).await;
    }

    pub fn org(&self) -> Result<&OrgConfig> {
        self.org
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("riabuild has not loaded the team configuration yet"))
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// Applies `mutate` to the config on disk under the lock, and refreshes this
    /// run's copy from what actually landed.
    ///
    /// `ctx.config` is a read-only snapshot for the run, not the authority —
    /// which is what it always was in truth. Every write goes through here so
    /// that the read it is based on happens inside the lock.
    pub async fn update_config(&mut self, mutate: impl FnOnce(&mut UserConfig)) -> Result<()> {
        self.config = UserConfig::update(self.paths.as_ref(), mutate).await?;
        Ok(())
    }

    /// Applies `mutate` to the state on disk under the lock, and refreshes this
    /// run's copy from what actually landed.
    pub async fn update_state(&mut self, mutate: impl FnOnce(&mut State)) -> Result<()> {
        self.state = State::update(self.paths.as_ref(), mutate).await?;
        Ok(())
    }
}

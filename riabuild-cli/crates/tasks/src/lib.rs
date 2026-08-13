//! The setup tasks and the context they run against.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct. The `feature = "testing"` half matters as much as the `test` half:
// when a downstream crate turns the feature on, this crate is compiled as a
// dependency and `cfg(test)` is false, so the exemption would not apply.
#![cfg_attr(any(test, feature = "testing"), allow(clippy::unwrap_used))]

pub mod accounts;
pub mod claude_accounts;
pub mod claude_agents_view;
pub mod claude_config;
pub mod claude_onboarding;
pub mod claude_plugins;
pub mod claude_statusline;
pub mod claude_trust;
pub mod engine;
pub mod env_local;
pub mod github_cli;
pub mod infisical_cli;
pub mod login;
pub mod org_settings;
pub mod project;
pub mod repo_status;
pub mod scope;
pub mod shell;
pub mod shims;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod toolchain;

use crate::scope::Scope;
use anyhow::Result;
use async_trait::async_trait;
use riabuild_api::org::OrgConfig;
use riabuild_api::{ApiClient, ApiError, Member};
use riabuild_keychain::Keychain;
use riabuild_paths::Paths;
use riabuild_paths::config::{State, UserConfig};
use riabuild_runner::CommandRunner;
use riabuild_ui::Ui;
use std::sync::Arc;

pub type TaskId = &'static str;

/// Why a task is about to run. Surfaced to the developer, so it reads as an
/// explanation rather than a status code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    NeverRun,
    VersionChanged { from: u32, to: u32 },
    UpstreamChanged(TaskId),
    CheckFailed(String),
}

impl Reason {
    pub fn describe(&self) -> String {
        match self {
            Reason::NeverRun => "first run".to_string(),
            Reason::VersionChanged { from, to } => {
                format!("riabuild changed what this should look like (v{from} → v{to})")
            }
            Reason::UpstreamChanged(id) => format!("{id} changed"),
            Reason::CheckFailed(detail) => detail.clone(),
        }
    }

    /// Stored in `state.json` for the next run to report against.
    pub fn tag(&self) -> String {
        match self {
            Reason::NeverRun => "never_run".into(),
            Reason::VersionChanged { .. } => "version_changed".into(),
            Reason::UpstreamChanged(id) => format!("upstream:{id}"),
            Reason::CheckFailed(_) => "check_failed".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Satisfied,
    Needs(Reason),
}

impl Status {
    /// Convenience for the common `check()` shape.
    pub fn needs(detail: impl Into<String>) -> Status {
        Status::Needs(Reason::CheckFailed(detail.into()))
    }
}

#[async_trait]
pub trait Task: Send + Sync {
    fn id(&self) -> TaskId;
    fn title(&self) -> &str;
    /// Forced-rerun escape hatch for drift `check()` genuinely cannot observe.
    /// `check()` is authoritative; bumping this to paper over a weak check is a
    /// bug in the check.
    fn version(&self) -> u32;
    fn depends_on(&self) -> &[TaskId];
    async fn check(&self, ctx: &Ctx) -> Result<Status>;
    async fn apply(&self, ctx: &mut Ctx) -> Result<()>;
}

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
            member: None,
            server: scope.server.clone(),
            cli_version: riabuild_version::VERSION.to_string(),
            env: Vec::new(),
            notes: Vec::new(),
            dry_run,
        }
    }

    /// Asks riabuild-web who this machine belongs to, before any task runs.
    ///
    /// A missing or expired session is not an error here — the `login` task
    /// exists to fix exactly that. Anything else (suspended, removed from the
    /// org) is surfaced immediately, because no amount of provisioning will
    /// help.
    pub async fn connect(&mut self) -> Result<()> {
        let Some(token) = self.keychain.get().await? else {
            return Ok(());
        };
        self.api.set_token(Some(token));

        match self.api.me().await {
            Ok(member) => {
                self.member = Some(member);
                self.org = Some(riabuild_api::org::fetch_config(&self.api).await?);
                Ok(())
            }
            Err(error) => match error.downcast_ref::<ApiError>() {
                Some(api_error) if api_error.needs_login() => {
                    self.api.set_token(None);
                    Ok(())
                }
                _ => Err(error),
            },
        }
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

    /// The project directory the developer chose, if one has been chosen.
    pub fn project_dir(&self) -> Option<std::path::PathBuf> {
        self.config
            .project_path
            .as_deref()
            .map(|path| riabuild_paths::expand_tilde(path, &self.paths.home()))
    }

    /// Where a checkout goes when the developer has not chosen a place.
    ///
    /// On a laptop this is simply the platform default. On a server several
    /// developers share one Unix account, so the checkout is grouped under the
    /// developer's own GitHub login instead — never the shared default — so one
    /// developer's branches, uncommitted work, and `.env.local` never land in
    /// another's session. See `paths::remote_project_dir`.
    pub async fn default_checkout(&self) -> std::path::PathBuf {
        let repo = self
            .org
            .as_ref()
            .map(|org| org.repo_name())
            .unwrap_or("repo");
        let home = self.paths.home();
        let Some(login) = self
            .server
            .as_ref()
            .and(self.member.as_ref())
            .map(|member| member.github_login.clone())
        else {
            return riabuild_paths::default_project_dir(&home, repo);
        };

        // A GitHub login can be freed and taken by somebody else, and a
        // directory can predate riabuild. Claim beside it rather than into it.
        for suffix in 1.. {
            let name = if suffix == 1 {
                login.clone()
            } else {
                format!("{login}-{suffix}")
            };
            let candidate = riabuild_paths::remote_project_dir(&home, &name, repo);
            let taken = tokio::fs::try_exists(&candidate).await.unwrap_or(false);
            if !taken || self.owned_by_this_namespace(&candidate).await {
                return candidate;
            }
        }
        unreachable!("suffix range 1.. never ends")
    }

    /// Whether a checkout candidate carries this namespace's own `.riabuild-owner`
    /// marker, so a re-run recognises its own tree rather than claiming the next
    /// suffix every time. A missing or unreadable marker is treated as "somebody
    /// else's" — the safe direction, since claiming a directory nobody marked as
    /// ours is exactly the sharing this exists to prevent.
    async fn owned_by_this_namespace(&self, candidate: &std::path::Path) -> bool {
        let marker = candidate.join(".riabuild-owner");
        tokio::fs::read_to_string(&marker)
            .await
            .map(|contents| contents.trim() == self.paths.root().to_string_lossy())
            .unwrap_or(false)
    }

    /// The `gh` riabuild owns.
    ///
    /// Every call site runs *this* rather than the string `"gh"`. Resolving
    /// through `PATH` would find whatever the developer happens to have, which
    /// is not the binary any `check()` verified — and during provisioning
    /// `~/.riabuild/bin` is not on `PATH` at all, so it would usually not find
    /// the owned copy even when one is installed.
    pub fn gh(&self) -> String {
        self.owned_tool(
            "gh",
            riabuild_fetch::tools::GH_VERSION,
            riabuild_fetch::tools::GH_MEMBER,
        )
    }

    /// The `infisical` riabuild owns. Same reasoning as `gh`.
    pub fn infisical(&self) -> String {
        self.owned_tool(
            "infisical",
            riabuild_fetch::tools::INFISICAL_VERSION,
            riabuild_fetch::tools::INFISICAL_MEMBER,
        )
    }

    /// The Claude Code riabuild installed, by absolute path.
    ///
    /// Same reasoning as `gh()`, with one addition: `which("claude")` reads the
    /// ambient `PATH`, which during provisioning does not contain riabuild's
    /// Node — so it finds whatever the developer happens to have installed, or
    /// nothing at all in the moment just after riabuild installed one. Claude
    /// Code is installed by riabuild's own npm, so its home is the pinned
    /// Node's `bin`.
    ///
    /// Falls back to the bare name before a Node is pinned, which is the only
    /// thing a machine with no toolchain yet could use.
    pub fn claude(&self) -> String {
        match &self.config.node_version {
            Some(version) => self
                .paths
                .node_dir(version)
                .join("bin")
                .join("claude")
                .to_string_lossy()
                .into_owned(),
            None => "claude".to_string(),
        }
    }

    fn owned_tool(&self, tool: &str, version: &str, member: &str) -> String {
        self.paths
            .tool_dir(tool, version)
            .join(member)
            .to_string_lossy()
            .into_owned()
    }
}

/// Every task riabuild knows how to perform, in declaration order. The engine
/// sorts by `depends_on`, so this order is for reading, not execution.
pub fn registry() -> Vec<Box<dyn Task>> {
    vec![
        Box::new(login::Login),
        Box::new(github_cli::GithubCli),
        Box::new(infisical_cli::InfisicalCli),
        Box::new(toolchain::Toolchain),
        Box::new(project::Project),
        Box::new(repo_status::RepoStatus),
        Box::new(claude_accounts::ClaudeAccounts),
        Box::new(org_settings::OrgSettings),
        Box::new(claude_trust::ClaudeTrust),
        Box::new(claude_onboarding::ClaudeOnboarding),
        Box::new(claude_agents_view::ClaudeAgentsView),
        Box::new(env_local::EnvLocal),
        Box::new(claude_statusline::ClaudeStatusline),
        Box::new(claude_plugins::ClaudePlugins),
    ]
}

#[cfg(test)]
mod tests {
    use crate::testing::ctx_with;
    use riabuild_runner::FakeRunner;

    #[tokio::test]
    async fn claude_is_the_one_riabuilds_node_installed() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.node_version = Some("22.23.1".into());
        let claude = ctx.claude();
        assert!(claude.ends_with("/node/22.23.1/bin/claude"), "{claude}");
        assert!(claude.starts_with(&ctx.paths.root().to_string_lossy().into_owned()));
    }

    #[tokio::test]
    async fn without_a_pinned_node_the_bare_name_is_all_there_is() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(ctx.claude(), "claude");
    }
}

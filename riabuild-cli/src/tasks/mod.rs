//! The setup tasks and the context they run against.

pub mod claude_profiles;
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
pub mod toolchain;

use crate::api::org::OrgConfig;
use crate::api::{ApiClient, Member};
use crate::config::{State, UserConfig};
use crate::keychain::Keychain;
use crate::paths::Paths;
use crate::runner::CommandRunner;
use crate::ui::Ui;
use anyhow::Result;
use async_trait::async_trait;
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
    pub cli_version: String,
    pub web_url: String,
    /// Environment the shell will be spawned with.
    pub env: Vec<(String, String)>,
    /// Report-only findings, printed after the run. `repo_status` fills this.
    pub notes: Vec<String>,
    /// Set when the developer asked for checks only.
    pub dry_run: bool,
}

impl Ctx {
    pub fn org(&self) -> Result<&OrgConfig> {
        self.org
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("riabuild has not loaded the team configuration yet"))
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// The project directory the developer chose, if one has been chosen.
    pub fn project_dir(&self) -> Option<std::path::PathBuf> {
        self.config
            .project_path
            .as_deref()
            .map(|path| crate::paths::expand_tilde(path, &self.paths.home()))
    }

    /// The `gh` riabuild owns.
    ///
    /// Every call site runs *this* rather than the string `"gh"`. Resolving
    /// through `PATH` would find whatever the developer happens to have, which
    /// is not the binary any `check()` verified — and during provisioning
    /// `~/.riabuild/bin` is not on `PATH` at all, so it would usually not find
    /// the owned copy even when one is installed.
    pub fn gh(&self) -> String {
        self.owned_tool("gh", crate::tools::GH_VERSION, crate::tools::GH_MEMBER)
    }

    /// The `infisical` riabuild owns. Same reasoning as `gh`.
    pub fn infisical(&self) -> String {
        self.owned_tool(
            "infisical",
            crate::tools::INFISICAL_VERSION,
            crate::tools::INFISICAL_MEMBER,
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
    // Task 5 wires this in — remove this once it does. `dead_code` finding a
    // real gap again is the point of not leaving it broader than needed.
    #[allow(dead_code)]
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
        Box::new(claude_profiles::ClaudeProfiles),
        Box::new(org_settings::OrgSettings),
        Box::new(claude_trust::ClaudeTrust),
        Box::new(env_local::EnvLocal),
        Box::new(claude_statusline::ClaudeStatusline),
    ]
}

#[cfg(test)]
mod tests {
    use crate::runner::FakeRunner;
    use crate::testing::ctx_with;

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

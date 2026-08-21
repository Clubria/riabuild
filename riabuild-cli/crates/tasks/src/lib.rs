//! The setup tasks and the context they run against.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct — but the exemption is `test` and nothing else, and must stay that
// way.
//
// It read `any(test, feature = "testing")`, which switched the lint off for
// this crate's *production* code under the one command that enforces it.
// `cargo clippy --workspace --all-targets` resolves dev-dependencies, a
// dev-dependency somewhere in the workspace asks for `testing`, and features
// unify onto the lib target — so the whole crate compiled with the allow on.
// With `test` alone the lib target is linted again, and the unit-test target
// that keeps the allow holds no production code the lib target does not.
//
// Scaffolding behind `feature = "testing"` carries its own allow where it is
// defined, which is a hole the size of a module rather than of a crate.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

pub mod accounts;
pub mod claude_accounts;
pub mod claude_agents_view;
pub mod claude_config;
pub mod claude_onboarding;
pub mod claude_plugins;
pub mod claude_statusline;
pub mod claude_trust;
pub mod codex_cli;
mod ctx;
pub mod engine;
pub mod env_local;
pub mod git_credentials;
pub mod github_cli;
pub mod grok_cli;
pub mod infisical_cli;
pub mod login;
pub mod ngrok;
pub mod org_settings;
pub(crate) mod owned_tool;
pub mod project;
pub mod repo;
pub mod repo_status;
pub mod scope;
pub mod shell;
pub mod shims;
mod task;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod toolchain;

pub use crate::ctx::Ctx;
pub use crate::task::{Reason, Status, Task, TaskId};

/// Every task riabuild knows how to perform, in declaration order. The engine
/// sorts by `depends_on`, so this order is for reading, not execution.
///
/// **Nothing here is load-bearing, and that is a recent thing.** `codex_cli`
/// and `grok_cli` used to be declared ahead of `claude_accounts` with a comment
/// explaining that a failed `apply()` aborted the whole run, so a task declared
/// after the one task that waits on a browser would never run on a machine
/// whose developer walked away from the Claude sign-in. That made the risk a
/// property of *this list* — invisible to the type system, unenforced by any
/// test, and undone by anybody who sorted these lines. `engine::run_all` now
/// carries on past a failed task and skips only its dependents, so the answer
/// to "what must not wait behind what" is `depends_on()` and nothing else.
pub fn registry() -> Vec<Box<dyn Task>> {
    vec![
        Box::new(login::Login),
        Box::new(github_cli::GithubCli),
        Box::new(git_credentials::GitCredentials),
        Box::new(infisical_cli::INFISICAL_CLI),
        Box::new(ngrok::NGROK),
        Box::new(toolchain::Toolchain),
        Box::new(project::Project),
        Box::new(repo_status::RepoStatus),
        Box::new(codex_cli::CodexCli),
        Box::new(grok_cli::GrokCli),
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

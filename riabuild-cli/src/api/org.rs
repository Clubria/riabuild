//! Org configuration: the data the CLI is allowed to receive from the server.
//!
//! Note what is absent — there is no task list, no script, no command. A
//! server-driven manifest would be a remote code execution channel onto every
//! developer's laptop.

use crate::api::ApiClient;
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct OrgConfig {
    #[serde(rename = "repoSlug")]
    pub repo_slug: String,
    #[serde(rename = "defaultProjectPath")]
    pub default_project_path: String,
    #[serde(rename = "minCliVersion")]
    pub min_cli_version: String,
    #[serde(rename = "latestCliVersion")]
    pub latest_cli_version: String,
    #[serde(rename = "secretsUpdatedAt", default)]
    pub secrets_updated_at: u64,
}

impl OrgConfig {
    /// Accepts every spelling of a GitHub remote for the same repository, so a
    /// developer who cloned over SSH is not told their checkout is wrong.
    pub fn matches_remote(&self, remote: &str) -> bool {
        let remote = remote.trim().trim_end_matches(".git");
        let slug = self.repo_slug.to_lowercase();
        let candidates = [
            format!("https://github.com/{slug}"),
            format!("http://github.com/{slug}"),
            format!("git@github.com:{slug}"),
            format!("ssh://git@github.com/{slug}"),
        ];
        candidates
            .iter()
            .any(|candidate| remote.to_lowercase() == *candidate)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeSettings {
    pub settings: serde_json::Value,
    #[serde(rename = "updatedAt", default)]
    pub updated_at: u64,
}

pub async fn fetch_config(api: &ApiClient) -> Result<OrgConfig> {
    api.get_json("/api/v1/org/config").await
}

pub async fn fetch_claude_settings(api: &ApiClient) -> Result<ClaudeSettings> {
    api.get_json("/api/v1/org/claude-settings").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OrgConfig {
        OrgConfig {
            repo_slug: "Clubria/ai-builders-hub".into(),
            default_project_path: "~/code/ai-builders-hub".into(),
            min_cli_version: "0.1.0".into(),
            latest_cli_version: "0.1.0".into(),
            secrets_updated_at: 0,
        }
    }

    #[test]
    fn recognises_every_way_of_writing_the_same_remote() {
        let config = config();
        for remote in [
            "https://github.com/Clubria/ai-builders-hub.git",
            "https://github.com/clubria/ai-builders-hub",
            "git@github.com:Clubria/ai-builders-hub.git",
            "ssh://git@github.com/Clubria/ai-builders-hub.git",
        ] {
            assert!(config.matches_remote(remote), "should accept {remote}");
        }
    }

    #[test]
    fn rejects_a_different_repository() {
        let config = config();
        assert!(!config.matches_remote("git@github.com:Clubria/other-repo.git"));
        assert!(!config.matches_remote("git@gitlab.com:Clubria/ai-builders-hub.git"));
        assert!(!config.matches_remote(""));
    }
}

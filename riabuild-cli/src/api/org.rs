//! Org configuration: the data the CLI is allowed to receive from the server.
//!
//! Note what is absent — there is no task list, no script, no command. A
//! server-driven manifest would be a remote code execution channel onto every
//! developer's laptop.

use crate::api::ApiClient;
use anyhow::Result;
use serde::Deserialize;

/// Note the absence of a checkout path. The server used to send one, but a
/// single string cannot be right on macOS and Linux at once, so where a
/// repository lands is now the CLI's decision — see `paths::default_project_dir`.
/// The endpoint still emits the old field for CLIs released before this change;
/// unknown fields are ignored here, so it costs nothing to keep receiving it.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgConfig {
    #[serde(rename = "repoSlug")]
    pub repo_slug: String,
    #[serde(rename = "minCliVersion", deserialize_with = "version_only")]
    pub min_cli_version: String,
    #[serde(rename = "latestCliVersion", deserialize_with = "version_only")]
    pub latest_cli_version: String,
    #[serde(rename = "secretsUpdatedAt", default)]
    pub secrets_updated_at: u64,
}

/// Refuses anything that is not digits and dots — `^\d+(\.\d+)*$`, the same
/// shape riabuild-web enforces on every write path.
///
/// Mirrors `api::uuid_only`, and for the same reason: the client-side check
/// exists so the CLI survives a server that forgets its own. `latestCliVersion`
/// reaches `download::riabuild_asset_url`, which formats it into
/// `{RELEASES}/v{version}/{asset}`. URL normalisation collapses dot segments,
/// so a value carrying `../` resolves to a *different GitHub repository* —
/// whose `checksums.txt` would then agree with whose binary, leaving the digest
/// check satisfied and an attacker's binary `chmod 755`'d onto the server and
/// executed. That is riabuild-web choosing what code runs, which is exactly the
/// channel "the server ships data, never logic" exists to close.
///
/// `minCliVersion` gets the same treatment: it does not reach a URL today, but
/// it is the same kind of value from the same field of the same reply, and a
/// floor that could be any string at all is a floor `version::` comparisons
/// silently mis-read.
fn version_only<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = String::deserialize(deserializer)?;
    // `split('.')` on "" yields one empty component, so an empty string is
    // refused by the same rule that refuses "1..2" and a leading/trailing dot.
    let shaped = value
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if shaped {
        Ok(value)
    } else {
        Err(D::Error::custom(format!(
            "{value:?} is not a riabuild version"
        )))
    }
}

impl OrgConfig {
    /// The repository half of `owner/repo`, which is what the checkout
    /// directory is named after.
    pub fn repo_name(&self) -> &str {
        self.repo_slug
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.repo_slug)
    }

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
            min_cli_version: "0.1.0".into(),
            latest_cli_version: "0.1.0".into(),
            secrets_updated_at: 0,
        }
    }

    #[test]
    fn the_checkout_is_named_after_the_repository_not_the_owner() {
        assert_eq!(config().repo_name(), "ai-builders-hub");
    }

    #[test]
    fn a_slug_without_an_owner_is_still_usable() {
        let mut config = config();
        config.repo_slug = "ai-builders-hub".into();
        assert_eq!(config.repo_name(), "ai-builders-hub");
        config.repo_slug = "Clubria/".into();
        assert_eq!(config.repo_name(), "Clubria/");
    }

    #[test]
    fn a_retired_checkout_path_from_an_older_server_is_ignored() {
        // The endpoint still sends defaultProjectPath for CLIs released before
        // the client started choosing the location. Receiving it must not fail.
        let config: OrgConfig = serde_json::from_str(
            r#"{"repoSlug":"Clubria/ai-builders-hub","defaultProjectPath":"~/code/ai-builders-hub",
                "minCliVersion":"0.1.0","latestCliVersion":"0.1.0","secretsUpdatedAt":0}"#,
        )
        .expect("an unknown field must not break the config");
        assert_eq!(config.repo_name(), "ai-builders-hub");
    }

    /// A payload with `latestCliVersion`/`minCliVersion` set to `value`.
    fn payload(field: &str, value: &str) -> String {
        let mut latest = "0.1.0";
        let mut min = "0.1.0";
        if field == "latestCliVersion" {
            latest = value;
        } else {
            min = value;
        }
        format!(
            r#"{{"repoSlug":"Clubria/ai-builders-hub","minCliVersion":"{min}",
                "latestCliVersion":"{latest}","secretsUpdatedAt":0}}"#
        )
    }

    #[test]
    fn an_ordinary_version_still_decodes() {
        let config: OrgConfig =
            serde_json::from_str(&payload("latestCliVersion", "2026.08.06")).expect("decodes");
        assert_eq!(config.latest_cli_version, "2026.08.06");
        let config: OrgConfig =
            serde_json::from_str(&payload("minCliVersion", "9999.0.0")).expect("decodes");
        assert_eq!(config.min_cli_version, "9999.0.0");
    }

    #[test]
    fn a_version_that_could_escape_the_pinned_repository_is_refused() {
        // The one that matters: `latest_cli_version` is formatted into
        // `{RELEASES}/v{version}/{asset}`, and URL normalisation collapses
        // dot segments — so this value resolves to a different GitHub
        // repository, whose `checksums.txt` matches its own binary, and the
        // digest check passes on an attacker's build that is then chmod
        // 755'd onto a server and run. Every riabuild-web write path already
        // refuses it; this is the CLI surviving a server that forgets to.
        for bad in [
            "",
            "x/../../../../../attacker/repo/releases/download/v1",
            "../1.0",
            "1.0/..",
            "1.0.0-rc1",
            "latest",
            " 1.0.0",
            "1.0.0 ",
            "1..0",
            ".1.0",
            "1.0.",
        ] {
            for field in ["latestCliVersion", "minCliVersion"] {
                let parsed = serde_json::from_str::<OrgConfig>(&payload(field, bad));
                let error = parsed
                    .err()
                    .unwrap_or_else(|| panic!("{field}={bad:?} must not be accepted"));
                assert!(
                    error.to_string().contains("is not a riabuild version"),
                    "{field}={bad:?} produced {error}"
                );
            }
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

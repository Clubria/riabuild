//! Brokered Infisical credentials.
//!
//! The token returned here is short-lived and is never written down. It is piped
//! into `infisical export` through stdin — not passed as an argument, where `ps`
//! would show it to every process on the machine.

use crate::ApiClient;
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct BrokeredToken {
    pub token: String,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: u64,
    #[serde(rename = "projectId")]
    pub project_id: String,
    pub environment: String,
    #[serde(rename = "secretPath", default = "root_path")]
    pub secret_path: String,
    #[serde(rename = "siteUrl", default)]
    pub site_url: String,
    #[serde(rename = "secretsUpdatedAt", default)]
    pub secrets_updated_at: u64,
}

fn root_path() -> String {
    "/".to_string()
}

pub async fn broker(api: &ApiClient) -> Result<BrokeredToken> {
    api.post_json("/api/v1/secrets/token", serde_json::json!({}))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_defaults_to_the_root_secret_path() {
        let brokered: BrokeredToken = serde_json::from_str(
            r#"{"token":"inf_x","projectId":"p1","environment":"dev","expiresAt":1}"#,
        )
        .unwrap();
        assert_eq!(brokered.secret_path, "/");
        assert_eq!(brokered.environment, "dev");
    }
}

//! Brokered Infisical credentials.
//!
//! The token returned here is short-lived and is never written down. It reaches
//! `infisical` in that one process's **environment** — not as an argument, where
//! `ps` would show it to every account on the machine.
//!
//! Two callers ask for one, and they are the same request from either side:
//! `tasks::env_local`, which writes `.env.<environment>` on every run, and
//! `internal::infisical`, which is what `~/.riabuild/bin/infisical` runs when a
//! developer types the command themselves. Neither leaves anything behind.

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
    /// The base environment alone. Kept because it is all a deployment
    /// released before `environments` sends; `environments` is what riabuild
    /// pulls from.
    pub environment: String,
    /// Every environment this credential was minted to reach, in the order the
    /// CLI writes them. Empty means the deployment predates the field — see
    /// the test beside this type for why that is not defaulted to anything.
    #[serde(default)]
    pub environments: Vec<String>,
    /// The primary folder alone — what a bare `infisical` command through the
    /// shim defaults to. It is the last of `secret_paths`, so a deployment
    /// released before that field is still answering the same question.
    #[serde(rename = "secretPath", default = "root_path")]
    pub secret_path: String,
    /// Every folder this credential was minted to export, in the order they
    /// are exported and therefore merged: **later wins**, exactly as a dotenv
    /// loader reads the finished file. Empty means the deployment predates the
    /// field, which `export_paths` answers with the primary alone.
    #[serde(rename = "secretPaths", default)]
    pub secret_paths: Vec<String>,
    #[serde(rename = "siteUrl", default)]
    pub site_url: String,
    #[serde(rename = "secretsUpdatedAt", default)]
    pub secrets_updated_at: u64,
}

fn root_path() -> String {
    "/".to_string()
}

impl BrokeredToken {
    /// The folders to export, in order, for one environment.
    ///
    /// Unlike `environments` — which is empty on an old deployment and is left
    /// empty, because guessing which environments a laptop may pull would be
    /// this CLI inventing an authorization answer — an absent `secret_paths`
    /// has a correct answer already in hand: the single folder that deployment
    /// has always named. So this falls back rather than failing, and a
    /// deployment nobody has updated keeps working exactly as it did.
    pub fn export_paths(&self) -> Vec<String> {
        let named: Vec<String> = if self.secret_paths.is_empty() {
            vec![self.secret_path.clone()]
        } else {
            self.secret_paths.clone()
        };
        // An empty string is not a folder, and `--path=` is not the root: it
        // is an argument infisical answers nothing for. A deployment that
        // sends one is misconfigured, and the root is what riabuild brokered
        // before either field existed.
        let named: Vec<String> = named.into_iter().filter(|path| !path.is_empty()).collect();
        if named.is_empty() {
            return vec![root_path()];
        }
        named
    }
}

/// Whether an environment name is safe to turn into a filename and an argument.
///
/// The names arrive from riabuild-web, which reads them from deployment
/// environment variables and validates nothing — so the check has to exist
/// here. A name becomes two things on a laptop: `--env=<name>` in an argument
/// list, and the tail of `.env.<name>` joined onto the developer's checkout.
/// The second is what makes this a security boundary rather than tidiness: a
/// name carrying `/` or `..` writes outside the checkout, and `.` as a leading
/// character writes a dotfile nobody will notice. This is the same reasoning
/// `org::version_only` applies to a version that reaches a download URL.
///
/// The check lives at the point of use rather than in the deserializer, so a
/// misconfigured deployment fails the one task that pulls secrets instead of
/// making `/api/v1/org/config` unparseable — which would take `riabuild
/// remote` and `riabuild remote forget` down with it.
pub fn is_safe_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
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

    #[test]
    fn a_deployment_that_names_no_folders_still_exports_the_one_it_named() {
        // Unlike `environments`, an absent `secretPaths` has a correct answer
        // already in hand — the folder that deployment has always sent — so
        // this falls back rather than failing. A deployment nobody has
        // updated goes on pulling exactly what it pulled before.
        let brokered: BrokeredToken = serde_json::from_str(
            r#"{"token":"inf_x","projectId":"p1","environment":"dev","secretPath":"/dev-env"}"#,
        )
        .unwrap();
        assert!(brokered.secret_paths.is_empty());
        assert_eq!(brokered.export_paths(), ["/dev-env"]);
    }

    #[test]
    fn every_folder_the_deployment_names_is_exported_in_order() {
        // Order is the contract: the credential folder is last, so a key both
        // folders hold takes its value, and `secretPath` — the shim's default
        // and all an older CLI reads — is that same last folder.
        let brokered: BrokeredToken = serde_json::from_str(
            r#"{"token":"inf_x","projectId":"p1","environment":"dev",
                "secretPath":"/tenant/aibuilders/convex",
                "secretPaths":["/tenant/aibuilders/frontend","/tenant/aibuilders/convex"]}"#,
        )
        .unwrap();
        assert_eq!(
            brokered.export_paths(),
            ["/tenant/aibuilders/frontend", "/tenant/aibuilders/convex"]
        );
        assert_eq!(brokered.secret_path, "/tenant/aibuilders/convex");
    }

    #[test]
    fn an_empty_folder_name_is_never_passed_as_a_path() {
        // `--path=` is not the root, it is an argument infisical answers
        // nothing for. A deployment that sends one is misconfigured, and the
        // root is what riabuild brokered before either field existed.
        let brokered: BrokeredToken = serde_json::from_str(
            r#"{"token":"inf_x","projectId":"p1","environment":"dev",
                "secretPath":"","secretPaths":["","/apps",""]}"#,
        )
        .unwrap();
        assert_eq!(brokered.export_paths(), ["/apps"]);

        let empty: BrokeredToken = serde_json::from_str(
            r#"{"token":"inf_x","projectId":"p1","environment":"dev","secretPath":""}"#,
        )
        .unwrap();
        assert_eq!(empty.export_paths(), ["/"]);
    }

    #[test]
    fn a_deployment_that_names_no_environments_is_not_quietly_given_one() {
        // The field is absent from every deployment released before it existed.
        // Defaulting it to `["dev"]` here would be this CLI guessing what that
        // deployment's INFISICAL_ENVIRONMENT says — so it stays empty, and the
        // task that needs it says out loud that the deployment is behind.
        let brokered: BrokeredToken = serde_json::from_str(
            r#"{"token":"inf_x","projectId":"p1","environment":"dev","expiresAt":1}"#,
        )
        .unwrap();
        assert!(brokered.environments.is_empty());
    }

    #[test]
    fn the_environment_list_is_read_when_the_deployment_sends_one() {
        let brokered: BrokeredToken = serde_json::from_str(
            r#"{"token":"inf_x","projectId":"p1","environment":"dev",
                "environments":["dev","staging"],"expiresAt":1}"#,
        )
        .unwrap();
        assert_eq!(brokered.environments, ["dev", "staging"]);
    }

    #[test]
    fn an_ordinary_environment_name_is_accepted() {
        for name in ["dev", "staging", "prod-eu", "qa_2", "v1.2"] {
            assert!(is_safe_environment_name(name), "{name}");
        }
    }

    #[test]
    fn a_name_that_would_escape_the_checkout_is_refused() {
        // `.env.<name>` is joined onto the project directory, so a name
        // carrying a separator or a dot segment writes somewhere the developer
        // did not agree to — `.env.../../.bashrc` resolves out of the checkout
        // entirely. This is the same reasoning `org::version_only` uses for a
        // version that reaches a download URL.
        for name in [
            "../../.bashrc",
            "..",
            ".",
            "a/b",
            "a\\b",
            "",
            ".hidden",
            "with space",
            "semi;colon",
            "new\nline",
        ] {
            assert!(!is_safe_environment_name(name), "{name:?} was accepted");
        }
    }

    #[test]
    fn a_name_long_enough_to_be_a_payload_is_refused() {
        assert!(!is_safe_environment_name(&"a".repeat(65)));
        assert!(is_safe_environment_name(&"a".repeat(64)));
    }
}

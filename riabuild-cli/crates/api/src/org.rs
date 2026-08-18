//! Org configuration: the data the CLI is allowed to receive from the server.
//!
//! Note what is absent — there is no task list, no script, no command. A
//! server-driven manifest would be a remote code execution channel onto every
//! developer's laptop.

use crate::ApiClient;
use crate::repo::Repo;
use anyhow::{Context, Result};
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
    /// The Infisical environments this developer may pull, which is also the
    /// set of `.env.<name>` files their checkout is expected to have.
    ///
    /// It is served here as well as by `/api/v1/secrets/token` because
    /// `env_local::check()` runs on every `riabuild --check` and must not
    /// broker a credential to learn what it is looking for: brokering reaches
    /// Infisical and writes an audit row. Empty means the deployment predates
    /// the field, which the task reports rather than guesses around.
    #[serde(rename = "secretEnvironments", default)]
    pub secret_environments: Vec<String>,
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
///
/// Surrounding whitespace is **trimmed rather than refused**, because
/// `org.update` on the server tests its regex against `value.trim()` and then
/// stores `args.minCliVersion` untrimmed — so `" 1.0.0"` is a value riabuild-web
/// accepts and serves. Refusing it here would not protect anything: `fetch_config`
/// is reached through `main::connect` with `?`, so a config the CLI cannot
/// deserialize fails `provision`, `riabuild remote` and `riabuild remote forget`
/// alike — every developer blocked by a stray space, rather than a version floor
/// quietly not applying. What is trimmed is then *gone*: the trimmed value is
/// what is stored, so no whitespace reaches the release URL either way.
fn version_only<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let raw = String::deserialize(deserializer)?;
    // ECMAScript's `String.prototype.trim` — the one the server's own check
    // runs — strips U+FEFF as whitespace and Rust's `str::trim` does not, so
    // the one character the two disagree about is named here. Anything else
    // Rust trims and JS does not is a value the server's regex already refused
    // to store.
    let value = raw
        .trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .to_string();
    // `split('.')` on "" yields one empty component, so an empty string is
    // refused by the same rule that refuses "1..2" and a leading/trailing dot.
    let shaped = value
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if shaped {
        Ok(value)
    } else {
        Err(D::Error::custom(format!(
            "{raw:?} is not a riabuild version"
        )))
    }
}

impl OrgConfig {
    /// The repository Enter takes at the picker, and the owner a bare name
    /// typed there is completed with.
    ///
    /// The *default*, not the only one — which is the whole of what changed when
    /// `riabuild` began asking. Everything that clones, names a directory, or
    /// checks a remote reads the repository this run is about, from
    /// `Ctx::repo`, and reaches this only to find out what Enter means.
    ///
    /// Fallible, and deliberately not checked at deserialize time the way
    /// `version_only` checks the fields beside it. `fetch_config` is reached
    /// through `main::connect` with `?`, so refusing there would stop `status`,
    /// `logout`, `remote forget` and every other command on a value only the
    /// provisioning flow needs. A lead who types a malformed slug into the
    /// dashboard should break the run that has to clone something and nothing
    /// else.
    pub fn default_repo(&self) -> Result<Repo> {
        Repo::parse(&self.repo_slug).with_context(|| {
            format!(
                "the default repository in the riabuild dashboard, {:?}, is not usable",
                self.repo_slug
            )
        })
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
            secret_environments: vec!["dev".into()],
        }
    }

    #[test]
    fn a_config_from_a_deployment_without_the_field_parses() {
        // /api/v1 is add-only in both directions: a CLI carrying this field
        // still has to read a deployment that has not been updated yet.
        let config: OrgConfig = serde_json::from_str(
            r#"{"repoSlug":"Clubria/ai-builders-hub","minCliVersion":"1.0.0",
                "latestCliVersion":"1.0.0"}"#,
        )
        .unwrap();
        assert!(config.secret_environments.is_empty());
    }

    #[test]
    fn the_environment_list_is_read_when_the_deployment_sends_one() {
        let config: OrgConfig = serde_json::from_str(
            r#"{"repoSlug":"Clubria/ai-builders-hub","minCliVersion":"1.0.0",
                "latestCliVersion":"1.0.0","secretEnvironments":["dev","staging"]}"#,
        )
        .unwrap();
        assert_eq!(config.secret_environments, ["dev", "staging"]);
    }

    #[test]
    fn the_default_repository_is_the_slug_the_dashboard_holds() {
        let repo = config().default_repo().expect("parses");
        assert_eq!(repo.slug(), "Clubria/ai-builders-hub");
        // What a bare name typed at the picker is completed with.
        assert_eq!(repo.owner(), "Clubria");
        // What the checkout directory is named after.
        assert_eq!(repo.name(), "ai-builders-hub");
    }

    #[test]
    fn a_default_repository_nobody_could_clone_says_where_it_came_from() {
        // `repoSlug` is the one field `org.update` stores without checking, so
        // this is reachable by a lead's typo — and the message has to send them
        // to the dashboard rather than read as a bug in their machine.
        let mut config = config();
        config.repo_slug = "ai-builders-hub".into();
        let error = config.default_repo().expect_err("names no owner");
        let message = format!("{error:#}");
        assert!(
            message.contains("riabuild dashboard") && message.contains("names no owner"),
            "unhelpful message: {message}"
        );
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
        assert_eq!(config.repo_slug, "Clubria/ai-builders-hub");
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
            "1. 0.0",
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
    fn surrounding_whitespace_the_server_stores_is_trimmed_rather_than_refused() {
        // `org.update` validates `value.trim()` against its regex and then
        // stores `args.minCliVersion` untrimmed, so these are values
        // riabuild-web accepts and serves. Refusing them here blocks every
        // developer, not just the version floor: `fetch_config` is called from
        // `main::connect` with `?`, so a config the CLI cannot deserialize
        // fails `provision`, `riabuild remote` and `riabuild remote forget`
        // alike.
        // Written as the JSON escapes the server would send, not as Rust
        // escapes: a raw tab inside a JSON string is a parse error long
        // before `version_only` sees it.
        for (raw, expected) in [
            (" 1.0.0", "1.0.0"),
            ("1.0.0 ", "1.0.0"),
            (r"\t2026.08.06\n", "2026.08.06"),
            ("\u{feff}1.0.0", "1.0.0"),
        ] {
            let config: OrgConfig = serde_json::from_str(&payload("latestCliVersion", raw))
                .unwrap_or_else(|error| panic!("{raw:?} must decode: {error}"));
            // Trimmed, not merely accepted: this value is formatted straight
            // into a release URL.
            assert_eq!(config.latest_cli_version, expected);
        }
    }

    #[test]
    fn a_version_field_that_is_absent_or_null_is_refused() {
        // Unpinned until now, and the safe behaviour is the current one. A
        // later `#[serde(default)]` — the obvious tidy-up — would turn a
        // missing `minCliVersion` into `""` *without* passing it through
        // `version_only` at all, so the floor would silently become a string
        // no `version::` comparison can read, and nothing else in the suite
        // would notice.
        for body in [
            // absent
            r#"{"repoSlug":"Clubria/x","latestCliVersion":"1.0.0","secretsUpdatedAt":0}"#,
            r#"{"repoSlug":"Clubria/x","minCliVersion":"1.0.0","secretsUpdatedAt":0}"#,
            // null
            r#"{"repoSlug":"Clubria/x","minCliVersion":null,"latestCliVersion":"1.0.0"}"#,
            r#"{"repoSlug":"Clubria/x","minCliVersion":"1.0.0","latestCliVersion":null}"#,
        ] {
            assert!(
                serde_json::from_str::<OrgConfig>(body).is_err(),
                "must not decode: {body}"
            );
        }
    }

    // Remote spellings moved with `matches_remote` onto `Repo`, and are
    // covered by `repo::tests::every_spelling_of_the_same_remote_matches`.
}

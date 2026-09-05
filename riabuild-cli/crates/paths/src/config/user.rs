//! `~/.riabuild/config.json` — the developer's own settings.
//!
//! The other half of this directory, and the half that is *not* a cache. It
//! holds the checkout of every repository this machine has cloned, both pinned
//! tool versions and the ordered Claude account list, so a file that will not
//! parse is set aside rather than overwritten — see [`UserConfig::load`], which
//! is where that argument is made.

use super::{now_secs, write_json};
use crate::Paths;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    /// The checkout of each repository this machine has cloned, keyed by
    /// `owner/repo`, absolute once chosen.
    ///
    /// A map rather than a path because `riabuild` asks which repository to work
    /// on, and a developer who answers with one they have used before must not
    /// be re-cloned or re-asked: the tree, its branches, its uncommitted work
    /// and its `.env` files are all still there. Keys are slugs an `api::Repo`
    /// produced — this crate stores them and never parses them, which is why it
    /// holds strings and not the newtype.
    #[serde(default)]
    pub repos: BTreeMap<String, String>,
    /// Which of `repos` this machine is working on, as `owner/repo`.
    ///
    /// `None` means the picker has not run here yet, which is what
    /// `adopt_legacy_checkout` tests before it migrates anything.
    #[serde(default)]
    pub active_repo: Option<String>,
    /// The repository this machine answered "always" for, as `owner/repo`.
    ///
    /// `active_repo` is what the picker's Enter *offers*; this is a developer
    /// saying they do not want to be offered anything. A run that finds it set
    /// puts no question at all — which is why the one thing that clears it is
    /// GitHub saying the repository is not there any more. A pin nobody can
    /// reach is a machine that provisions the wrong checkout in silence, and
    /// silence is the whole feature the rest of the time.
    ///
    /// Recorded here rather than beside the checkout in `repos` because it is a
    /// fact about this *machine* — which of several repositories it works on
    /// without being asked — and not about any one of them.
    #[serde(default)]
    pub always_repo: Option<String>,
    /// The single checkout riabuild recorded before it asked which repository.
    ///
    /// Migrated into `repos` by `adopt_legacy_checkout`, which the picker calls —
    /// the first place both this path and the org's default repository are known.
    /// It cannot be folded in `load` the way `claude_profile` is, because folding
    /// it means knowing *which* repository the path is a checkout of, and that
    /// arrives from `/api/v1/org/config` long after this file is read.
    ///
    /// Still serialised until then, deliberately. `skip_serializing` would mean
    /// any run that rewrites `config.json` for an unrelated reason — `riabuild
    /// claude add` is one — drops a checkout nothing has folded yet, and the next
    /// provisioning run clones a second copy of the repository the developer
    /// already has.
    #[serde(default)]
    pub project_path: Option<String>,
    /// Pinned by `toolchain` so every later run agrees on which Node is ours.
    #[serde(default)]
    pub node_version: Option<String>,
    #[serde(default)]
    pub pnpm_version: Option<String>,
    /// Claude Code config directories, in the order the developer numbers them.
    ///
    /// Position *is* the number: account 3 is index 2, and removing it makes
    /// what was account 4 into account 3 with no renumbering code at all. The
    /// UUID is the only identity anything persists.
    #[serde(default)]
    pub claude_accounts: Vec<String>,
    /// Which Claude accounts report usage to riabuild-web, by UUID.
    ///
    /// A separate list rather than a flag inside `claude_accounts`, because
    /// position in that vector *is* the account number and nothing may disturb
    /// it — and because the identity persisted here has to survive `riabuild
    /// claude delete` renumbering the account it belongs to.
    ///
    /// **Empty by default, and that is the feature.** A developer's accounts
    /// include personal subscriptions; see `2026-08-06-claude-accounts-design.md`.
    /// Collecting from an account nobody marked would ship a person's private
    /// usage to their employer's dashboard, and the developer it would happen to
    /// is exactly the one who did not read the release note. `riabuild claude
    /// track <n>` adds one; the account list shows which are in it.
    #[serde(default)]
    pub tracked_accounts: Vec<String>,
    /// The single profile older riabuilds recorded.
    ///
    /// Read on load and folded into `claude_accounts`, never written back —
    /// which is what `skip_serializing` is for. Keeping it means a developer
    /// who upgrades does not lose the account they are already signed in to.
    #[serde(default, skip_serializing)]
    pub claude_profile: Option<String>,
    /// When this machine's riabuild session runs out, so `login` can refresh
    /// before a developer is interrupted. Not a secret — the token itself lives
    /// in the keychain.
    #[serde(default)]
    pub session_expires_at: Option<u64>,
    /// The `updatedAt` of the org Claude settings currently cached on disk.
    #[serde(default)]
    pub org_settings_updated_at: Option<u64>,
}

impl UserConfig {
    /// Reads `config.json`, and keeps a copy of one it cannot parse.
    ///
    /// The `Default` an unparseable file falls back to is not an empty config,
    /// it is an amnesiac one: every checkout, both pinned versions and the
    /// ordered Claude account list are gone, and because `update` reads under
    /// the lock and writes the result back, the very next write makes that
    /// forgetting permanent. The accounts are the expensive half — their
    /// directories stay on disk with nothing naming them, and since position
    /// *is* the account number, adopting one of those orphans later changes
    /// which account `claude-2` opens.
    ///
    /// [`write_atomic`](crate::config::write_atomic) closes the torn-write
    /// case, which is the only one
    /// riabuild itself causes. It does not close a hand edit, a half-synced
    /// cloud folder, or a field written by a riabuild newer than this one — so
    /// the file is moved aside under a name that says what it is and the
    /// developer is told where it went. Recovering from that is a text editor;
    /// recovering from the overwrite was re-onboarding.
    ///
    /// [`State::load`](crate::config::State::load) deliberately does none of
    /// this. State is a cache of
    /// decisions, and losing it costs one redundant `check()`.
    pub async fn load(paths: &dyn Paths) -> Self {
        let path = paths.config_file();
        let mut config = match tokio::fs::read_to_string(&path).await {
            Ok(text) => match serde_json::from_str::<UserConfig>(&text) {
                Ok(config) => config,
                Err(error) => {
                    keep_unreadable(&path, &error.to_string()).await;
                    UserConfig::default()
                }
            },
            // Nothing there is the ordinary first run. An unreadable file — a
            // mode nobody can read, a directory in its place — is not this
            // function's to report either: the write that follows fails on it
            // and says so with the errno, which is more than we know here.
            Err(_) => UserConfig::default(),
        };
        config.fold_legacy_profile();
        config
    }

    /// Acquires the lock, reads what is on disk *now*, applies `mutate`, and
    /// writes the result atomically. See `State::update` for why the read is
    /// inside the lock, and why there is no `save`.
    ///
    /// This one matters more than `State`'s. State is a cache, and a lost record
    /// costs one redundant `check()`. `config.json` is where the checkout path,
    /// the pinned versions and the ordered account list live — a lost update
    /// there drops a Claude account from the registry while its directory stays
    /// on disk, and because position *is* the account number, adopting that
    /// orphan later changes which account `claude-2` opens.
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self> {
        let _lock = crate::filelock::FileLock::acquire(&paths.state_lock_file(), || {}).await?;
        let mut config = Self::load(paths).await;
        mutate(&mut config);
        write_json(&paths.config_file(), &config).await?;
        Ok(config)
    }

    /// Folds the single profile of an older riabuild into the account list.
    ///
    /// Takes the field rather than copying it, so no caller can read a value
    /// that will not be saved.
    /// The checkout of one repository, if this machine has one.
    ///
    /// Keyed by the repository asked about rather than by `active_repo`: which
    /// repository a *run* is about is the run's to know — `--repo` and the picker
    /// both set it before any task looks at a checkout — and `active_repo` is
    /// only how that is remembered for the next run's default.
    pub fn checkout_of(&self, slug: &str) -> Option<&str> {
        self.repos.get(slug).map(String::as_str)
    }

    /// The one checkout riabuild recorded before it asked which repository.
    ///
    /// Only `Ctx::project_dir` reads it, and only for the org default, because
    /// that is the only repository this path can be a checkout of. Kept
    /// separate from `checkout_of` so no caller can answer a question about
    /// `Clubria/payments` with a path that is a checkout of something else.
    pub fn legacy_checkout(&self) -> Option<&str> {
        self.project_path.as_deref()
    }

    /// Records where a repository's checkout is.
    pub fn set_checkout(&mut self, slug: &str, path: impl Into<String>) {
        self.repos.insert(slug.to_string(), path.into());
    }

    /// Folds the pre-picker checkout into `repos` under `slug`, the org's default
    /// repository — the only repository riabuild could have cloned before it
    /// asked.
    ///
    /// Taken unconditionally, and never over an entry the map already has: the
    /// field is cleared in the same write, so the only way it can reappear is an
    /// older riabuild writing it again, and folding that under the default is
    /// still the right reading of it.
    ///
    /// Says nothing about which repository is *active*. Both callers decide that
    /// for themselves, and a fold that also claimed it would have to refuse to
    /// run once they had — which is how a path gets orphaned in a file nothing
    /// reads any more.
    pub fn adopt_legacy_checkout(&mut self, slug: &str) {
        if let Some(path) = self.project_path.take() {
            self.repos.entry(slug.to_string()).or_insert(path);
        }
    }

    fn fold_legacy_profile(&mut self) {
        // Taken unconditionally: a value that will not be saved must not be
        // readable either. `extend` over the Option keeps this one statement
        // rather than a nested `if`, which `clippy::collapsible_if` rejects.
        let legacy = self.claude_profile.take();
        if self.claude_accounts.is_empty() {
            self.claude_accounts.extend(legacy);
        }
    }
}

/// Moves a `config.json` that will not parse aside, and says where it went.
///
/// Infallible on purpose: this is the recovery path of a read that has already
/// decided to carry on, so there is no caller to return an error to. Both
/// outcomes are worth a line on the developer's terminal, and the second one
/// especially — a file that could not even be renamed is one the next write
/// will land on top of.
///
/// The `Ui` is built here rather than passed in. `load` is on the read path of
/// every command riabuild has, and threading an output channel through all of
/// them to serve a branch that runs once in the life of a machine would put the
/// cost in the wrong place; `remote::channel` builds one for the same reason.
/// Nothing but the corrupt branch reaches this, so no ordinary run pays for it.
async fn keep_unreadable(path: &Path, why: &str) {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.json".to_string());
    let aside = path.with_file_name(format!("{name}.broken-{}", now_secs()));
    let ui = riabuild_ui::Ui::new(false);

    match tokio::fs::rename(path, &aside).await {
        Ok(()) => ui.warn(&format!(
            "{} could not be read ({why}). It has been kept at {}, and riabuild is carrying on with a fresh one — so the checkouts, pinned versions and Claude accounts it named are recoverable from that copy rather than lost.",
            path.display(),
            aside.display()
        )),
        Err(error) => ui.warn(&format!(
            "{} could not be read ({why}), and could not be set aside either ({error}). riabuild is carrying on with a fresh one and the next write will replace it — copy it somewhere else now if you want what it named back.",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RealPaths;
    use tempfile::TempDir;

    /// Two `riabuild claude new` runs in two terminal windows.
    ///
    /// Before the lock, each run loaded `config.json` at startup and wrote its
    /// whole snapshot back later, so the later writer won with a list that
    /// never contained the earlier writer's account. The UUID vanished from the
    /// registry while its directory stayed on disk — and because position *is*
    /// the account number, adopting that orphan on a later run changes which
    /// account `claude-2` opens.
    #[tokio::test]
    async fn concurrent_account_additions_do_not_lose_an_account() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("mkdir");

        let mut writers = Vec::new();
        for n in 0..8 {
            let paths = RealPaths::rooted_at(home.path());
            writers.push(tokio::spawn(async move {
                UserConfig::update(&paths, |config| {
                    config.claude_accounts.push(format!("account-{n}"));
                })
                .await
                .expect("update");
            }));
        }
        for writer in writers {
            writer.await.expect("join");
        }

        let mut found = UserConfig::load(&paths).await.claude_accounts;
        found.sort();
        let expected: Vec<String> = (0..8).map(|n| format!("account-{n}")).collect();
        assert_eq!(found, expected, "an account was lost between two windows");
    }

    #[tokio::test]
    async fn an_update_returns_exactly_what_it_wrote() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());

        let written = UserConfig::update(&paths, |config| {
            config.project_path = Some("/srv/checkout".into());
        })
        .await
        .expect("update");

        assert_eq!(written.project_path.as_deref(), Some("/srv/checkout"));
        assert_eq!(
            UserConfig::load(&paths).await.project_path.as_deref(),
            Some("/srv/checkout"),
            "what was handed back must be what landed on disk"
        );
    }

    #[tokio::test]
    async fn round_trips_user_config() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        UserConfig::update(&paths, |config| {
            config.project_path = Some("/Users/ada/code/hub".into());
            config.node_version = Some("22.23.1".into());
        })
        .await
        .unwrap();

        let loaded = UserConfig::load(&paths).await;
        assert_eq!(loaded.project_path.as_deref(), Some("/Users/ada/code/hub"));
        assert_eq!(loaded.node_version.as_deref(), Some("22.23.1"));
        assert_eq!(loaded.claude_profile, None);
    }

    #[test]
    fn each_repository_keeps_its_own_checkout() {
        let mut config = UserConfig::default();
        config.set_checkout("Clubria/ai-builders-hub", "/Users/ada/code/ai-builders-hub");
        config.set_checkout("Clubria/payments", "/Users/ada/code/payments");

        // Switching is a pointer move: both trees, their branches and their
        // .env files stay recorded.
        assert_eq!(
            config.checkout_of("Clubria/payments"),
            Some("/Users/ada/code/payments")
        );
        assert_eq!(
            config.checkout_of("Clubria/ai-builders-hub"),
            Some("/Users/ada/code/ai-builders-hub")
        );
    }

    #[test]
    fn a_repository_with_no_checkout_yet_has_none() {
        // What the `project` task reads as "no project directory chosen yet",
        // which is exactly right for a repository picked but never cloned.
        let mut config = UserConfig::default();
        config.set_checkout("Clubria/ai-builders-hub", "/Users/ada/code/hub");
        assert_eq!(config.checkout_of("Clubria/payments"), None);
    }

    #[test]
    fn the_checkout_an_older_riabuild_recorded_is_kept_apart_from_the_map() {
        // `riabuild status` and `riabuild --check` never put the picker's
        // question, so they read this file with the migration still pending —
        // and a path that is a checkout of the org default must not be handed
        // back as a checkout of anything else.
        let config = UserConfig {
            project_path: Some("/Users/ada/code/hub".into()),
            ..UserConfig::default()
        };
        assert_eq!(config.legacy_checkout(), Some("/Users/ada/code/hub"));
        assert_eq!(config.checkout_of("Clubria/ai-builders-hub"), None);
    }

    #[test]
    fn the_legacy_checkout_is_adopted_by_the_org_default_and_then_forgotten() {
        let mut config = UserConfig {
            project_path: Some("/Users/ada/code/hub".into()),
            ..UserConfig::default()
        };

        config.adopt_legacy_checkout("Clubria/ai-builders-hub");

        assert_eq!(
            config.checkout_of("Clubria/ai-builders-hub"),
            Some("/Users/ada/code/hub"),
            "the developer's existing tree must not be re-cloned"
        );
        assert_eq!(
            config.project_path, None,
            "the migrated field must be cleared in the same write"
        );
    }

    #[test]
    fn a_fold_never_writes_over_a_checkout_the_map_already_has() {
        // Reachable by an older riabuild writing the field again after a
        // migration. The map is the truth, and the path in it is the one the
        // last run actually cloned or moved.
        let mut config = UserConfig::default();
        config.set_checkout("Clubria/ai-builders-hub", "/Users/ada/code/hub");
        config.project_path = Some("/somewhere/stale".into());

        config.adopt_legacy_checkout("Clubria/ai-builders-hub");

        assert_eq!(
            config.checkout_of("Clubria/ai-builders-hub"),
            Some("/Users/ada/code/hub")
        );
        assert_eq!(config.project_path, None, "and the stale field is gone");
    }

    #[test]
    fn a_fresh_machine_adopts_nothing() {
        let mut config = UserConfig::default();
        config.adopt_legacy_checkout("Clubria/ai-builders-hub");
        assert!(config.repos.is_empty(), "there was no checkout to adopt");
    }

    #[tokio::test]
    async fn the_repository_map_round_trips() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());

        UserConfig::update(&paths, |config| {
            config.set_checkout("Clubria/payments", "/Users/ada/code/payments");
            config.active_repo = Some("Clubria/payments".into());
        })
        .await
        .unwrap();

        let loaded = UserConfig::load(&paths).await;
        assert_eq!(
            loaded.checkout_of("Clubria/payments"),
            Some("/Users/ada/code/payments")
        );
    }

    #[tokio::test]
    async fn a_config_written_before_the_picker_existed_still_loads() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(
            paths.config_file(),
            br#"{"project_path":"/Users/ada/code/hub","node_version":"22.23.1"}"#,
        )
        .await
        .unwrap();

        let loaded = UserConfig::load(&paths).await;
        assert_eq!(loaded.legacy_checkout(), Some("/Users/ada/code/hub"));
        assert!(loaded.repos.is_empty());
        assert_eq!(loaded.active_repo, None);
    }

    #[tokio::test]
    async fn a_legacy_profile_becomes_the_first_account() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(
            paths.config_file(),
            r#"{"claude_profile":"11111111-2222-4333-8444-555555555555"}"#,
        )
        .await
        .unwrap();

        let config = UserConfig::load(&paths).await;
        assert_eq!(
            config.claude_accounts,
            vec!["11111111-2222-4333-8444-555555555555".to_string()]
        );
        // Folded in on load, so nothing downstream ever sees the old field.
        assert_eq!(config.claude_profile, None);
        // The folded profile is the *primary* account, which is what position 1
        // means — read off the list, because that list is the only record of it.
        assert_eq!(
            config.claude_accounts.first().map(String::as_str),
            Some("11111111-2222-4333-8444-555555555555")
        );
    }

    #[tokio::test]
    async fn an_account_list_wins_over_a_legacy_profile() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(
            paths.config_file(),
            r#"{"claude_profile":"aaaaaaaa-2222-4333-8444-555555555555",
                "claude_accounts":["bbbbbbbb-2222-4333-8444-555555555555"]}"#,
        )
        .await
        .unwrap();

        let config = UserConfig::load(&paths).await;
        assert_eq!(
            config.claude_accounts,
            vec!["bbbbbbbb-2222-4333-8444-555555555555".to_string()]
        );
    }

    #[tokio::test]
    async fn saving_drops_the_legacy_profile_from_the_file() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        UserConfig::update(&paths, |config| {
            config.claude_accounts = vec!["11111111-2222-4333-8444-555555555555".into()];
            config.claude_profile = Some("11111111-2222-4333-8444-555555555555".into());
        })
        .await
        .unwrap();

        let text = tokio::fs::read_to_string(paths.config_file())
            .await
            .unwrap();
        assert!(!text.contains("claude_profile"), "{text}");
        assert!(text.contains("claude_accounts"), "{text}");
    }

    /// The overwrite regression. A `config.json` riabuild cannot parse used to
    /// become `Default` in memory and then `Default` on disk, taking the
    /// checkout map, both pinned versions and every Claude account UUID with
    /// it — and the write that did it was the one under the lock, so there was
    /// nothing left to compare against afterwards.
    #[tokio::test]
    async fn a_config_that_will_not_parse_is_kept_rather_than_overwritten() {
        let home = TempDir::new().unwrap();
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root()).await.unwrap();
        tokio::fs::write(paths.config_file(), "{ half a file")
            .await
            .unwrap();

        let loaded = UserConfig::load(&paths).await;
        assert!(
            loaded.claude_accounts.is_empty(),
            "a file that will not parse cannot answer anything"
        );

        // The write that follows the read is what used to destroy it, and it
        // loads again on its way in — which is also the second call that must
        // not set a second copy aside.
        UserConfig::update(&paths, |config| {
            config.claude_accounts.push("fresh".into());
        })
        .await
        .expect("update");

        let mut names = Vec::new();
        let mut entries = tokio::fs::read_dir(paths.root()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        let kept: Vec<&String> = names
            .iter()
            .filter(|name| name.starts_with("config.json.broken-"))
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "exactly one copy should have been set aside: {names:?}"
        );
        assert_eq!(
            tokio::fs::read_to_string(paths.root().join(kept[0]))
                .await
                .unwrap(),
            "{ half a file",
            "the copy must be the developer's own bytes, unaltered"
        );
    }
}

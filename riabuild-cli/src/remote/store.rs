//! `remotes.json` — the servers this laptop knows about.
//!
//! No secrets live here: a local name, hash, hostname, port, username, and a
//! couple of timestamps are the whole file. The one secret remote mode keeps
//! anywhere is a server's own session token, and that lives at
//! `<namespace>/session.token` on the server itself — never here.

use crate::paths::Paths;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by Task 21
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub name: String,
    pub hash: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub added_at: u64,
    pub last_used_at: u64,
    /// When the session minted for this server runs out.
    pub session_expires_at: u64,
    pub last_seen_cli_version: String,
    /// The server's own absolute home directory, as reported by the server
    /// itself — never `~`. Asked for once by `remote::resolve_home` and kept
    /// here so every later command can use an absolute path without asking
    /// again. `#[serde(default)]` lets a `remotes.json` written before this
    /// field existed still deserialize (as `""`, which `resolve_home` treats
    /// as "not yet known" and re-asks for); it does not help struct-literal
    /// construction, so every literal in this file names it explicitly.
    #[serde(default)]
    pub home: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(dead_code)] // consumed by Task 21
#[serde(rename_all = "camelCase")]
pub struct Store {
    #[serde(default)]
    pub remotes: Vec<Record>,
}

impl Store {
    /// Infallible, like `State::load`: a store we cannot read or cannot parse
    /// means we know of no saved servers, and the correct response is to ask
    /// the developer rather than to panic or stop. This never touches disk to
    /// write anything, so a corrupt file is left exactly as it was — a later
    /// `save` is the only thing that overwrites it, and only with a full
    /// `Store` a caller built deliberately, never a silent wipe.
    #[allow(dead_code)] // consumed by Task 21
    pub async fn load(paths: &dyn Paths) -> Store {
        let Ok(text) = tokio::fs::read_to_string(paths.remotes_file()).await else {
            return Store::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    #[allow(dead_code)] // consumed by Task 21
    pub async fn save(&self, paths: &dyn Paths) -> Result<()> {
        crate::config::write_json(&paths.remotes_file(), self).await
    }

    #[allow(dead_code)] // consumed by Task 21
    pub fn find(&self, name: &str) -> Option<&Record> {
        self.remotes.iter().find(|record| record.name == name)
    }

    #[allow(dead_code)] // consumed by Task 21
    pub fn names(&self) -> Vec<String> {
        self.remotes.iter().map(|r| r.name.clone()).collect()
    }
}

/// A short local label, from the first label of the hostname.
#[allow(dead_code)] // consumed by Task 21 (also called from Remote::parse for a first guess)
pub fn allocate_name(host: &str, taken: &[String]) -> String {
    let base: String = host
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let base = if base.is_empty() {
        "server".to_string()
    } else {
        base
    };

    if !taken.iter().any(|name| name == &base) {
        return base;
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !taken.iter().any(|name| name == &candidate) {
            return candidate;
        }
    }
    base
}

/// A `Record` for `remote`, as if it had just been added and never connected
/// to. Shared across the remote test modules (13b, 17, 20, 21, 22) so there is
/// one definition of "a store entry that matches this `Remote`" instead of
/// each task's tests drifting from the next.
#[cfg(test)]
pub fn record_for(remote: &super::Remote) -> Record {
    Record {
        name: remote.name.clone(),
        hash: remote.hash(),
        host: remote.host.clone(),
        port: remote.port,
        user: remote.user.clone(),
        added_at: 0,
        last_used_at: 0,
        session_expires_at: 0,
        last_seen_cli_version: String::new(),
        home: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_comes_from_the_first_label_of_the_hostname() {
        assert_eq!(allocate_name("build-01.fly.dev", &[]), "build-01");
        assert_eq!(allocate_name("gpu.internal", &[]), "gpu");
        assert_eq!(allocate_name("192.168.1.10", &[]), "192");
    }

    #[test]
    fn a_taken_name_is_numbered_rather_than_reused() {
        let taken = vec!["build".to_string(), "build-2".to_string()];
        assert_eq!(allocate_name("build.example.com", &taken), "build-3");
    }

    #[test]
    fn a_hostname_with_nothing_usable_in_it_still_gets_a_name() {
        assert_eq!(allocate_name("", &[]), "server");
        assert_eq!(allocate_name("...", &[]), "server");
    }

    #[tokio::test]
    async fn an_unreadable_store_means_no_saved_servers_rather_than_an_error() {
        // Same rule as state.json: a file we cannot parse must degrade, never
        // stop a developer from connecting.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("mkdir");
        tokio::fs::write(paths.remotes_file(), "{{{ not json")
            .await
            .expect("write");

        assert!(Store::load(&paths).await.remotes.is_empty());
    }

    #[tokio::test]
    async fn a_missing_store_means_no_saved_servers_rather_than_an_error() {
        // The other degenerate case point 5 calls out: no file at all, not
        // even the directory it would live in.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());

        assert!(Store::load(&paths).await.remotes.is_empty());
    }

    #[tokio::test]
    async fn an_empty_store_means_no_saved_servers_rather_than_an_error() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("mkdir");
        tokio::fs::write(paths.remotes_file(), "")
            .await
            .expect("write");

        assert!(Store::load(&paths).await.remotes.is_empty());
    }

    #[tokio::test]
    async fn a_store_round_trips() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let mut store = Store::default();
        store.remotes.push(Record {
            name: "build-01".into(),
            hash: "9f2c000000000000".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
            added_at: 1,
            last_used_at: 2,
            session_expires_at: 0,
            last_seen_cli_version: "2026.08.06".into(),
            home: "/home/ada".into(),
        });
        store.save(&paths).await.expect("save");

        let loaded = Store::load(&paths).await;
        assert_eq!(loaded.remotes.len(), 1);
        assert_eq!(loaded.remotes[0].name, "build-01");
    }

    #[tokio::test]
    async fn saving_one_remote_does_not_erase_the_others_already_on_disk() {
        // Point 5's other half: `save` must write exactly what the caller
        // built, not silently drop what load could not parse into it — and a
        // caller that loaded, appended, and saved must see everything.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let mut store = Store::default();
        store.remotes.push(Record {
            name: "one".into(),
            hash: "aaaa000000000000".into(),
            host: "one.example.com".into(),
            port: 22,
            user: "ada".into(),
            added_at: 1,
            last_used_at: 1,
            session_expires_at: 0,
            last_seen_cli_version: "2026.08.06".into(),
            home: "/home/ada".into(),
        });
        store.save(&paths).await.expect("save");

        let mut reloaded = Store::load(&paths).await;
        reloaded.remotes.push(Record {
            name: "two".into(),
            hash: "bbbb000000000000".into(),
            host: "two.example.com".into(),
            port: 22,
            user: "ada".into(),
            added_at: 2,
            last_used_at: 2,
            session_expires_at: 0,
            last_seen_cli_version: "2026.08.06".into(),
            home: "/home/ada".into(),
        });
        reloaded.save(&paths).await.expect("save");

        let final_store = Store::load(&paths).await;
        assert_eq!(
            final_store.names(),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}

//! `remotes.json` — the servers this laptop knows about.
//!
//! No secrets live here: a local name, hash, hostname, port, username, and a
//! couple of timestamps are the whole file. The one secret remote mode keeps
//! anywhere is a server's own session token, and that lives at
//! `<namespace>/session.token` on the server itself — never here.

use super::Remote;
use crate::paths::Paths;
use crate::tasks::Ctx;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The `cliSessions` row id behind this server's own session, from
    /// `TokenResponse::session_id`. Empty until `remote::session::ensure`
    /// mints a session for the first time (or for a `remotes.json` written
    /// before this field existed — `#[serde(default)]` again, not
    /// struct-literal construction). `remote::forget::forget_remote` treats
    /// empty as "nothing minted, nothing to revoke" and skips straight to
    /// the SSH cleanup rather than calling the API with an empty id.
    #[serde(default)]
    pub session_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub async fn load(paths: &dyn Paths) -> Store {
        let Ok(text) = tokio::fs::read_to_string(paths.remotes_file()).await else {
            return Store::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub async fn save(&self, paths: &dyn Paths) -> Result<()> {
        crate::config::write_json(&paths.remotes_file(), self).await
    }

    pub fn find(&self, name: &str) -> Option<&Record> {
        self.remotes.iter().find(|record| record.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        self.remotes.iter().map(|r| r.name.clone()).collect()
    }
}

/// A short local label, from the first label of the hostname.
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

impl From<&Record> for Remote {
    fn from(record: &Record) -> Self {
        Remote {
            name: record.name.clone(),
            host: record.host.clone(),
            port: record.port,
            user: record.user.clone(),
        }
    }
}

/// The local login, for a first guess at who is connecting: `$USER`, falling
/// back to `$LOGNAME`, and finally to `"root"` — the account every image ships
/// with, so this never comes back empty.
pub fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

/// Records a newly-chosen server, as if it had just been added and never
/// connected to.
pub fn add(store: &mut Store, remote: &Remote) {
    store.remotes.push(Record {
        name: remote.name.clone(),
        hash: remote.hash(),
        host: remote.host.clone(),
        port: remote.port,
        user: remote.user.clone(),
        added_at: crate::config::now_secs(),
        last_used_at: 0,
        session_expires_at: 0,
        last_seen_cli_version: String::new(),
        home: String::new(),
        session_id: String::new(),
    });
}

/// Which server this invocation is about.
///
/// A `target` names a saved server or spells one out
/// (`[user@]host[:port]`); with none, an empty store asks the three
/// questions once, one saved server reconnects without asking, and several
/// saved servers are offered as a numbered list.
pub async fn choose(ctx: &mut Ctx, store: &mut Store, target: Option<String>) -> Result<Remote> {
    if let Some(target) = target {
        if let Some(record) = store.find(&target) {
            return Ok(record.into());
        }
        let user = whoami();
        let mut remote = Remote::parse(&target, &user)?;
        remote.name = allocate_name(&remote.host, &store.names());
        add(store, &remote);
        return Ok(remote);
    }

    match store.remotes.len() {
        0 => {
            let remote = ask_for_one(ctx, store).await?;
            add(store, &remote);
            Ok(remote)
        }
        1 => {
            let record = &store.remotes[0];
            ctx.ui.info(&format!(
                "Reconnecting to {} · {}@{}",
                record.name, record.user, record.host
            ));
            Ok(record.into())
        }
        _ => {
            ctx.ui.heading("Which server?");
            for (index, record) in store.remotes.iter().enumerate() {
                ctx.ui.info(&format!(
                    "  {}  {:<10} {}@{}{}   used {}",
                    index + 1,
                    record.name,
                    record.user,
                    record.host,
                    if record.port == 22 {
                        String::new()
                    } else {
                        format!(":{}", record.port)
                    },
                    crate::ui::duration_words(
                        crate::config::now_secs().saturating_sub(record.last_used_at) / 60
                    ),
                ));
            }
            let answer = ctx.ui.ask("", Some("1"))?;
            let index: usize = answer.trim().parse().unwrap_or(1);
            let record = store
                .remotes
                .get(index.saturating_sub(1))
                .ok_or_else(|| anyhow!("there is no server {index}"))?;
            Ok(record.into())
        }
    }
}

/// The three questions, once, on a first run.
async fn ask_for_one(ctx: &mut Ctx, store: &Store) -> Result<Remote> {
    ctx.ui.heading("Adding a server");
    let host = ctx.ui.ask("Hostname  ", None)?;
    let port: u16 = ctx.ui.ask("Port      ", Some("22"))?.parse().unwrap_or(22);
    let user = ctx.ui.ask("Username  ", Some(&whoami()))?;
    let name = allocate_name(&host, &store.names());
    ctx.ui
        .note(&format!("This server will be known as {name}."));
    Ok(Remote {
        name,
        host,
        port,
        user,
    })
}

/// What a successful connect leaves behind: this server moves to the front of
/// "recently used", and remembers the riabuild version it is now running.
pub async fn remember(ctx: &Ctx, store: &mut Store, remote: &Remote, version: &str) -> Result<()> {
    if let Some(record) = store.remotes.iter_mut().find(|r| r.name == remote.name) {
        record.last_used_at = crate::config::now_secs();
        record.last_seen_cli_version = version.to_string();
    }
    store.save(ctx.paths.as_ref()).await
}

/// `riabuild remote list`.
pub fn list(ctx: &Ctx, store: &Store) -> Result<i32> {
    if store.remotes.is_empty() {
        ctx.ui
            .info("No servers yet. Run `riabuild remote` to add one.");
        return Ok(0);
    }
    for record in &store.remotes {
        ctx.ui.info(&format!(
            "  {:<10} {}@{}{}   used {}",
            record.name,
            record.user,
            record.host,
            if record.port == 22 {
                String::new()
            } else {
                format!(":{}", record.port)
            },
            // `duration_words` takes minutes elapsed, not a timestamp.
            crate::ui::duration_words(
                crate::config::now_secs().saturating_sub(record.last_used_at) / 60
            ),
        ));
    }
    Ok(0)
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
        session_id: String::new(),
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

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    #[tokio::test]
    async fn one_saved_server_reconnects_without_asking() {
        let (mut ctx, _home) = crate::testing::ctx_with(crate::runner::FakeRunner::new()).await;
        let mut store = Store::default();
        store.remotes.push(record_for(&remote()));

        // `Ui::ask` would fail outright without a TTY, so reaching a prompt here
        // is itself the failure this asserts against.
        let chosen = choose(&mut ctx, &mut store, None)
            .await
            .expect("reconnects");
        assert_eq!(chosen.name, "build-01");
    }

    #[tokio::test]
    async fn a_named_server_that_is_not_saved_is_parsed_and_added() {
        let (mut ctx, _home) = crate::testing::ctx_with(crate::runner::FakeRunner::new()).await;
        let mut store = Store::default();

        let chosen = choose(&mut ctx, &mut store, Some("ada@gpu.internal:2222".into()))
            .await
            .expect("parses");
        assert_eq!(chosen.user, "ada");
        assert_eq!(chosen.port, 2222);
        assert_eq!(store.remotes.len(), 1);
    }

    #[tokio::test]
    async fn a_saved_server_named_on_the_command_line_is_reused_not_reparsed() {
        let (mut ctx, _home) = crate::testing::ctx_with(crate::runner::FakeRunner::new()).await;
        let mut store = Store::default();
        store.remotes.push(record_for(&remote()));

        let chosen = choose(&mut ctx, &mut store, Some("build-01".into()))
            .await
            .expect("finds the saved one");
        assert_eq!(chosen.host, "build-01.fly.dev");
        assert_eq!(
            store.remotes.len(),
            1,
            "a saved server must not be added a second time"
        );
    }

    #[tokio::test]
    async fn the_last_used_column_is_a_duration_not_a_timestamp() {
        let (ctx, _home) = crate::testing::ctx_with(crate::runner::FakeRunner::new()).await;
        let mut store = Store::default();
        let mut record = record_for(&remote());
        record.last_used_at = crate::config::now_secs().saturating_sub(3 * 3600);
        store.remotes.push(record);
        // Asserting the arithmetic rather than the wording: handing
        // `duration_words` the raw epoch renders roughly "1236111 days".
        assert_eq!(list(&ctx, &store).expect("lists"), 0);
    }

    #[test]
    fn whoami_never_comes_back_empty() {
        assert!(!whoami().is_empty());
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
            session_id: String::new(),
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
            session_id: String::new(),
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
            session_id: String::new(),
        });
        reloaded.save(&paths).await.expect("save");

        let final_store = Store::load(&paths).await;
        assert_eq!(
            final_store.names(),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}

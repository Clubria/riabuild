//! How a record reaches `remotes.json`, and how it leaves.
//!
//! Every write lands through `Ctx::update_state`'s read-modify-rename, and a
//! store that cannot be parsed is set aside rather than overwritten: the
//! sessions it records are the only thing that can revoke them.

use anyhow::{Result, anyhow};
use riabuild_paths::Paths;
use riabuild_tasks::Ctx;
use riabuild_ui::{Failure, Ui};

use super::{Origin, Record, Store};
use crate::Remote;

impl Store {
    /// Infallible, like `State::load`: a store we cannot read means we know of
    /// no saved servers, and the correct response is to ask the developer
    /// rather than to panic or stop.
    ///
    /// **Absent and unparseable are not the same thing, and this used to treat
    /// them as one.** Nothing there is the ordinary first run. A file that will
    /// not parse is every server this laptop has ever set up — and because
    /// `update` reads under the lock and writes the result back, the *first*
    /// `persist_one` of the next run replaced the damaged file with the one
    /// record that run happened to be about. Every other server's `sessionId`
    /// went with it, leaving 90-day sessions live on machines nothing on this
    /// laptop could name, let alone revoke.
    ///
    /// So an unparseable file is moved aside under a name that says what it is,
    /// and the developer is told where it went. This is `UserConfig::load`'s
    /// answer to the identical problem in `config.json`, deliberately spelled
    /// the same way — same `.broken-<epoch>` suffix, same warn-and-carry-on —
    /// rather than a second convention for one directory.
    pub async fn load(paths: &dyn Paths) -> Store {
        let path = paths.remotes_file();
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            return Store::default();
        };
        // An empty file is the other ordinary case — `write_json` has been
        // interrupted, or the file was touched into existence — and there is
        // nothing in it to keep.
        if text.trim().is_empty() {
            return Store::default();
        }
        match serde_json::from_str(&text) {
            Ok(store) => store,
            Err(error) => {
                keep_unreadable(&path, &error.to_string()).await;
                Store::default()
            }
        }
    }

    /// Acquires the lock, reads what is on disk *now*, applies `mutate`, and
    /// writes the result atomically. See `config::State::update`.
    ///
    /// Shares `state_lock_file` with the other two state files: contention is
    /// milliseconds, and one lock across all three removes any question of lock
    /// ordering between them.
    pub async fn update(paths: &dyn Paths, mutate: impl FnOnce(&mut Self)) -> Result<Self> {
        let _lock =
            riabuild_paths::filelock::FileLock::acquire(&paths.state_lock_file(), || {}).await?;
        let mut store = Self::load(paths).await;
        mutate(&mut store);
        riabuild_paths::config::write_json(&paths.remotes_file(), &store).await?;
        Ok(store)
    }
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
        added_at: riabuild_paths::config::now_secs(),
        last_used_at: 0,
        session_expires_at: 0,
        last_seen_cli_version: String::new(),
        home: String::new(),
        repo: String::new(),
        session_id: String::new(),
        shared_id: String::new(),
        // A server this developer typed in has nobody to describe it, and
        // riabuild is not going to: they are looking at the only description
        // there is.
        description: String::new(),
        fresh: false,
    });
}

/// Writes this run's record for `name` into whatever is on disk now, leaving
/// every other saved server exactly as it found them.
///
/// An upsert rather than a whole-store write, because the whole-store write is
/// the lost update itself: the `Store` a remote flow holds was read before a
/// long SSH conversation, and another terminal window may have added or removed
/// a server since. Writing this run's copy back wholesale would erase that.
///
/// The run's own copy is refreshed from what landed, so a caller that keeps
/// using `store` afterwards sees the merged result rather than its own snapshot.
/// Matched on the *display* name throughout, which is the one spelling that
/// belongs to exactly one server. The bare `name` does not: a shared `gpu` and
/// a `gpu` the developer added themselves both carry `"gpu"` in this field, and
/// keying on it would have each overwrite the other's row — one session id
/// landing on the other's record, and `forget` revoking a session for a machine
/// it is not about.
pub async fn persist_one(paths: &dyn Paths, store: &mut Store, name: &str) -> Result<()> {
    // A miss is an error, not a no-op. Every caller has just done something
    // worth keeping — minted a 90-day session, resolved a home, authorised a
    // key — and returning `Ok(())` with nothing written told them it landed.
    // The pairing that made it dangerous: `session::ensure` minted a session,
    // wrote the token onto the server, failed to find its own record, and
    // called this, which agreed. The result was a live session no `remote
    // forget` could ever name, which is the exact failure `sessionId` exists
    // to prevent.
    let Some(mine) = store.find(name).cloned() else {
        return Err(anyhow!(
            "riabuild has no saved record for \"{name}\", so there is nothing to write to remotes.json"
        ));
    };
    let fresh = store.fresh_shared();
    *store = Store::update(paths, |on_disk| {
        let key = mine.display_name();
        match on_disk.remotes.iter_mut().find(|r| r.display_name() == key) {
            Some(existing) => *existing = mine,
            None => on_disk.remotes.push(mine),
        }
    })
    .await?;
    store.restore_fresh(fresh);
    Ok(())
}

/// Drops the server named `name`, leaving every other saved server alone.
///
/// For a shared server this drops only this laptop's record of it — the row in
/// riabuild-web is untouched, so the server is back in the picker on the next
/// run with no key, no password and no session. The CLI has no way to remove a
/// server from the team, and this is not one.
pub async fn forget_one(paths: &dyn Paths, store: &mut Store, name: &str) -> Result<()> {
    let key = match store.find(name) {
        Some(record) => record.display_name(),
        None => name.to_string(),
    };
    // Everything this run learned from riabuild-web *except* the server being
    // forgotten. Without the exclusion `restore_fresh` puts it straight back:
    // one of the team's servers that this run refreshed is knowledge the disk
    // does not have, so it is re-added after the merge — including the one just
    // deleted, which would make `forget` a no-op for exactly the servers this
    // feature added.
    let fresh: Vec<Record> = store
        .fresh_shared()
        .into_iter()
        .filter(|record| record.display_name() != key)
        .collect();
    *store = Store::update(paths, |on_disk| {
        on_disk.remotes.retain(|r| r.display_name() != key);
    })
    .await?;
    store.restore_fresh(fresh);
    Ok(())
}

/// Refuses a record whose address this run has no business connecting to.
///
/// The shared-servers design puts it plainly: "every path that can lead to a
/// connection — the picker, and a target being resolved — sees only `Local` and
/// `Shared`". The picker enforced it; a target resolved by name or by hash did
/// not, so `riabuild remote shared-gpu` happily `ssh`'d to the address on disk
/// whenever the fetch had failed *or the leads had withdrawn the server*. That
/// is the remembered address being trusted, which is the one thing the whole
/// `fresh` mechanism exists to prevent.
///
/// A `Failure` rather than a silent skip: the record is still there, its
/// session may still be live, and `remote forget` is the command that clears
/// it — so the way out is worth naming.
pub(super) fn refuse_if_stale(record: &Record) -> Result<()> {
    if record.origin() != Origin::Stale {
        return Ok(());
    }
    Err(Failure::new(
        format!("connecting to {}", record.display_name()),
        format!(
            "riabuild is not connecting to a remembered address. If the team still has this \
             server, check you can reach riabuild-web and run it again; if they have removed \
             it, run `riabuild remote forget {}` to revoke the session riabuild left on it.",
            record.display_name()
        ),
    )
    .detail(format!(
        "riabuild-web did not describe {} on this run, so the address in remotes.json \
         ({}@{}:{}) is a memory rather than somewhere to connect.",
        record.display_name(),
        record.user,
        record.host,
        record.port
    ))
    .into())
}

/// Moves a `remotes.json` that will not parse aside, and says where it went.
///
/// Infallible on purpose, and spelled the way `paths::config`'s
/// `keep_unreadable` is: this is the recovery path of a read that has already
/// decided to carry on, so there is no caller to return an error to, and the
/// two files should not acquire two conventions. The `Ui` is built here for the
/// same reason it is there — `load` is on the read path of every remote command
/// and threading an output channel through all of them to serve a branch that
/// runs once in the life of a machine would put the cost in the wrong place.
async fn keep_unreadable(path: &std::path::Path, why: &str) {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "remotes.json".to_string());
    let aside = path.with_file_name(format!(
        "{name}.broken-{}",
        riabuild_paths::config::now_secs()
    ));
    let ui = Ui::new(false);

    match tokio::fs::rename(path, &aside).await {
        Ok(()) => ui.warn(&format!(
            "{} could not be read ({why}). It has been kept at {}, and riabuild is carrying on with a fresh one — so the sessions it recorded on your servers are still named in that copy, and `riabuild remote forget` can revoke them once it is back in place.",
            path.display(),
            aside.display()
        )),
        Err(error) => ui.warn(&format!(
            "{} could not be read ({why}), and could not be set aside either ({error}). riabuild is carrying on with a fresh one and the next write will replace it — copy it somewhere else now if you want the sessions it recorded to stay revocable.",
            path.display()
        )),
    }
}

/// What a successful connect leaves behind: this server moves to the front of
/// "recently used", remembers the riabuild version it is now running, and
/// remembers the repository it was set up for.
///
/// `repo` is `None` where this run had nothing to say about it — a `--check`,
/// or an unattended run on a server this laptop has never chosen for — and a
/// `None` leaves whatever is recorded alone rather than clearing it. Written
/// here rather than when the question is answered, because what is worth
/// remembering is the repository a server was *set up for*: a run that failed
/// on the way there has left the server on whatever it had before.
pub async fn remember(
    ctx: &Ctx,
    store: &mut Store,
    remote: &Remote,
    version: &str,
    repo: Option<&str>,
) -> Result<()> {
    // `find_mut`, not a match on the bare `name`: `remote.name` is the display
    // name, so a shared server would otherwise miss its own record and a local
    // server of the same bare name would be stamped instead.
    if let Some(record) = store.find_mut(&remote.name) {
        record.last_used_at = riabuild_paths::config::now_secs();
        record.last_seen_cli_version = version.to_string();
        if let Some(repo) = repo {
            record.repo = repo.to_string();
        }
    }
    persist_one(ctx.paths.as_ref(), store, &remote.name).await
}

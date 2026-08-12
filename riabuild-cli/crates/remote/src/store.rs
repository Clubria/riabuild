//! `remotes.json` — the servers this laptop knows about.
//!
//! No secrets live here: a local name, hash, hostname, port, username, and a
//! couple of timestamps are the whole file. The one secret remote mode keeps
//! anywhere is a server's own session token, and that lives at
//! `<namespace>/session.token` on the server itself — never here.

use super::Remote;
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_tasks::Ctx;
use riabuild_ui::Ui;
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
    /// `auth::Session::session_id`. Empty until `remote::session::ensure`
    /// mints a session for the first time (or for a `remotes.json` written
    /// before this field existed — `#[serde(default)]` again, not
    /// struct-literal construction). `remote::forget::forget_remote` treats
    /// empty as "nothing minted, nothing to revoke" and skips straight to
    /// the SSH cleanup rather than calling the API with an empty id.
    #[serde(default)]
    pub session_id: String,
    /// Empty for a server this laptop added. Otherwise the riabuild-web row id
    /// of the shared server this record holds *local state* for — the session,
    /// the home directory, and when it was last connected to, none of which is
    /// shareable and all of which is this laptop's alone.
    ///
    /// The address beside it is a *copy* of what riabuild-web last served, kept
    /// only so that an address a lead has since edited can still be cleaned up
    /// at the machine it used to name. It is never what riabuild connects to:
    /// see [`Record::origin`].
    #[serde(default)]
    pub shared_id: String,
    /// Whether this run's fetch of `/api/v1/remotes/shared` refreshed this
    /// record. In memory only, and **false by default on purpose** — a record
    /// read off the disk has not been refreshed by anything, so a shared server
    /// starts every run out of reach and becomes reachable only when
    /// riabuild-web has just described it. That is what makes "pull the address
    /// every time" a property of the code rather than a promise.
    #[serde(skip)]
    pub fresh: bool,
}

/// Where a record's address came from, which decides what may be done with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Added on this laptop. Its address is its own.
    Local,
    /// One of the team's, described by riabuild-web during this run.
    Shared,
    /// One of the team's, *not* described by riabuild-web during this run —
    /// either the fetch failed or the leads have removed it. The address here
    /// is a memory, so nothing may connect to it; the session recorded beside
    /// it may still be live, so `remote list` shows it and `remote forget`
    /// accepts it.
    Stale,
}

impl Record {
    pub fn origin(&self) -> Origin {
        if self.shared_id.is_empty() {
            Origin::Local
        } else if self.fresh {
            Origin::Shared
        } else {
            Origin::Stale
        }
    }

    pub fn is_shared(&self) -> bool {
        !self.shared_id.is_empty()
    }

    /// What this server is called everywhere a developer sees or types it.
    ///
    /// The prefix is applied here and stored nowhere: `remotes.json` holds the
    /// bare name with a `sharedId` beside it, and riabuild-web holds the bare
    /// name too. It exists between the two lists, which is where the collision
    /// it prevents actually happens — a team `gpu` and a `gpu` somebody added
    /// themselves are two servers, and the picker has to be able to say which.
    pub fn display_name(&self) -> String {
        if self.is_shared() {
            format!("{DISPLAY_PREFIX}{}", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// What a shared server's name is shown with. A local name may not start with
/// it — see [`ask_name`] — and riabuild-web refuses it on a shared name, so the
/// prefixed form belongs to exactly one server.
pub const DISPLAY_PREFIX: &str = "shared-";

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

    /// The server a developer meant by `name`.
    ///
    /// Two passes rather than one predicate with an `||` in it, because the
    /// ordering *is* the behaviour: a local server always answers to its own
    /// name, even when the team has a shared server called the same thing. A
    /// single `find` matching either spelling would resolve by whichever record
    /// happened to be saved first, which is the same class of bug the duplicate
    /// -record comment below this describes.
    ///
    /// So: the display name first — `gpu` finds a local `gpu`, `shared-gpu`
    /// finds the team's — and only then the bare name, which is what lets
    /// `riabuild remote gpu` still reach the team's `gpu` on a laptop that has
    /// no `gpu` of its own.
    pub fn find(&self, name: &str) -> Option<&Record> {
        self.find_index(name).map(|index| &self.remotes[index])
    }

    pub fn find_mut(&mut self, name: &str) -> Option<&mut Record> {
        self.find_index(name).map(|index| &mut self.remotes[index])
    }

    fn find_index(&self, name: &str) -> Option<usize> {
        self.remotes
            .iter()
            .position(|record| record.display_name() == name)
            .or_else(|| self.remotes.iter().position(|record| record.name == name))
    }

    /// The servers this run may connect to: everything except a shared server
    /// riabuild-web has not described during it. See [`Origin::Stale`].
    pub fn reachable(&self) -> Vec<&Record> {
        self.remotes
            .iter()
            .filter(|record| record.origin() != Origin::Stale)
            .collect()
    }

    /// Every name a developer could type, in the spelling they would type it.
    pub fn names(&self) -> Vec<String> {
        self.remotes.iter().map(Record::display_name).collect()
    }

    /// What this run learned from riabuild-web, which the disk does not know.
    fn fresh_shared(&self) -> Vec<Record> {
        self.remotes
            .iter()
            .filter(|record| record.origin() == Origin::Shared)
            .cloned()
            .collect()
    }

    /// Puts this run's knowledge of the team's servers back after a merge with
    /// the disk.
    ///
    /// `persist_one` and `forget_one` replace this run's `Store` with what
    /// actually landed, which is the whole point of them — they exist so that a
    /// second terminal window's servers are not erased by this one's snapshot.
    /// But what lands is what serde wrote, and a fresh shared server is in
    /// neither: `fresh` is `#[serde(skip)]`, and a shared server this laptop
    /// has never connected to has no row on disk at all. Without this, the
    /// first save of a connect would drop the very server being connected to
    /// out of the store it is being read from.
    fn restore_fresh(&mut self, fresh: Vec<Record>) {
        for record in fresh {
            match self
                .remotes
                .iter_mut()
                .find(|existing| existing.shared_id == record.shared_id)
            {
                Some(existing) => existing.fresh = true,
                None => self.remotes.push(record),
            }
        }
    }
}

/// How many times riabuild asks for a usable name before naming the server
/// itself. Bounded for the same reason `tasks::project`'s checkout prompt is:
/// a developer who cannot give a usable answer is better served by riabuild
/// picking one than by being asked forever.
const NAME_ATTEMPTS: usize = 3;

/// What a typed name is reduced to before it is used.
///
/// The name is not decoration. It is exported as `RIABUILD_REMOTE=<name>`
/// inside the single-quoted `env …` prefix every remote invocation is wrapped
/// in, and it is what `riabuild remote forget <name>` looks a server up by. So
/// it is held to what a shell word and a lookup key can both carry: letters,
/// digits, dot, dash, underscore. Anything else is dropped rather than
/// rejected, and the developer is told the name they actually got.
pub fn sanitise_name(typed: &str) -> String {
    typed
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// The developer's own label for the server being added, offered riabuild's
/// guess as the default.
///
/// `ask`, not `ask_required`: there is a sensible default here, so an
/// unattended run — `riabuild remote user@host --accept-host-key …` in CI, or
/// anything with no terminal — takes it silently rather than failing. That is
/// the crate rule in `CLAUDE.md`, and it is also what keeps this prompt from
/// turning a scripted `remote` invocation into a hang.
///
/// Why ask at all: the default is the first label of the hostname, which is
/// the machine's name only when a developer connects straight to the machine.
/// Behind a gateway it is the gateway's — every server reached through
/// `ssh.cloudcli.ai` wants to be called `ssh`, then `ssh-2`, then `ssh-3`, and
/// the list in `remote list` stops telling anyone which box is which.
pub fn ask_name(ui: &Ui, host: &str, taken: &[String]) -> String {
    let default = allocate_name(host, taken);
    for _ in 0..NAME_ATTEMPTS {
        // `None` is Enter, ^D, or nobody there — all of which mean "use the
        // one you suggested", so none of them cost the developer an attempt.
        let Some(answer) = ui.ask(&format!("Name this server [{default}]")) else {
            break;
        };
        let name = sanitise_name(&answer);
        if name.is_empty() {
            ui.warn("A name can hold letters, digits, dots, dashes and underscores.");
            continue;
        }
        if taken.iter().any(|other| other == &name) {
            // Not a cosmetic clash: `Store::find` returns the first match, so
            // two records under one name means `remote forget <name>` and
            // every later reconnect act on whichever was saved first.
            ui.warn(&format!(
                "{name} is already the name of another server. Pick another."
            ));
            continue;
        }
        if name.to_ascii_lowercase().starts_with(DISPLAY_PREFIX) {
            // Reserved: it is how the team's servers are shown, so a local one
            // wearing it would be two servers a developer cannot tell apart at
            // the prompt — the same reason a name already taken is refused.
            ui.warn(&format!(
                "Names starting with \"{DISPLAY_PREFIX}\" belong to the team's servers. Pick another."
            ));
            continue;
        }
        if name != answer.trim() {
            ui.note(&format!("This server will be known as {name}."));
        }
        return name;
    }
    ui.note(&format!("This server will be known as {default}."));
    default
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
    /// The *display* name travels, not the bare one. `Remote::name` is what
    /// reaches `RIABUILD_REMOTE`, so it is what the server's shell banner reads
    /// back — "active on shared-gpu" — and what `store::remember` looks the
    /// record up by afterwards. `Remote::hash` is taken over the login target
    /// and never the name, so nothing about which key or session this server
    /// uses depends on the prefix.
    fn from(record: &Record) -> Self {
        Remote {
            name: record.display_name(),
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
        added_at: riabuild_paths::config::now_secs(),
        last_used_at: 0,
        session_expires_at: 0,
        last_seen_cli_version: String::new(),
        home: String::new(),
        session_id: String::new(),
        shared_id: String::new(),
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
    let mine = store.find(name).cloned();
    let fresh = store.fresh_shared();
    *store = Store::update(paths, |on_disk| {
        let Some(mine) = mine else {
            return;
        };
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

/// Which server this invocation is about.
///
/// A `target` names a saved server or spells one out (`[user@]host[:port]`).
/// With none, the question belongs to `pick`: the servers already saved, plus
/// the option of adding one.
pub async fn choose(ctx: &mut Ctx, store: &mut Store, target: Option<String>) -> Result<Remote> {
    if let Some(target) = target {
        if let Some(record) = store.find(&target) {
            return Ok(record.into());
        }
        let user = whoami();
        let mut remote = Remote::parse(&target, &user)?;

        // `remotes.json` is looked up by local *name*, but a developer who set
        // a server up by typing its spec was never told it acquired one — the
        // "will be known as" note only fires on `ask_for_one`'s interactive
        // path. So they retype last week's working command, `find` misses, and
        // without this a second record called `build-01-2` is created for one
        // machine: a browser sign-in on every run, a fresh `cliSessions` row
        // each time, and one keychain item — keyed on `Remote::hash()`, which
        // is identical for both records — that each overwrites from the other,
        // so `forget build-01` leaves `build-01-2` with neither a session nor
        // a key. `Remote::hash()` is the documented identity of a server; this
        // is the code matching the documentation.
        // Matched on `Remote::hash()`, which covers the whole login target —
        // user, host, port — so what this reunites is a respelt *host*
        // (`Build-01.Fly.Dev`, an FQDN's trailing dot), not every spelling of
        // the same machine. A spec with no `user@` falls back to `whoami()`,
        // so a bare host typed for a server that was added under some other
        // username hashes differently and still forks a second record. That
        // one is left alone deliberately: riabuild cannot tell it apart from a
        // genuine second account on the same box, and the run that follows
        // fails at authentication, which announces itself.
        if let Some(record) = store.remotes.iter_mut().find(|r| r.hash == remote.hash()) {
            // The freshly typed spelling wins — unless the address is the
            // team's, where riabuild-web's spelling is the one that has to
            // survive the run: this record is refreshed from it on every fetch,
            // so a local edit here would be overwritten anyway, and in the
            // meantime `remote list` would show a developer's typing as though
            // a lead had entered it. The identity is the same either way, since
            // this branch is reached only on an equal `Remote::hash`.
            if !record.is_shared() {
                record.host = remote.host.clone();
                record.port = remote.port;
                record.user = remote.user.clone();
            }
            return Ok(Remote::from(&*record));
        }

        // Asked here too, not only on `ask_for_one`'s path: a server typed as
        // a spec is just as much a server being added, and it is the path a
        // developer behind a gateway is most likely to use — where the name
        // riabuild would allocate is the gateway's hostname rather than the
        // machine's.
        remote.name = ask_name(&ctx.ui, &remote.host, &store.names());
        add(store, &remote);
        return Ok(remote);
    }

    super::pick::pick(ctx, store).await
}

/// What a successful connect leaves behind: this server moves to the front of
/// "recently used", and remembers the riabuild version it is now running.
pub async fn remember(ctx: &Ctx, store: &mut Store, remote: &Remote, version: &str) -> Result<()> {
    // `find_mut`, not a match on the bare `name`: `remote.name` is the display
    // name, so a shared server would otherwise miss its own record and a local
    // server of the same bare name would be stamped instead.
    if let Some(record) = store.find_mut(&remote.name) {
        record.last_used_at = riabuild_paths::config::now_secs();
        record.last_seen_cli_version = version.to_string();
    }
    persist_one(ctx.paths.as_ref(), store, &remote.name).await
}

/// `riabuild remote list`.
///
/// The same box the picker shows, minus the numbers nothing here is waiting to
/// read. An empty store keeps its one line rather than rendering a box with no
/// rows, whose hints would name no server — `render::hints` exists to keep a
/// developer from being shown a command that refuses when typed.
pub fn list(ctx: &Ctx, store: &Store) -> Result<i32> {
    if store.remotes.is_empty() {
        ctx.ui
            .info("No servers yet. Run `riabuild remote` to add one.");
        return Ok(0);
    }
    ctx.ui.info("");
    ctx.ui.info(&super::render::servers_box(
        &listing_order(store),
        super::render::Shown::Listing,
        ctx.ui.theme(),
    ));
    Ok(0)
}

/// Everything that can be connected to, and then everything that cannot.
///
/// A server the leads have removed is still shown — its session may still be
/// live, and a row nobody can see is a row nobody can clear — but it is shown
/// after the servers that work, marked `no longer shared`, so the list still
/// reads top-down as "what you can use".
fn listing_order(store: &Store) -> Vec<Record> {
    let (stale, usable): (Vec<Record>, Vec<Record>) = store
        .remotes
        .iter()
        .cloned()
        .partition(|record| record.origin() == Origin::Stale);
    usable.into_iter().chain(stale).collect()
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
        shared_id: String::new(),
        fresh: false,
    }
}

/// A `Record` for one of the team's servers, as this run's fetch would leave
/// it: refreshed, and never connected to.
#[cfg(test)]
pub fn shared_record_for(remote: &super::Remote, id: &str) -> Record {
    Record {
        shared_id: id.to_string(),
        fresh: true,
        ..record_for(remote)
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

    fn named(name: &str, host: &str) -> Remote {
        Remote {
            name: name.into(),
            host: host.into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// One server the developer added, and one of the team's, deliberately
    /// under the same bare name — the collision the display prefix exists for.
    fn both_called_gpu() -> Store {
        let mut store = Store::default();
        store.remotes.push(record_for(&named("gpu", "gpu.local")));
        store
            .remotes
            .push(shared_record_for(&named("gpu", "gpu.internal"), "k1"));
        store
    }

    #[test]
    fn a_local_server_answers_to_its_own_name_even_when_the_team_has_one_too() {
        // The ordering is the behaviour. A single `find` matching either
        // spelling would resolve by whichever record happened to be saved
        // first, so which `gpu` a developer reached would depend on the order
        // of a JSON file.
        let store = both_called_gpu();

        assert_eq!(store.find("gpu").expect("finds one").host, "gpu.local");
        assert_eq!(
            store.find("shared-gpu").expect("finds one").host,
            "gpu.internal"
        );
    }

    #[test]
    fn a_bare_name_reaches_the_teams_server_when_nothing_local_claims_it() {
        let mut store = Store::default();
        store
            .remotes
            .push(shared_record_for(&named("gpu", "gpu.internal"), "k1"));

        assert_eq!(
            store.find("gpu").expect("finds the team's").host,
            "gpu.internal"
        );
    }

    #[test]
    fn the_display_prefix_is_never_written_down() {
        // `remotes.json` holds the bare name with a sharedId beside it, and
        // riabuild-web holds the bare name too. The prefix lives between the
        // two lists, which is where the collision it prevents happens.
        let record = shared_record_for(&named("gpu", "gpu.internal"), "k1");
        assert_eq!(record.name, "gpu");
        assert_eq!(record.display_name(), "shared-gpu");

        let json = serde_json::to_string(&record).expect("serialises");
        assert!(!json.contains("shared-gpu"), "{json}");
        assert!(json.contains("\"sharedId\":\"k1\""), "{json}");
        // …and freshness is not written either, so a record read back off the
        // disk is out of reach until a fetch describes it again.
        assert!(!json.contains("fresh"), "{json}");
    }

    #[tokio::test]
    async fn a_record_read_back_off_the_disk_is_out_of_reach_until_a_fetch() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = Store::default();
        store
            .remotes
            .push(shared_record_for(&named("gpu", "gpu.internal"), "k1"));
        persist_one(&paths, &mut store, "shared-gpu")
            .await
            .expect("persists");

        let loaded = Store::load(&paths).await;

        assert_eq!(loaded.remotes.len(), 1);
        assert_eq!(loaded.remotes[0].origin(), Origin::Stale);
        assert!(
            loaded.reachable().is_empty(),
            "an address off the disk is a memory, not somewhere to connect"
        );
    }

    #[tokio::test]
    async fn two_servers_of_one_bare_name_are_two_rows_on_disk() {
        // `persist_one` upserts, and keying that on the bare name would have
        // these two overwrite each other: one session id landing on the other's
        // record, and `forget` revoking a session for a machine it is not
        // about.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = both_called_gpu();
        store.remotes[1].session_id = "sess_team".into();

        persist_one(&paths, &mut store, "gpu").await.expect("local");
        persist_one(&paths, &mut store, "shared-gpu")
            .await
            .expect("shared");

        let loaded = Store::load(&paths).await;
        assert_eq!(loaded.remotes.len(), 2, "{:?}", loaded.names());
        assert_eq!(loaded.find("gpu").expect("local").session_id, "");
        assert_eq!(
            loaded.find("shared-gpu").expect("shared").session_id,
            "sess_team"
        );
    }

    #[tokio::test]
    async fn persisting_a_server_leaves_the_teams_others_reachable() {
        // `persist_one` replaces this run's store with what landed on disk, and
        // freshness is in-memory only — so without `mark_fresh` a mid-flow save
        // would turn every shared server Stale and the connect in progress
        // would find its own server unreachable.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = both_called_gpu();

        persist_one(&paths, &mut store, "shared-gpu")
            .await
            .expect("persists");

        // Still there, and still reachable — the connect that triggered this
        // save has several more steps to run against this very record.
        let record = store.find("shared-gpu").expect("still there");
        assert_eq!(record.origin(), Origin::Shared);
        assert_eq!(record.host, "gpu.internal");
        assert!(!store.reachable().is_empty());
    }

    #[tokio::test]
    async fn forgetting_one_of_the_teams_servers_leaves_the_local_one_of_that_name() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let mut store = both_called_gpu();
        persist_one(&paths, &mut store, "gpu").await.expect("local");
        persist_one(&paths, &mut store, "shared-gpu")
            .await
            .expect("shared");

        forget_one(&paths, &mut store, "shared-gpu")
            .await
            .expect("forgets");

        let loaded = Store::load(&paths).await;
        assert_eq!(loaded.names(), vec!["gpu".to_string()]);
        assert_eq!(loaded.remotes[0].host, "gpu.local");
    }

    #[tokio::test]
    async fn a_developer_cannot_name_a_server_the_way_the_team_s_are_shown() {
        // Reserved, for the same reason a name already taken is: two servers a
        // developer cannot tell apart at the prompt.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["shared-gpu", "mine"]);
        let mut store = Store::default();

        let chosen = choose(&mut ctx, &mut store, Some("ada@gpu.internal".into()))
            .await
            .expect("adds it under the second answer");

        assert_eq!(chosen.name, "mine");
        assert!(
            ctx.ui
                .warned()
                .iter()
                .any(|warning| warning.contains("belong to the team")),
            "{:?}",
            ctx.ui.warned()
        );
    }

    /// With nobody there to ask — a script, a CI job, this test process — one
    /// saved server is still reconnected to rather than asked about. A
    /// developer at a terminal now gets the picker instead
    /// (`pick::one_saved_server_is_still_offered_a_choice_when_someone_is_there`);
    /// what this pins is that `choose` still reaches the unattended answer
    /// through `pick`, and asks nothing on the way.
    #[tokio::test]
    async fn one_saved_server_reconnects_without_asking_when_nobody_is_there() {
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        let mut store = Store::default();
        store.remotes.push(record_for(&remote()));

        let chosen = choose(&mut ctx, &mut store, None)
            .await
            .expect("reconnects");
        assert_eq!(chosen.name, "build-01");
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }

    #[test]
    fn a_typed_name_is_reduced_to_what_a_shell_word_can_carry() {
        assert_eq!(sanitise_name("  gpu-01 "), "gpu-01");
        assert_eq!(sanitise_name("bench.eu_west"), "bench.eu_west");
        // The name is interpolated into the single-quoted `env 'RIABUILD_REMOTE=…'`
        // prefix every remote invocation is wrapped in, so a quote or a space
        // in it is a broken command line, not a quirky label.
        let hostile = sanitise_name("gpu'; rm -rf /");
        assert!(
            !hostile.contains(['\'', ' ', ';', '/']),
            "{hostile} would not survive being quoted into a command"
        );
        assert_eq!(sanitise_name("  "), "");
        assert_eq!(sanitise_name("🚀"), "");
    }

    #[tokio::test]
    async fn a_server_being_added_is_named_by_the_developer() {
        // The hostname's first label is a poor name behind a gateway: every
        // server reached through `ssh.cloudcli.ai` would be `ssh`, `ssh-2`,
        // `ssh-3`, and `remote list` would stop telling anyone which is which.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["gpu-box"]);
        let mut store = Store::default();

        let chosen = choose(&mut ctx, &mut store, Some("clubria@ssh.cloudcli.ai".into()))
            .await
            .expect("adds it");

        assert_eq!(chosen.name, "gpu-box");
        assert_eq!(store.remotes[0].name, "gpu-box");
        assert_eq!(
            store.remotes[0].host, "ssh.cloudcli.ai",
            "the name is a label, never the address riabuild connects to"
        );
    }

    #[tokio::test]
    async fn a_name_nobody_types_is_the_one_riabuild_allocated() {
        // The unattended path: no terminal, so `ask` answers `None` and the
        // run must go on with riabuild's own guess rather than fail or hang.
        // `Ui` under `cfg(test)` is never interactive, which is exactly CI —
        // `new(false)` only turns off `--quiet`, so the note can be seen.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = Ui::new(false);
        let mut store = Store::default();

        let chosen = choose(&mut ctx, &mut store, Some("clubria@ssh.cloudcli.ai".into()))
            .await
            .expect("must not need an answer");

        assert_eq!(chosen.name, "ssh");
        assert!(
            ctx.ui
                .noted()
                .iter()
                .any(|note| note.contains("known as ssh")),
            "a name nobody chose has to be announced: {:?}",
            ctx.ui.noted()
        );
    }

    #[tokio::test]
    async fn a_name_already_in_use_is_refused_rather_than_duplicated() {
        // `Store::find` returns the first match, so two records under one name
        // means `remote forget <name>` and every later reconnect act on
        // whichever happened to be saved first.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["build-01", "build-02"]);
        let mut store = Store::default();
        store.remotes.push(record_for(&remote()));

        let chosen = choose(&mut ctx, &mut store, Some("ada@gpu.internal".into()))
            .await
            .expect("adds it under the second answer");

        assert_eq!(chosen.name, "build-02");
        assert_eq!(
            store.names(),
            vec!["build-01".to_string(), "build-02".to_string()]
        );
    }

    #[tokio::test]
    async fn a_name_with_nothing_usable_left_in_it_is_asked_for_again() {
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["🚀", "rocket"]);
        let mut store = Store::default();

        let chosen = choose(&mut ctx, &mut store, Some("ada@gpu.internal".into()))
            .await
            .expect("adds it");

        assert_eq!(chosen.name, "rocket");
    }

    #[tokio::test]
    async fn a_developer_who_cannot_give_a_usable_name_is_not_asked_forever() {
        // Bounded, like `tasks::project`'s checkout prompt: riabuild names the
        // server itself rather than looping on a question with no good answer.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["!!!", "???", "***", "still-not-used"]);
        let mut store = Store::default();

        let chosen = choose(&mut ctx, &mut store, Some("ada@gpu.internal".into()))
            .await
            .expect("adds it under riabuild's own name");

        assert_eq!(chosen.name, "gpu");
    }

    #[tokio::test]
    async fn a_server_that_is_only_being_reconnected_to_is_not_renamed() {
        // The over-correction to guard against: the prompt belongs to *adding*
        // a server. A saved one answered from `remotes.json` must not be asked
        // about — an answer waiting in the script proves it was never read.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["renamed"]);
        let mut store = Store::default();
        store.remotes.push(record_for(&remote()));

        let chosen = choose(&mut ctx, &mut store, Some("build-01".into()))
            .await
            .expect("finds the saved one");

        assert_eq!(chosen.name, "build-01");
        assert!(
            ctx.ui.asked().is_empty(),
            "reconnecting must ask nothing: {:?}",
            ctx.ui.asked()
        );
    }

    #[tokio::test]
    async fn a_named_server_that_is_not_saved_is_parsed_and_added() {
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
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
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
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
    async fn the_same_spec_twice_is_one_server_rather_than_two_records() {
        // Last week's working command, retyped. The record it created is named
        // `build-01`, so `find("build-01.fly.dev")` misses and `allocate_name`
        // sees `build-01` taken — and a second record for one machine means a
        // browser sign-in every run and two records fighting over one keychain
        // item, since `Remote::hash()` is identical for both.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        let mut store = Store::default();

        let first = choose(&mut ctx, &mut store, Some("ada@build-01.fly.dev".into()))
            .await
            .expect("adds it");
        let second = choose(&mut ctx, &mut store, Some("ada@build-01.fly.dev".into()))
            .await
            .expect("finds it again");

        assert_eq!(
            store.remotes.len(),
            1,
            "one server, typed twice, is one record: {:?}",
            store.names()
        );
        assert_eq!(first.name, second.name);
        assert_eq!(first.hash(), second.hash());
    }

    #[tokio::test]
    async fn a_respelt_server_updates_its_record_rather_than_forking_it() {
        // `Remote::hash()` already normalises hostname case and the trailing
        // dot of an FQDN, so these are one server by definition. The record
        // follows the spelling in front of the developer rather than drifting
        // from it.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        let mut store = Store::default();

        choose(&mut ctx, &mut store, Some("ada@Build-01.Fly.Dev.".into()))
            .await
            .expect("adds it");
        let again = choose(&mut ctx, &mut store, Some("ada@build-01.fly.dev".into()))
            .await
            .expect("same server");

        assert_eq!(store.remotes.len(), 1, "{:?}", store.names());
        assert_eq!(store.remotes[0].host, "build-01.fly.dev");
        assert_eq!(again.host, "build-01.fly.dev");
    }

    #[tokio::test]
    async fn a_genuinely_different_server_still_gets_a_record_of_its_own() {
        // The over-correction to guard against: matching on identity must not
        // collapse two machines that merely share a first hostname label.
        let (mut ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        let mut store = Store::default();

        let one = choose(&mut ctx, &mut store, Some("ada@build-01.fly.dev".into()))
            .await
            .expect("adds the first");
        let two = choose(&mut ctx, &mut store, Some("ada@build-01.other.dev".into()))
            .await
            .expect("adds the second");

        assert_eq!(store.remotes.len(), 2, "{:?}", store.names());
        assert_ne!(one.name, two.name);
        assert_eq!(store.names(), vec!["build-01", "build-01-2"]);
    }

    /// `list` renders through `render::servers_box` now, and what each column
    /// says is asserted there. What is still this function's own is the
    /// dispatch: a store with servers in it renders the box, and an empty one
    /// takes the line above instead of a box whose hints could name nothing.
    #[tokio::test]
    async fn a_saved_server_is_listed_and_an_empty_store_is_not_a_box() {
        let (ctx, _home) =
            riabuild_tasks::testing::ctx_with(riabuild_runner::FakeRunner::new()).await;
        let mut store = Store::default();
        assert_eq!(list(&ctx, &store).expect("lists nothing"), 0);

        let mut record = record_for(&remote());
        record.last_used_at = riabuild_paths::config::now_secs().saturating_sub(3 * 3600);
        store.remotes.push(record);
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
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
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
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());

        assert!(Store::load(&paths).await.remotes.is_empty());
    }

    #[tokio::test]
    async fn an_empty_store_means_no_saved_servers_rather_than_an_error() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
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
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
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
            shared_id: String::new(),
            fresh: false,
        });
        Store::update(&paths, |on_disk| *on_disk = store)
            .await
            .expect("save");

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
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
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
            shared_id: String::new(),
            fresh: false,
        });
        Store::update(&paths, |on_disk| *on_disk = store)
            .await
            .expect("save");

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
            shared_id: String::new(),
            fresh: false,
        });
        Store::update(&paths, |on_disk| *on_disk = reloaded)
            .await
            .expect("save");

        let final_store = Store::load(&paths).await;
        assert_eq!(
            final_store.names(),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}

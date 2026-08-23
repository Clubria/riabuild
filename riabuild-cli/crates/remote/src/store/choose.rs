//! Which server this invocation is about.

use anyhow::Result;
use riabuild_tasks::Ctx;

use super::naming::ask_name;
use super::persist::{add, refuse_if_stale};
use super::{Store, whoami};
use crate::Remote;

/// Which server this invocation is about.
///
/// A `target` names a saved server or spells one out (`[user@]host[:port]`).
/// With none, the question belongs to `pick`: the servers already saved, plus
/// the option of adding one.
pub async fn choose(ctx: &mut Ctx, store: &mut Store, target: Option<String>) -> Result<Remote> {
    if let Some(target) = target {
        if let Some(record) = store.find(&target) {
            refuse_if_stale(record)?;
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
            // Reached by typing the *address* of one of the team's servers
            // rather than its name, which resolves to the same record and so
            // has to answer the same way. See [`refuse_if_stale`].
            refuse_if_stale(record)?;
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

    crate::pick::pick(ctx, store).await
}

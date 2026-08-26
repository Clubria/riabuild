//! The team's servers, as this run learns about them.
//!
//! `remotes.json` holds what *this laptop* knows about a server — the session
//! it minted, the home directory it resolved, when it last connected. For one
//! of the team's servers it holds the address too, but only as a copy: the
//! address riabuild connects to comes from riabuild-web on every run, and a
//! record that this run's fetch did not describe is [`Origin::Stale`] and
//! unreachable. There is no path from a remembered address to an `ssh`
//! command, which is what makes "pull it every time" a property of the code.
//!
//! Design: `docs/superpowers/specs/2026-08-12-shared-servers-design.md`.

use super::Remote;
use super::store::{self, Origin, Record, Store};
use riabuild_api::remotes::{self, SharedServer};
use riabuild_tasks::Ctx;
use riabuild_ui::Ui;

/// Fetches the team's servers, and never fails.
///
/// Every failure — an unreachable riabuild-web, a 500, a body that will not
/// parse — becomes the same note and an empty list. A developer who cannot
/// reach the team's servers can still reach the one they set up themselves,
/// and a server list is a smaller thing to lose than that.
///
/// A server riabuild-web described but this riabuild will not use is reported
/// individually, naming it: a row that vanishes silently from a picker is a
/// support ticket, and the reason it went is the whole answer.
pub async fn fetch(ctx: &Ctx) -> Vec<SharedServer> {
    match remotes::fetch_shared(&ctx.api).await {
        Ok(fetched) => {
            for refused in &fetched.refused {
                ctx.ui
                    .warn(&format!("Ignoring one of the team's servers: {refused}"));
            }
            fetched.servers
        }
        Err(error) => {
            ctx.ui.note(&format!(
                "Could not load the team's servers, so this is only what is on this laptop. ({error})"
            ));
            Vec::new()
        }
    }
}

/// What every command on its way to a connection does first: ask riabuild-web
/// which servers the team has, and fold the answer in.
///
/// Returns the identities a lead's edit has superseded, for the caller to
/// retire — see [`reconcile`].
pub async fn refresh(ctx: &Ctx, store: &mut Store) -> Vec<Record> {
    let servers = fetch(ctx).await;
    reconcile(&ctx.ui, store, &servers)
}

/// The same, for `riabuild remote list`, which only reads.
///
/// Two differences, and both are about not surprising a developer who asked to
/// be shown a list:
///
/// - **It signs in softly.** `remote list` worked with no network at all before
///   the team's servers existed, and it still does: a failure to reach
///   riabuild-web is a note and this laptop's own servers, never an error.
/// - **It retires nothing.** A superseded identity is dropped on the floor
///   here, because clearing one means an SSH round trip to a machine the
///   developer did not ask about. Nothing is persisted either, so the address
///   on disk stays exactly as it was and the next connect finds the same
///   mismatch and deals with it.
pub async fn refresh_for_listing(ctx: &mut Ctx, store: &mut Store) {
    if let Err(error) = ctx.connect().await {
        ctx.ui.note(&format!(
            "Could not reach riabuild-web, so this is only what is on this laptop. ({error})"
        ));
        return;
    }
    let servers = fetch(ctx).await;
    reconcile(&ctx.ui, store, &servers);
}

/// Folds a fetch into the store, and reports the identities it replaced.
///
/// Three things happen, one per kind of server:
///
/// - **Described, and known here** — the record takes riabuild-web's name and
///   address, and becomes reachable for this run. Its session, its home and
///   its timestamps are left alone; they are this laptop's, not the team's.
/// - **Described, and new here** — a record arrives with empty state, exactly
///   as if the developer had just typed the address in.
/// - **Not described** — left untouched and unreachable. Either the fetch
///   failed or the leads removed it, and neither is a reason to connect to a
///   remembered address or to drop a record whose session may still be live.
///
/// The returned records are the *old* copies of servers whose address a lead
/// has edited since this laptop last connected. An address is an identity —
/// `Remote::hash` is taken over `user@host:port` — so each of those names a
/// machine holding a key riabuild put there and, possibly, a live session.
/// They are returned rather than acted on here because acting means SSH and an
/// API call, and this function is also what `remote list` runs.
pub fn reconcile(ui: &Ui, store: &mut Store, servers: &[SharedServer]) -> Vec<Record> {
    let mut superseded = Vec::new();

    for server in servers {
        let name = usable_name(ui, server);
        let remote = Remote {
            name: name.clone(),
            host: server.host.clone(),
            port: server.port,
            user: server.user.clone(),
        };
        let hash = remote.hash();

        let Some(record) = store
            .remotes
            .iter_mut()
            .find(|record| record.shared_id == server.id)
        else {
            store.remotes.push(Record {
                name: name.clone(),
                hash,
                host: server.host.clone(),
                port: server.port,
                user: server.user.clone(),
                added_at: riabuild_paths::config::now_secs(),
                last_used_at: 0,
                session_expires_at: 0,
                last_seen_cli_version: String::new(),
                home: String::new(),
                repo: String::new(),
                session_id: String::new(),
                shared_id: server.id.clone(),
                fresh: true,
            });
            continue;
        };

        if record.hash != hash {
            // A different machine under the same name. What is left behind is
            // a key riabuild authorised there and, if this laptop ever
            // connected, a session that is still live — so the old copy goes
            // back to the caller before it is overwritten.
            superseded.push(record.clone());
            record.session_id = String::new();
            record.session_expires_at = 0;
            record.home = String::new();
            record.last_used_at = 0;
        }

        record.name = name;
        record.host = server.host.clone();
        record.port = server.port;
        record.user = server.user.clone();
        record.hash = hash;
        record.fresh = true;
    }

    superseded
}

/// What one of the team's servers is called on *this* laptop.
///
/// A locally typed name is put through `store::sanitise_name` and refused the
/// reserved prefix; a server-supplied one used to be copied onto `Record::name`
/// exactly as it arrived. That field is not decoration — it becomes
/// `RIABUILD_REMOTE=<name>` inside the single-quoted `env …` prefix every
/// remote invocation is wrapped in, and it is the key `find`, `persist_one` and
/// `forget_one` all match on. `api::remotes::usable` does refuse both shapes on
/// the way in, so this is the second of two checks rather than the only one —
/// which is the point: the field is used as a shell word and a lookup key here,
/// so the rule belongs here too and cannot be lost by a change to the wire
/// format.
///
/// Said out loud when it changes anything, once per server, because the name a
/// lead typed into the dashboard and the name in `riabuild remote list` are
/// then two different strings and nothing else would explain why.
fn usable_name(ui: &Ui, server: &SharedServer) -> String {
    let sanitised = store::sanitise_name(&server.name);
    let usable = if sanitised.is_empty()
        || sanitised
            .to_ascii_lowercase()
            .starts_with(store::DISPLAY_PREFIX)
    {
        // Derived from the address rather than invented, so it is the same
        // string on every run and on every laptop — a name that moved between
        // runs would fork one server into two records. `allocate_name` is given
        // no taken list on purpose: a collision here is two rows a developer
        // has to tell apart, which the `shared-` prefix already handles, while
        // an unstable name is a row nobody can forget.
        store::allocate_name(&server.host, &[])
    } else {
        sanitised
    };

    if usable != server.name {
        ui.warn(&format!(
            "One of the team's servers is listed as {:?}, which riabuild cannot use as a name. \
             It is shown here as {}{usable}.",
            server.name,
            store::DISPLAY_PREFIX
        ));
    }
    usable
}

/// Whether anything in the store is one of the team's servers that this run
/// could not refresh — a server the leads removed, or a fetch that failed.
///
/// `remote list` shows these, marked, and `remote forget` accepts them: the
/// session recorded beside a removed server may still be live, and a row
/// nobody can see is a row nobody can clear.
pub fn has_stale(store: &Store) -> bool {
    store
        .remotes
        .iter()
        .any(|record| record.origin() == Origin::Stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::record_for;

    fn server(id: &str, name: &str, host: &str) -> SharedServer {
        SharedServer {
            id: id.into(),
            name: name.into(),
            host: host.into(),
            port: 22,
            user: "ada".into(),
        }
    }

    fn remote(name: &str, host: &str) -> Remote {
        Remote {
            name: name.into(),
            host: host.into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// A `Ui` whose warnings a test can read back. `reconcile` only ever
    /// speaks when it had to change a name.
    fn ui() -> Ui {
        Ui::new(false)
    }

    /// A store holding one of the team's servers, as a previous run left it:
    /// on disk, with state, and *not* refreshed by anything yet.
    fn saved_shared(id: &str, name: &str, host: &str) -> Store {
        let mut store = Store::default();
        let mut record = record_for(&remote(name, host));
        record.shared_id = id.to_string();
        record.session_id = "sess_1".into();
        record.home = "/home/ada".into();
        record.last_used_at = 1_000;
        store.remotes.push(record);
        store
    }

    #[test]
    fn a_server_this_laptop_has_never_seen_arrives_with_empty_state() {
        let mut store = Store::default();

        let superseded = reconcile(&ui(), &mut store, &[server("k1", "gpu", "gpu.internal")]);

        assert!(superseded.is_empty());
        assert_eq!(store.remotes.len(), 1);
        let record = &store.remotes[0];
        assert_eq!(record.display_name(), "shared-gpu");
        assert_eq!(record.origin(), Origin::Shared);
        assert!(record.session_id.is_empty());
        assert_eq!(record.last_used_at, 0);
    }

    #[test]
    fn a_known_server_keeps_its_session_and_takes_the_servers_address() {
        // The split this whole feature turns on: the address is the team's and
        // is replaced, the session is this laptop's and is not.
        let mut store = saved_shared("k1", "gpu", "gpu.internal");

        reconcile(&ui(), &mut store, &[server("k1", "gpu", "gpu.internal")]);

        let record = &store.remotes[0];
        assert_eq!(record.session_id, "sess_1");
        assert_eq!(record.home, "/home/ada");
        assert_eq!(record.last_used_at, 1_000);
        assert_eq!(record.origin(), Origin::Shared);
    }

    #[test]
    fn a_renamed_server_keeps_its_session_because_a_name_is_not_an_identity() {
        let mut store = saved_shared("k1", "gpu", "gpu.internal");

        let superseded = reconcile(
            &ui(),
            &mut store,
            &[server("k1", "trainer", "gpu.internal")],
        );

        assert!(superseded.is_empty(), "a rename is not a new machine");
        assert_eq!(store.remotes[0].display_name(), "shared-trainer");
        assert_eq!(store.remotes[0].session_id, "sess_1");
    }

    #[test]
    fn an_edited_address_hands_back_the_old_copy_and_starts_the_new_one_clean() {
        // An address *is* an identity — `Remote::hash` is taken over
        // user@host:port — so this is a different machine wearing the same
        // name, and the session recorded here belongs to the old one.
        let mut store = saved_shared("k1", "gpu", "gpu.internal");
        let was = store.remotes[0].hash.clone();

        let superseded = reconcile(&ui(), &mut store, &[server("k1", "gpu", "gpu-2.internal")]);

        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0].host, "gpu.internal");
        assert_eq!(superseded[0].hash, was);
        assert_eq!(
            superseded[0].session_id, "sess_1",
            "the caller has to be able to revoke it"
        );

        let record = &store.remotes[0];
        assert_eq!(record.host, "gpu-2.internal");
        assert_ne!(record.hash, was);
        assert!(
            record.session_id.is_empty() && record.home.is_empty(),
            "a session minted for the old machine must not be claimed for the new one"
        );
    }

    #[test]
    fn a_server_the_leads_removed_is_left_alone_and_goes_out_of_reach() {
        let mut store = saved_shared("k1", "gpu", "gpu.internal");

        reconcile(&ui(), &mut store, &[]);

        assert_eq!(store.remotes.len(), 1, "its session may still be live");
        assert_eq!(store.remotes[0].origin(), Origin::Stale);
        assert!(store.reachable().is_empty());
        assert!(has_stale(&store));
        // Still findable by name, which is what makes it forgettable.
        assert!(store.find("shared-gpu").is_some());
    }

    #[test]
    fn a_failed_fetch_leaves_every_shared_server_out_of_reach() {
        // `fetch` answers with an empty list when riabuild-web cannot be
        // reached, and this is what that means downstream: the addresses on
        // disk are memories, and nothing may connect to a memory.
        let mut store = saved_shared("k1", "gpu", "gpu.internal");
        store.remotes.push(record_for(&remote("mine", "mine.dev")));

        reconcile(&ui(), &mut store, &[]);

        let reachable: Vec<String> = store
            .reachable()
            .iter()
            .map(|record| record.display_name())
            .collect();
        assert_eq!(reachable, vec!["mine".to_string()]);
    }

    #[test]
    fn a_local_server_is_never_touched_by_a_fetch() {
        let mut store = Store::default();
        store.remotes.push(record_for(&remote("gpu", "gpu.local")));

        reconcile(&ui(), &mut store, &[server("k1", "gpu", "gpu.internal")]);

        assert_eq!(store.remotes.len(), 2, "two servers, both called gpu");
        assert_eq!(store.remotes[0].origin(), Origin::Local);
        assert_eq!(store.remotes[0].host, "gpu.local");
        assert_eq!(store.remotes[0].display_name(), "gpu");
        assert_eq!(store.remotes[1].display_name(), "shared-gpu");
    }

    #[test]
    fn a_second_run_does_not_add_the_same_server_twice() {
        let mut store = Store::default();
        let servers = [server("k1", "gpu", "gpu.internal")];

        reconcile(&ui(), &mut store, &servers);
        reconcile(&ui(), &mut store, &servers);

        assert_eq!(store.remotes.len(), 1);
    }

    #[test]
    fn the_row_id_is_what_a_record_is_matched_by_not_its_name() {
        // Which is why it has to be the riabuild-web row id: a name and an
        // address can both be edited, and matching on either would strand this
        // laptop's session under a record nothing looks at again.
        let mut store = saved_shared("k1", "gpu", "gpu.internal");

        reconcile(
            &ui(),
            &mut store,
            &[server("k1", "trainer", "gpu-2.internal")],
        );

        assert_eq!(store.remotes.len(), 1, "{:?}", store.names());
        assert_eq!(store.remotes[0].shared_id, "k1");
    }

    /// I009. `Record::name` becomes `RIABUILD_REMOTE=<name>` inside the
    /// single-quoted `env …` prefix every remote invocation is wrapped in, and
    /// it is the key `find`, `persist_one` and `forget_one` all match on. A
    /// locally typed name is reduced to what a shell word and a lookup key can
    /// both carry; a server-supplied one used to be copied across untouched.
    #[test]
    fn a_name_from_riabuild_web_is_held_to_the_rule_a_typed_one_is() {
        let ui = ui();
        let mut store = Store::default();

        reconcile(
            &ui,
            &mut store,
            &[server("k1", "gpu box'; rm -rf /", "gpu.internal")],
        );

        let name = &store.remotes[0].name;
        assert!(
            !name.contains(['\'', ' ', ';', '/']),
            "{name} would not survive being quoted into a command"
        );
        assert_eq!(store.remotes[0].display_name(), "shared-gpuboxrm-rf");
        assert!(
            ui.warned()
                .iter()
                .any(|warning| warning.contains("cannot use as a name")),
            "a name the developer will not recognise has to be explained: {:?}",
            ui.warned()
        );
    }

    #[test]
    fn a_name_already_wearing_the_display_prefix_does_not_get_a_second_one() {
        // Reserved: the prefix is how the team's servers are shown, so a bare
        // name carrying it would render as `shared-shared-gpu` and collide
        // with whatever `shared-gpu` already means.
        let mut store = Store::default();

        reconcile(
            &ui(),
            &mut store,
            &[server("k1", "shared-gpu", "gpu.internal")],
        );

        assert_eq!(store.remotes[0].display_name(), "shared-gpu");
        assert_eq!(store.remotes[0].name, "gpu");
    }

    #[test]
    fn a_name_with_nothing_usable_in_it_falls_back_to_the_address_not_to_nothing() {
        // An empty name is worse than a wrong one: `RIABUILD_REMOTE=` on the
        // wire, and a lookup key nothing can type. The fallback is derived
        // from the address so it is the same string on every run — a name that
        // moved between runs would fork one server into two records.
        let mut store = Store::default();
        let servers = [server("k1", "🚀🚀", "gpu-7.internal")];

        reconcile(&ui(), &mut store, &servers);
        let first = store.remotes[0].name.clone();
        reconcile(&ui(), &mut store, &servers);

        assert_eq!(first, "gpu-7");
        assert_eq!(store.remotes.len(), 1, "{:?}", store.names());
        assert_eq!(store.remotes[0].name, first);
    }

    #[test]
    fn an_ordinary_name_is_passed_through_and_says_nothing() {
        let ui = ui();
        let mut store = Store::default();

        reconcile(
            &ui,
            &mut store,
            &[server("k1", "gpu.eu_west-2", "gpu.internal")],
        );

        assert_eq!(store.remotes[0].name, "gpu.eu_west-2");
        assert!(ui.warned().is_empty(), "{:?}", ui.warned());
    }
}

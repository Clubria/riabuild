//! One command on the server, and the one answer the rest of the run needs
//! before anything else can be addressed: where this developer's home is.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use riabuild_paths::Paths;
use riabuild_runner::{CommandOutput, CommandRunner};
use riabuild_ui::Failure;

use crate::{Remote, issued, shell_command, ssh, store};

/// One command on the server, through the key riabuild owns for it.
pub async fn ssh_once(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    command: &str,
    carry: Option<&issued::Working>,
) -> Result<CommandOutput> {
    ssh::Ssh::to(remote, paths, runner)
        .carry(carry)
        .run(command)
        .await
}

/// The server's own home directory, asked for once and cached on the store
/// entry from then on.
///
/// Everything downstream of this uses the absolute string it returns,
/// never `~`: a `~` is only a home directory to a shell willing to expand
/// it, mosh runs commands with no shell at all, and an unexpanded `~` that
/// reached `paths::root_for` would be refused outright rather than
/// silently collapsing every developer on the box into one namespace.
///
/// **Caches in memory; never writes `remotes.json` itself.** This is the one
/// step `riabuild remote --check` runs that reaches the server, and a
/// `store.save` here made a read-only probe persist a full record — name,
/// host, port, user — for a machine the developer had only asked riabuild to
/// look at, which then read back from `riabuild remote list` as a server they
/// had set up. Persisting is left to the callers that know whether this run is
/// read-only: `flow::connect_and_setup` saves either side of this call on the
/// non-`--check` path — before `authorise`, which can modify the server, and
/// again here, because `forget`'s server-side cleanup needs the home this
/// resolved — and `session::ensure` and `store::remember` save again later.
pub async fn resolve_home(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    store: &mut store::Store,
    carry: Option<&issued::Working>,
) -> Result<String> {
    if let Some(record) = store.find(&remote.name)
        && !record.home.is_empty()
    {
        return Ok(record.home.clone());
    }

    let output = ssh_once(
        remote,
        paths,
        runner,
        &shell_command("printf %s \"$HOME\""),
        carry,
    )
    .await?;
    let home = output.trimmed().to_string();
    if !output.ok() || !home.starts_with('/') {
        return Err(Failure::new(
            format!("asking {} where your home directory is", remote.host),
            "Check that you can `ssh` to that server yourself, then run `riabuild remote` again.",
        )
        .detail(output.stderr)
        .into());
    }

    // `find_mut`, not a match on the bare `name`. `remote.name` is the
    // *display* name — `shared-gpu` for one of the team's servers — while the
    // record holds the bare `gpu` with a `sharedId` beside it, so a bare-name
    // match silently found nothing for every shared server and the home was
    // never cached. `forget`'s server-side cleanup builds its paths out of
    // this field and skips entirely when it is empty, which left the key line
    // and the namespace on every shared server for ever.
    let Some(record) = store.find_mut(&remote.name) else {
        // Not a warning to swallow: every caller of this reaches it through
        // `store::choose`, which either found a record or added one, so a miss
        // here means the name the flow is carrying no longer names the record
        // it is about — and the next `persist_one` would drop this run's work
        // on the floor exactly as quietly.
        return Err(anyhow!(
            "riabuild has no saved record for \"{}\", so there is nowhere to remember its home directory",
            remote.name
        ));
    };
    record.home = home.clone();
    Ok(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_fixture as remote;

    #[tokio::test]
    async fn the_servers_home_is_asked_for_once_and_remembered() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake =
            Arc::new(riabuild_runner::FakeRunner::new().containing("printf", 0, "/home/dev\n", ""));
        let mut store = store::Store::default();
        store.remotes.push(store::record_for(&remote()));

        let first = resolve_home(&remote(), &paths, fake.clone(), &mut store, None)
            .await
            .expect("asks");
        assert_eq!(first, "/home/dev");
        assert_eq!(store.remotes[0].home, "/home/dev");

        let second = resolve_home(&remote(), &paths, fake.clone(), &mut store, None)
            .await
            .expect("cached");
        assert_eq!(second, "/home/dev");
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|call| call.contains("printf"))
                .count(),
            1,
            "the second call must come from the record, not the server"
        );
        // Cached on the record, not written out: this is the only step
        // `riabuild remote --check` runs that reaches the server, and a save
        // here persisted a full record for a machine that had only been
        // probed. `session::ensure` and `store::remember` are what mean it.
        assert!(
            !paths.remotes_file().exists(),
            "resolving a home must not write remotes.json"
        );
    }

    #[tokio::test]
    async fn a_tilde_home_is_refused_rather_than_sent_to_root_for() {
        // This is the R1 mechanism: `paths::root_for` refuses a non-absolute
        // override rather than defaulting, so a `~` that reached it would
        // hard-error there instead of silently collapsing every developer on
        // a shared box into one namespace. `resolve_home` must catch it
        // first, with an actionable message, rather than caching a `~` that
        // later commands would carry unexpanded.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(riabuild_runner::FakeRunner::new().containing("printf", 0, "~\n", ""));
        let mut store = store::Store::default();
        store.remotes.push(store::record_for(&remote()));

        let err = resolve_home(&remote(), &paths, fake, &mut store, None)
            .await
            .expect_err("a `~` is not an absolute path");
        assert!(
            err.downcast_ref::<Failure>().is_some(),
            "must be the actionable Failure, not a generic error: {err}"
        );
        assert!(
            store.remotes[0].home.is_empty(),
            "a refused home must not be cached"
        );
    }

    #[tokio::test]
    async fn a_relative_home_is_refused_rather_than_sent_to_root_for() {
        // The other shape a non-absolute `$HOME` can take: no leading `/` and
        // no `~` either, just a bare relative path.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(riabuild_runner::FakeRunner::new().containing(
            "printf",
            0,
            "relative/path\n",
            "",
        ));
        let mut store = store::Store::default();
        store.remotes.push(store::record_for(&remote()));

        let err = resolve_home(&remote(), &paths, fake, &mut store, None)
            .await
            .expect_err("a relative path is not an absolute path");
        assert!(
            err.downcast_ref::<Failure>().is_some(),
            "must be the actionable Failure, not a generic error: {err}"
        );
    }

    #[tokio::test]
    async fn one_of_the_teams_servers_remembers_its_home_under_the_name_it_is_reached_by() {
        // I034. `Remote::from(&Record)` carries the *display* name, so
        // `remote.name` here is `shared-build-01` while the record's own
        // `name` is the bare `build-01`. A write-back matching on the bare
        // field found nothing and said nothing, so `home` stayed empty on
        // every one of the team's servers — and `forget`'s server-side
        // cleanup, which builds its paths out of `home` and returns early
        // when it is empty, left the namespace and the key line behind for
        // ever.
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(laptop.path());
        let fake =
            Arc::new(riabuild_runner::FakeRunner::new().containing("printf", 0, "/home/dev\n", ""));
        let mut store = store::Store::default();
        store
            .remotes
            .push(store::shared_record_for(&remote(), "k17abc"));
        let shared: Remote = (&store.remotes[0]).into();
        assert_eq!(shared.name, "shared-build-01");

        let home = resolve_home(&shared, &paths, fake.clone(), &mut store, None)
            .await
            .expect("asks the server");

        assert_eq!(home, "/home/dev");
        assert_eq!(
            store.remotes[0].home, "/home/dev",
            "the answer has to land on the record, or forget can never reach in to clean up"
        );

        // …and the second call comes from the record rather than the server,
        // which is the observable half of the same fix.
        resolve_home(&shared, &paths, fake.clone(), &mut store, None)
            .await
            .expect("cached");
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|call| call.contains("printf"))
                .count(),
            1,
            "{:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn a_home_with_no_record_to_remember_it_on_is_an_error_not_a_shrug() {
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(laptop.path());
        let fake =
            Arc::new(riabuild_runner::FakeRunner::new().containing("printf", 0, "/home/dev\n", ""));
        let mut store = store::Store::default();

        let error = resolve_home(&remote(), &paths, fake, &mut store, None)
            .await
            .expect_err("nothing would have remembered this");
        assert!(error.to_string().contains("build-01"), "{error}");
    }
}

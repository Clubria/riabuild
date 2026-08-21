//! Lending the laptop's GitHub sign-in to a server session.
//!
//! Split out of `session.rs` rather than added there: `session.rs` mints and
//! writes the riabuild session itself, a distinct concern from handing over a
//! *GitHub* credential, and growing `session.rs` past this file's own weight
//! would have pushed it over the ~300-line production budget every file in
//! this crate is held to (see `riabuild-cli/CLAUDE.md`'s "one task per file").
//! `keychain.rs` and `install.rs` were the cautionary examples when this was
//! written; both have since been split for the same reason, which is the
//! argument for doing it up front rather than after a file has grown.

use super::Remote;
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::{CommandRunner, RunOptions};
use riabuild_ui::Ui;
use std::sync::Arc;

/// Hands the laptop's GitHub sign-in to the server for this session.
///
/// Never fatal. If the laptop has no usable token, `github_cli` on the server
/// signs in for itself over the TTY that setup already has — the fallback the
/// task always had.
///
/// The caller (Task 21's orchestration) must run `internal gh-sweep` on
/// `remote` before calling this, not after. Each SSH invocation — the sweep,
/// this seed, the setup run, the shell — is its own separate process, and
/// `gh_session`'s design (see that module's doc) only lets the interactive
/// environment shell hold a live marker; every other invocation, this one
/// included, only ever `attach`es. An earlier draft got that backwards: it
/// had the seeding run itself claim a marker, so its own exit found no other
/// marker yet and wiped the credential it had just written, milliseconds
/// before the setup run ever saw it. Sweeping first — rather than after —
/// only ever clears a *dead* session's leftovers, never anything this write
/// is about to depend on.
pub async fn seed_github(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    riabuild_path: &str,
    carry: Option<&crate::issued::Working>,
) -> Result<()> {
    // The `gh` riabuild owns on *this laptop*, by absolute path, for the same
    // reason every other call site uses one: `~/.riabuild/bin` is not on `PATH`
    // during provisioning, so a bare `gh` reads whatever the developer happens
    // to have — a different install, with a different `hosts.yml`, holding a
    // sign-in that is not the one riabuild's own tasks verified.
    let gh = paths
        .tool_dir("gh", riabuild_fetch::tools::GH_VERSION)
        .join(riabuild_fetch::tools::GH_MEMBER)
        .to_string_lossy()
        .into_owned();
    let token = runner
        .run(&gh, &["auth", "token"], &RunOptions::default())
        .await?;
    if !token.ok() || token.trimmed().is_empty() {
        ui.note("This laptop has no GitHub sign-in to lend; the server will sign in itself.");
        return Ok(());
    }

    // Said out loud because this hop is no longer instant: on a server that has
    // never been set up, the far side installs `gh` before it can be handed
    // anything, and that is a download with its output captured over SSH. Silent
    // for several seconds reads as riabuild having hung.
    ui.note("Lending this laptop's GitHub sign-in to the server…");

    let seeded = crate::ssh::Ssh::to(remote, paths, runner.clone())
        .carry(carry)
        // On stdin, never in argv: `ps` is readable by everyone, and on a
        // shared server that means every other developer's session too.
        .stdin(token.trimmed().as_bytes().to_vec())
        .run(&format!("{riabuild_path} internal seed-github"))
        .await?;
    if !seeded.ok() {
        // With the reason, not without it. This note used to be the only trace
        // a failed lend left, and it named no cause — so the seed silently
        // failing on every first run against a new server (`gh` was not
        // installed yet) looked exactly like a network blip, for as long as
        // nobody read the code. A degradation still is not fatal, but it has
        // to be diagnosable from the terminal it happened in.
        ui.note("Could not lend the server your GitHub sign-in; it will sign in itself.");
        let detail = seeded.stderr.trim();
        if !detail.is_empty() {
            ui.note(detail);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_runner::FakeRunner;

    /// The absolute `gh` `seed_github` will run for these `Paths`.
    fn gh_path(paths: &dyn Paths) -> String {
        paths
            .tool_dir("gh", riabuild_fetch::tools::GH_VERSION)
            .join(riabuild_fetch::tools::GH_MEMBER)
            .to_string_lossy()
            .into_owned()
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
    async fn the_token_is_piped_and_never_put_in_an_argument_list() {
        // Arguments are world-readable through `ps`. This is the same assertion
        // `keychain/platform.rs` already makes about `secret-tool`.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        // `FakeRunner` matches on an invocation prefix, so the stub has to
        // carry the same absolute path the code resolves — a bare "gh" here
        // would never match, and the call would fall through to the default.
        let gh = gh_path(&paths);
        let fake = Arc::new(
            FakeRunner::new()
                .with(&format!("{gh} auth token"), 0, "gho_super_secret\n", "")
                .with("ssh", 0, "", ""),
        );

        seed_github(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "~/.riabuild/riabuild/v/riabuild",
            None,
        )
        .await
        .expect("seeds");

        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.contains("gho_super_secret")),
            "{:?}",
            fake.calls()
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.contains("internal seed-github")),
            "{:?}",
            fake.calls()
        );
        // "piped" is the half `calls()` cannot see. Without this, deleting
        // `stdin: Some(…)` above still passes: the token is absent from argv
        // and `internal seed-github` still runs — it just reads EOF from
        // `tokio::io::stdin()` and hands `gh auth login --with-token` an empty
        // token. Note the trailing newline from `gh auth token` must already
        // be gone: `gh` rejects a token with one.
        assert_eq!(
            fake.stdin_text_of("ssh").as_deref(),
            Some("gho_super_secret"),
            "the token must actually be piped to the server's riabuild"
        );
    }

    #[tokio::test]
    async fn a_laptop_with_no_gh_sign_in_does_not_stop_the_run() {
        // The server's own device-code sign-in is the fallback, and it costs no new
        // code: github_cli's check finds gh signed out and its apply signs in over
        // the TTY that setup already has.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with(
            &format!("{} auth token", gh_path(&paths)),
            1,
            "",
            "not logged in",
        ));

        seed_github(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            "riabuild",
            None,
        )
        .await
        .expect("must not fail the run");
        assert!(!fake.calls().iter().any(|call| call.starts_with("ssh")));
    }
}

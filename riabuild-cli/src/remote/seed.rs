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

use super::{Remote, identity};
use crate::paths::Paths;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::Ui;
use anyhow::Result;
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
) -> Result<()> {
    // The `gh` riabuild owns on *this laptop*, by absolute path, for the same
    // reason every other call site uses one: `~/.riabuild/bin` is not on `PATH`
    // during provisioning, so a bare `gh` reads whatever the developer happens
    // to have — a different install, with a different `hosts.yml`, holding a
    // sign-in that is not the one riabuild's own tasks verified.
    let gh = paths
        .tool_dir("gh", crate::tools::GH_VERSION)
        .join(crate::tools::GH_MEMBER)
        .to_string_lossy()
        .into_owned();
    let token = runner
        .run(&gh, &["auth", "token"], &RunOptions::default())
        .await?;
    if !token.ok() || token.trimmed().is_empty() {
        ui.note("This laptop has no GitHub sign-in to lend; the server will sign in itself.");
        return Ok(());
    }

    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(format!("{riabuild_path} internal seed-github"));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let seeded = runner
        .run(
            "ssh",
            &refs,
            &RunOptions {
                // On stdin, never in argv: `ps` is readable by everyone, and
                // on a shared server that means every other developer's
                // session too.
                stdin: Some(token.trimmed().as_bytes().to_vec()),
                ..super::askpass::run_options(remote, paths)
            },
        )
        .await?;
    if !seeded.ok() {
        ui.note("Could not lend the server your GitHub sign-in; it will sign in itself.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    /// The absolute `gh` `seed_github` will run for these `Paths`.
    fn gh_path(paths: &dyn Paths) -> String {
        paths
            .tool_dir("gh", crate::tools::GH_VERSION)
            .join(crate::tools::GH_MEMBER)
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
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
        let paths = crate::paths::RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with(
            &format!("{} auth token", gh_path(&paths)),
            1,
            "",
            "not logged in",
        ));

        seed_github(&remote(), &paths, fake.clone(), &Ui::new(true), "riabuild")
            .await
            .expect("must not fail the run");
        assert!(!fake.calls().iter().any(|call| call.starts_with("ssh")));
    }
}

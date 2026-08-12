//! The riabuild session a server runs on.
//!
//! Minted by the laptop, labelled after the server so the dashboard lists it as
//! its own revocable device, and written to the server's namespace at 0600 —
//! the one amendment to "no secrets in ~/.riabuild", argued in the design.

use super::{Remote, identity, shell_command, shell_quote};
use crate::api::{ApiClient, Member, auth};
use crate::keychain;
use crate::paths::{Paths, RealPaths};
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

/// The namespace as a string, for a remote command line.
///
/// Delegates to `paths::remote_namespace` rather than formatting its own copy:
/// this value is what `forget` hands to `rm -rf`, and two spellings of one
/// layout is exactly the drift that makes that dangerous.
///
/// Absolute, never `~`: `mosh`, `fish`, and `csh` do not expand a `~` in the
/// positions remote mode uses it, and an unexpanded one reaching
/// `paths::root_for` is refused outright rather than defaulting (R1 in
/// `decisions.md` — this file's own interface line and test used to say
/// otherwise; both were stale).
pub fn namespace(home: &str, member_id: &str) -> String {
    crate::paths::remote_namespace(Path::new(home), member_id)
        .to_string_lossy()
        .into_owned()
}

/// A `Paths` view of `member_id`'s namespace on a server with home `home`.
///
/// Exists so a file this module writes into that namespace — `owner.json`
/// today — has its basename read out of the one shared layout definition in
/// `paths.rs` rather than formatted a second time here (R10 in
/// `decisions.md`). `RealPaths::with_root` is exactly the mechanism `paths.rs`
/// documents for evaluating that layout against a remote home instead of this
/// laptop's own.
fn remote_layout(home: &str, member_id: &str) -> RealPaths {
    RealPaths::with_root(
        home,
        crate::paths::remote_namespace(Path::new(home), member_id),
    )
}

/// The final path component of `path`, or an empty string.
///
/// Never panics: every `Paths` layout method joins a literal onto a root, so
/// the `None` arm is unreachable in practice, but a filename is worth reading
/// out of one place (`paths.rs`) rather than asserting it can't fail.
fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Writes one file into the namespace, through a shell riabuild names and with
/// every path quoted. The bytes go on stdin so a secret never reaches argv.
///
/// Calls `runner.run` directly rather than `ssh_once`: `ssh_once` always runs
/// with `RunOptions::default()`, which carries no stdin, so a write routed
/// through it would open the remote `cat` against a closed pipe and produce an
/// empty file instead of `contents`.
async fn write_into_namespace(
    remote: &Remote,
    paths: &dyn Paths,
    runner: &Arc<dyn CommandRunner>,
    ns: &str,
    name: &str,
    contents: Vec<u8>,
) -> Result<()> {
    let target = format!("{ns}/{name}");
    let script = shell_command(&format!(
        "umask 077 && mkdir -p {ns} && cat > {target} && chmod 600 {target}",
        ns = shell_quote(ns),
        target = shell_quote(&target),
    ));
    let mut args = identity::ssh_options(remote, paths, true);
    args.push(remote.target());
    args.push(script);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = runner
        .run(
            "ssh",
            &refs,
            &RunOptions {
                stdin: Some(contents),
                ..super::askpass::run_options(remote, paths)
            },
        )
        .await?;
    if !output.ok() {
        return Err(Failure::new(
            format!("writing {name} on {}", remote.host),
            "Check there is space in your home directory on that server, then run `riabuild remote` again.",
        )
        .detail(output.stderr)
        .into());
    }
    Ok(())
}

/// Who a namespace belongs to, for whoever has a shell on the box and finds a
/// directory named after a UUID.
///
/// Through `serde_json`, not `format!`: the name is whatever the developer
/// typed into their profile, and one containing a quote or a backslash would
/// otherwise produce a file riabuild cannot read back when it names the other
/// people sharing an account.
pub fn owner_json(login: &str, name: &str, email: &str) -> String {
    serde_json::json!({ "githubLogin": login, "name": name, "email": email }).to_string()
}

/// The git identity for this namespace.
///
/// `GIT_CONFIG_GLOBAL` makes git stop reading `~/.gitconfig` altogether, so
/// setting that variable without writing this file is worse than doing
/// neither: the first commit on the server fails with "Please tell me who you
/// are", on a box where the developer never configured git in the first
/// place.
pub fn gitconfig(name: &str, email: &str) -> String {
    format!("[user]\n\tname = {name}\n\temail = {email}\n")
}

/// How long a session this module mints is good for.
///
/// Mirrors the 90 days riabuild-web itself mints a CLI session for (the same
/// number `tasks::login::SESSION_TTL_MS` mirrors, for the laptop's own
/// session) — not a value this file is free to choose, since a shorter one
/// here would only make `ensure` re-mint more often than the server actually
/// requires, and a longer one would let it believe a token is live well past
/// the point the server has stopped honouring it.
const SESSION_TTL_MS: u64 = 90 * 24 * 60 * 60 * 1000;

/// Whether the store's record of this session's expiry is recent enough that
/// probing the server for liveness is worth the round trip at all.
///
/// Split out from `ensure` so this decision — the only part of "is a cached
/// token worth reusing" that does not need a real riabuild-web to answer — is
/// unit-testable. The round trip itself (`ApiClient::me`) is not: this crate's
/// test scaffolding (`testing.rs`) has never stood up a fake riabuild-web, the
/// same reason `tasks::login`'s own `apply()` — which drives the identical
/// browser-login flow this calls into — has no test of its own beyond
/// `check()`.
fn expires_soon(record: &super::store::Record) -> bool {
    record.session_expires_at <= crate::config::now_millis()
}

/// Mints (or reuses) the session a server's own riabuild runs as, and writes
/// it — with a git identity and an owner label — into that developer's
/// namespace on `remote`.
///
/// `store` is threaded through rather than owned: `resolve_home` (below) reads
/// and writes it to cache the server's home directory, and minting a fresh
/// session records its expiry there too, under the same entry, so a second
/// `ensure` for the same server finds both without asking again.
///
/// Called from `remote::flow::connect_and_setup`, which is `riabuild remote`'s
/// real orchestration and the only production caller.
#[allow(clippy::too_many_arguments)]
pub async fn ensure(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    api: &ApiClient,
    member: &Member,
    // Only for the `ApiClient` that probes a cached token below. There is no
    // `web_url` beside it any more: `auth::login` builds no dashboard link, the
    // server returns the verification URL because it is the thing that knows
    // where the dashboard is deployed.
    version: &str,
    store: &mut super::store::Store,
) -> Result<()> {
    let home = super::resolve_home(remote, paths, runner.clone(), store).await?;

    // The laptop's own cache of this server's session, kept under an account
    // named for the server so several servers never collide on one laptop
    // keychain entry, and revoking one server's session can never sign the
    // laptop itself out. Never `RIABUILD_TOKEN`: that is this machine's
    // override, and honouring it here would hand every server the same token.
    let keychain = keychain::for_account(
        runner.clone(),
        &keychain::remote_account(&remote.hash()),
        None,
    );

    // A stored token is not automatically a live one. It expires, and
    // `forget` on another laptop may have revoked it. Writing a dead token to
    // the server strands whoever lands on it: the server's own riabuild 401s,
    // and while the device-code flow *can* now sign in over SSH, doing so from
    // the server would mint a session nothing on this laptop recorded — so no
    // `riabuild remote forget` could ever revoke it again.
    let record = store.find(&remote.name).cloned();
    let usable = match (keychain.get().await?, record.as_ref()) {
        (Some(token), Some(record)) if !expires_soon(record) => {
            let mut probe = ApiClient::new(version);
            probe.set_token(Some(token.clone()));
            probe.me().await.is_ok().then_some(token)
        }
        _ => None,
    };

    let token = match usable {
        Some(token) => token,
        None => {
            ui.heading(&format!("Signing {} in to riabuild", remote.name));
            ui.note("Approve it in your browser.");
            // Laptop's browser, server's hostname as the label: the dashboard
            // lists this session as its own revocable device, distinct from
            // the laptop's. The heading above is why `auth::login` prints none
            // of its own — "Signing this machine in" would be a lie here.
            //
            // The `member` the grant carries is dropped on purpose: it
            // describes the developer approving in the browser, who is the
            // same person `ensure` was already handed as `member`.
            let auth::Session {
                token, session_id, ..
            } = auth::login(api, runner.as_ref(), ui, &remote.host).await?;
            keychain.set(&token).await?;
            // Recorded so the check above can skip the round trip next time,
            // and so `riabuild remote list` can show it — which requires
            // actually saving the store, not just mutating it in memory.
            // `session_id` is what lets `riabuild remote forget` name this
            // exact session when it revokes it through
            // `DELETE /api/v1/cli/sessions/<id>` — see `forget::forget_remote`.
            if let Some(saved) = store.remotes.iter_mut().find(|r| r.name == remote.name) {
                saved.session_expires_at = crate::config::now_millis() + SESSION_TTL_MS;
                saved.session_id = session_id;
            }
            store.save(paths).await?;
            token
        }
    };

    let ns = namespace(&home, &member.member_id);
    let layout = remote_layout(&home, &member.member_id);

    let session_token_name = basename(&layout.session_token_file());
    write_into_namespace(
        remote,
        paths,
        &runner,
        &ns,
        &session_token_name,
        token.into_bytes(),
    )
    .await?;

    // The git identity. Nothing else writes this file, and GIT_CONFIG_GLOBAL
    // pointing at a file that does not exist is what makes `git commit` fail.
    let identity = gitconfig(&member.display_name(), &member.email);
    write_into_namespace(
        remote,
        paths,
        &runner,
        &ns,
        "gitconfig",
        identity.into_bytes(),
    )
    .await?;

    let owner = owner_json(&member.github_login, &member.display_name(), &member.email);
    let owner_name = basename(&layout.owner_file());
    write_into_namespace(remote, paths, &runner, &ns, &owner_name, owner.into_bytes()).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    #[test]
    fn a_namespace_is_named_after_the_immutable_id_and_is_never_a_tilde() {
        // Not the login: a GitHub rename would otherwise orphan a developer's
        // whole environment and silently re-provision them from scratch. And
        // absolute, per R1: a `~` reaching the server is either expanded by
        // some shells and not others, or refused outright by `paths::root_for`.
        let ns = namespace("/home/dev", "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            ns,
            "/home/dev/.riabuild-remote/550e8400-e29b-41d4-a716-446655440000"
        );
        assert!(!ns.contains('~'), "{ns}");
    }

    #[test]
    fn the_owner_file_says_who_this_is_in_words() {
        let json = owner_json("ada", "Ada Lovelace", "ada@clubria.dev");
        assert!(json.contains("\"githubLogin\":\"ada\""), "{json}");
        assert!(json.contains("Ada Lovelace"), "{json}");
        // No secret ever goes in here: it is a label, readable by everyone who
        // shares the account.
        assert!(!json.contains("token"), "{json}");
    }

    #[test]
    fn a_quote_in_a_name_does_not_produce_unreadable_json() {
        // The reason this goes through serde_json rather than format!: a
        // developer's profile name is not riabuild's to sanitise.
        let json = owner_json("ada", "Ada \"Countess\" Lovelace", "ada@clubria.dev");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["name"], "Ada \"Countess\" Lovelace");
    }

    #[test]
    fn the_gitconfig_names_who_committed() {
        let config = gitconfig("Ada Lovelace", "ada@clubria.dev");
        assert!(config.contains("name = Ada Lovelace"), "{config}");
        assert!(config.contains("email = ada@clubria.dev"), "{config}");
    }

    fn record_expiring_at(millis: u64) -> super::super::store::Record {
        let mut record = super::super::store::record_for(&remote());
        record.session_expires_at = millis;
        record
    }

    #[test]
    fn an_expired_record_is_not_worth_probing() {
        assert!(expires_soon(&record_expiring_at(0)));
        assert!(expires_soon(&record_expiring_at(1)));
    }

    #[test]
    fn a_record_expiring_well_in_the_future_is_worth_probing() {
        let far_future = crate::config::now_millis() + SESSION_TTL_MS;
        assert!(!expires_soon(&record_expiring_at(far_future)));
    }

    #[tokio::test]
    async fn a_write_carries_its_secret_on_stdin_never_in_the_command_line() {
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let fake = Arc::new(FakeRunner::new().containing("cat", 0, "", ""));

        write_into_namespace(
            &remote(),
            &paths,
            &(fake.clone() as Arc<dyn CommandRunner>),
            "/home/dev/.riabuild-remote/abc",
            "session.token",
            b"rb_live_secret_token".to_vec(),
        )
        .await
        .expect("writes");

        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.contains("rb_live_secret_token")),
            "the token must never appear in an argument list: {:?}",
            fake.calls()
        );
        assert!(
            fake.calls().iter().any(|call| call.contains("chmod 600")),
            "{:?}",
            fake.calls()
        );
        // The other half, and the half the two assertions above cannot see.
        // Deleting `stdin: Some(contents)` from `write_into_namespace` leaves
        // both of them green — the token is still absent from argv, `chmod
        // 600` still runs, `ssh` still exits 0 — while the remote `cat` reads
        // a closed pipe and the server gets a zero-byte `session.token`
        // reported as a success. That regression happened on this branch once
        // already; this assertion is what would have caught it.
        assert_eq!(
            fake.stdin_text_of("ssh").as_deref(),
            Some("rb_live_secret_token"),
            "the token must actually reach the remote `cat` on stdin"
        );
    }

    #[tokio::test]
    async fn a_failed_write_is_reported_with_an_actionable_next_step() {
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = crate::paths::RealPaths::rooted_at(laptop.path());
        let fake = Arc::new(FakeRunner::new().containing("cat", 1, "", "No space left on device"));

        let error = write_into_namespace(
            &remote(),
            &paths,
            &(fake as Arc<dyn CommandRunner>),
            "/home/dev/.riabuild-remote/abc",
            "session.token",
            b"token".to_vec(),
        )
        .await
        .expect_err("no space");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(failure.action.contains("space"), "{}", failure.action);
    }

    #[test]
    fn the_owner_file_basename_comes_from_the_shared_layout_not_a_second_literal() {
        // R10: the basename `ensure` writes under must be read out of
        // `Paths::owner_file` rather than hardcoded a second time here.
        let layout = remote_layout("/home/dev", "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(basename(&layout.owner_file()), "owner.json");
        assert_eq!(basename(&layout.session_token_file()), "session.token");
    }
}

//! Where a developer's things live on a server, and how a file gets there.
//!
//! One layout, computed by `paths::remote_namespace` rather than formatted
//! here: this value is what `forget` hands to `rm -rf`, and two spellings of
//! one layout is exactly the drift that makes that dangerous.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use riabuild_paths::{Paths, RealPaths};
use riabuild_runner::CommandRunner;
use riabuild_ui::Failure;

use crate::{Remote, shell_command, shell_quote};

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
    riabuild_paths::remote_namespace(Path::new(home), member_id)
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
pub(super) fn remote_layout(home: &str, member_id: &str) -> RealPaths {
    RealPaths::with_root(
        home,
        riabuild_paths::remote_namespace(Path::new(home), member_id),
    )
}

/// The final path component of `path`, or an empty string.
///
/// Never panics: every `Paths` layout method joins a literal onto a root, so
/// the `None` arm is unreachable in practice, but a filename is worth reading
/// out of one place (`paths.rs`) rather than asserting it can't fail.
pub(super) fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Writes one file into the namespace, through a shell riabuild names and with
/// every path quoted. The bytes go on stdin so a secret never reaches argv.
///
/// Goes through the `Ssh` builder's `stdin` rather than `ssh_once`, which
/// pipes nothing: a write routed through `ssh_once` would open the remote
/// `cat` against a closed pipe and produce an empty file instead of
/// `contents`.
pub(super) async fn write_into_namespace(
    remote: &Remote,
    paths: &dyn Paths,
    runner: &Arc<dyn CommandRunner>,
    ns: &str,
    name: &str,
    contents: Vec<u8>,
    carry: Option<&crate::issued::Working>,
) -> Result<()> {
    let target = format!("{ns}/{name}");
    let script = shell_command(&format!(
        "umask 077 && mkdir -p {ns} && cat > {target} && chmod 600 {target}",
        ns = shell_quote(ns),
        target = shell_quote(&target),
    ));
    let output = crate::ssh::Ssh::to(remote, paths, runner.clone())
        .carry(carry)
        .stdin(contents)
        .run(&script)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_fixture as remote;
    use riabuild_runner::FakeRunner;

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

    #[tokio::test]
    async fn a_write_carries_its_secret_on_stdin_never_in_the_command_line() {
        let laptop = tempfile::TempDir::new().expect("tempdir");
        let paths = riabuild_paths::RealPaths::rooted_at(laptop.path());
        let fake = Arc::new(FakeRunner::new().containing("cat", 0, "", ""));

        write_into_namespace(
            &remote(),
            &paths,
            &(fake.clone() as Arc<dyn CommandRunner>),
            "/home/dev/.riabuild-remote/abc",
            "session.token",
            b"rb_live_secret_token".to_vec(),
            None,
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
        let paths = riabuild_paths::RealPaths::rooted_at(laptop.path());
        let fake = Arc::new(FakeRunner::new().containing("cat", 1, "", "No space left on device"));

        let error = write_into_namespace(
            &remote(),
            &paths,
            &(fake as Arc<dyn CommandRunner>),
            "/home/dev/.riabuild-remote/abc",
            "session.token",
            b"token".to_vec(),
            None,
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

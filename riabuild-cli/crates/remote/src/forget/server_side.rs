//! The traces this developer left on the server itself.
//!
//! Their namespace, and the one line their key added to `authorized_keys`.
//! Never fatal: a server that happens to be unreachable must not become a
//! server nobody can ever forget, so every failure here is a warning and the
//! local delete still runs.

use std::sync::Arc;

use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::{CommandOutput, CommandRunner};
use riabuild_ui::Ui;

use super::api::Carries;
use crate::{Remote, identity, session, shell_command, shell_quote, ssh_once, store};

/// Step 2 of [`forget_remote`]: the namespace and the `authorized_keys` line
/// this developer's own key added, if either was ever created.
///
/// Never fails the caller: an unreachable server here is reported through
/// `ui.warn` and left for a human to notice, not propagated as an error that
/// would stop the local delete that follows it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn cleanup_server_side(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    carries: &dyn Carries,
    record: &store::Record,
    member_id: &str,
) {
    if record.home.is_empty() {
        // `resolve_home` never succeeded for this server — nothing was ever
        // installed on it to clean up.
        return;
    }

    let cleanup = cleanup_script(&record.home, member_id);

    let outcome = ssh_once(remote, paths, runner.clone(), &cleanup, None).await;
    if matches!(&outcome, Ok(output) if output.ok()) {
        return;
    }

    // riabuild's own key could not do it. On a managed SSH gateway that is not
    // an exception, it is the *expected* answer — such a box accepts the write
    // to `authorized_keys` and then authenticates against its own registry
    // regardless, which is precisely why issued keys exist. Hardcoding `None`
    // here meant `forget` could never authenticate on the servers that feature
    // is for, so it always warned and always left the namespace and the key
    // line behind, while `CLAUDE.md` claimed it "still has exactly one
    // developer's line to remove".
    //
    // Only on a refusal, never on an unreachable server: resolving an issued
    // identity costs a fetch, an `ssh-agent` and a probe per key, and a box
    // that is simply switched off would pay all of it to be told again that it
    // is switched off.
    if refused_us(&outcome)
        && let Some(carried) = carries.carry(remote, paths, runner.clone(), ui).await
    {
        let retry = ssh_once(remote, paths, runner, &cleanup, Some(&carried.working)).await;
        let ok = matches!(&retry, Ok(output) if output.ok());
        carried.stop().await;
        if ok {
            return;
        }
    }

    ui.warn(&format!(
        "Could not reach {}. Its riabuild namespace and authorized_keys line are still there.",
        remote.host
    ));
}

/// The namespace and this developer's own `authorized_keys` line, removed.
///
/// Matched on the member id, as a fixed string via `grep -vF`. On a shared
/// account every developer's key comment carries the same `user@host`, so
/// matching on that would delete Bob's and Carla's lines too and lock them out
/// of the box with no diagnostic anywhere. `sed` would also read the hostname's
/// dots as wildcards, and `-i.bak` would leave the "removed" key sitting in a
/// sibling file instead of gone.
///
/// **`grep -v` exits 1 when it selects no lines, and that is the ordinary case
/// here.** On a box riabuild alone provisioned, riabuild's is the only key in
/// the file, so removing it selects nothing — and the old
/// `grep … > new && cat new > keys && rm -f new` chain short-circuited on that
/// exit. The key line survived, a stray `authorized_keys.new` was left behind,
/// and the developer was told "Could not reach {host}" about a server that had
/// answered perfectly. This is the cleanup path failing in exactly the case it
/// matters most.
///
/// So the status is captured and read rather than chained: `0` and `1` are both
/// "grep did its job", and only `2` or above — the file vanished, it cannot be
/// read — leaves `authorized_keys` untouched. That distinction is the whole
/// reason this is not a bare `|| true`, which would let an unreadable file
/// produce an empty `.new` and then truncate every key on the box.
fn cleanup_script(home: &str, member_id: &str) -> String {
    let ns = session::namespace(home, member_id);
    let keys = format!("{home}/.ssh/authorized_keys");
    shell_command(&format!(
        "rm -rf {ns}; if [ -f {keys} ]; then grep -vF {marker} {keys} {redirect} {keys}.new; \
         status=$?; if [ \"$status\" -le 1 ]; then cat {keys}.new {redirect} {keys}; fi; \
         rm -f {keys}.new; [ \"$status\" -le 1 ]; fi",
        ns = shell_quote(&ns),
        keys = shell_quote(&keys),
        marker = shell_quote(&identity::key_comment_marker(member_id)),
        redirect = ">",
    ))
}

/// Whether the server turned us away rather than failing to answer.
///
/// The difference decides whether an issued identity is worth resolving:
/// a refusal is what a managed gateway gives riabuild's own key, and a
/// connection that never got that far is a box that is off.
fn refused_us(outcome: &Result<CommandOutput>) -> bool {
    let Ok(output) = outcome else {
        return false;
    };
    let stderr = output.stderr.to_ascii_lowercase();
    stderr.contains("permission denied")
        || stderr.contains("publickey")
        || stderr.contains("too many authentication failures")
        || stderr.contains("no supported authentication methods")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_fixture as remote;

    const MEMBER_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    /// I037, reproduced in a real shell before it was fixed here. On a box
    /// riabuild alone provisioned, riabuild's is the only key in
    /// `authorized_keys` — so removing it selects no lines, `grep -v` exits 1,
    /// and the old `&&` chain stopped there: the key line survived, a stray
    /// `authorized_keys.new` was left behind, and the developer was told
    /// "Could not reach build-01.fly.dev" about a server that answered fine.
    ///
    /// Asserted against a real `/bin/sh` running the real script over a real
    /// file, because every part of this bug is in the shell's exit statuses
    /// and none of it is visible to a `FakeRunner` that only records strings.
    #[tokio::test]
    async fn removing_the_only_key_in_authorized_keys_is_not_a_failure() {
        let server = tempfile::TempDir::new().expect("tempdir");
        let home = server.path().to_string_lossy().into_owned();
        tokio::fs::create_dir_all(server.path().join(".ssh"))
            .await
            .expect("mkdir");
        let keys = server.path().join(".ssh/authorized_keys");
        let only_line = format!(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 {}\n",
            identity::key_comment(&remote(), MEMBER_ID)
        );
        tokio::fs::write(&keys, &only_line).await.expect("write");

        let script = cleanup_script(&home, MEMBER_ID);
        let status = run_on_a_real_shell(&script).await;

        assert_eq!(
            status, 0,
            "a server whose only key was riabuild's answered fine; \
             reporting that as unreachable is the bug"
        );
        assert_eq!(
            tokio::fs::read_to_string(&keys).await.expect("read"),
            "",
            "riabuild's key line has to be gone"
        );
        assert!(
            !server.path().join(".ssh/authorized_keys.new").exists(),
            "the temp file must not be left on the developer's server"
        );
    }

    /// The other half, and the case that used to work: somebody else's key is
    /// in the file too, so `grep -v` selects a line and exits 0. Both have to
    /// pass, or "tolerate exit 1" has quietly become "ignore the result".
    #[tokio::test]
    async fn a_colleagues_key_survives_the_same_cleanup() {
        let server = tempfile::TempDir::new().expect("tempdir");
        let home = server.path().to_string_lossy().into_owned();
        tokio::fs::create_dir_all(server.path().join(".ssh"))
            .await
            .expect("mkdir");
        let keys = server.path().join(".ssh/authorized_keys");
        let bob = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 riabuild 11111111-2222-3333-4444-555555555555 bob@build-01.fly.dev:22\n";
        tokio::fs::write(
            &keys,
            format!(
                "{bob}ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 {}\n",
                identity::key_comment(&remote(), MEMBER_ID)
            ),
        )
        .await
        .expect("write");

        let status = run_on_a_real_shell(&cleanup_script(&home, MEMBER_ID)).await;

        assert_eq!(status, 0);
        assert_eq!(
            tokio::fs::read_to_string(&keys).await.expect("read"),
            bob,
            "matching on the member id is what keeps a co-tenant signed in"
        );
        assert!(!server.path().join(".ssh/authorized_keys.new").exists());
    }

    /// Runs the script `cleanup_server_side` would send, the way a server's
    /// `sshd` would: as one argument to a shell. `shell_command` already wraps
    /// it in `/bin/sh -c '…'`, so this hands that whole line to another `sh`
    /// exactly as the remote login shell does.
    async fn run_on_a_real_shell(script: &str) -> i32 {
        let runner = riabuild_runner::RealRunner;
        let output = runner
            .run(
                "/bin/sh",
                &["-c", script],
                &riabuild_runner::RunOptions::default(),
            )
            .await
            .expect("the shell runs");
        output
            .code
            .expect("the shell exited rather than being signalled")
    }

    #[test]
    fn a_refusal_and_an_unreachable_server_are_told_apart() {
        let refused = |stderr: &str| {
            refused_us(&Ok(CommandOutput {
                code: Some(255),
                stdout: String::new(),
                stderr: stderr.to_string(),
            }))
        };
        assert!(refused("Permission denied (publickey)."));
        assert!(refused("ada@gw: Permission denied (publickey,password)."));
        assert!(refused(
            "Received disconnect: Too many authentication failures"
        ));
        assert!(!refused(
            "ssh: connect to host build-01.fly.dev port 22: Connection refused"
        ));
        assert!(!refused("ssh: Could not resolve hostname build-01.fly.dev"));
        assert!(!refused(""));
    }
}

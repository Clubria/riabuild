//! Getting riabuild's own key into `~/.ssh/authorized_keys` on the server.
//!
//! Split out of `identity.rs` (Task 15) rather than folded into it: that file
//! already sits at the crate's ~300-line guideline and Task 15 deliberately
//! kept its security-rationale comments over trimming further, so this is a
//! second file rather than a third trim pass.
//!
//! ## Why riabuild never holds a password
//!
//! This is the one step in remote mode that can end up talking to a server
//! that only knows a password for the developer, not riabuild's new key yet.
//! riabuild never prompts for that password itself. Two reasons, both fatal
//! on their own:
//!
//! - `Ui::ask` (`src/ui/prompt.rs`) is a plain `read_line` — no `rpassword`,
//!   no raw `termios` in this crate — so anything typed at it is echoed to
//!   the screen and lands in scrollback.
//! - Even with echo suppressed, a value `authorise` read into a `String`
//!   would have to travel from there into `ssh-copy-id` somehow. The only
//!   channel in this crate that does not end up in `ps` output — visible to
//!   every other developer on a shared build box — is `RunOptions.stdin`,
//!   and `ssh-copy-id` itself already prompts on its *own* controlling
//!   terminal; piping a password into it non-interactively is not how it
//!   works, so using `stdin` here would mean writing SSH's password protocol
//!   by hand instead of trusting the real client to do it.
//!
//! So `authorise` hands the terminal to `ssh-copy-id` via
//! [`CommandRunner::run_interactive`], the same handoff `main.rs` uses for
//! the environment shell. `ssh-copy-id` execs the real `ssh`, which does its
//! own password prompting directly against the inherited terminal — with its
//! own no-echo `termios` handling — and returns only an exit code. The
//! password exists in the child's memory and the terminal driver's, never in
//! any `String` this crate owns, so there is nothing here for a log line, an
//! error message, or `Failure::detail` to accidentally include.
//!
//! What follows from that: a server that offers no interactive method at all
//! (`methods()` never sees `password` or `keyboard-interactive` in sshd's
//! refusal) must not be handed to `ssh-copy-id` — there is no password to
//! ask for, so a prompt would be a lie, and this returns the key as a line
//! to paste by hand instead.

use super::Remote;
use super::identity::{key_path, ssh_options};
use crate::paths::Paths;
use crate::runner::{CommandRunner, RunOptions};
use crate::ui::{Failure, Ui};
use anyhow::Result;
use std::sync::Arc;

/// The authentication methods sshd named in its refusal, e.g. `Permission
/// denied (publickey,password).` → `["publickey", "password"]`. Empty when
/// the failure was not an authentication refusal at all (a timeout, a closed
/// port) — there is no method list to read out of those.
#[allow(dead_code)] // consumed by Task 21, via authorise
pub fn offered_methods(stderr: &str) -> Vec<String> {
    let Some(start) = stderr.find("Permission denied (") else {
        return Vec::new();
    };
    let rest = &stderr[start + "Permission denied (".len()..];
    let Some(end) = rest.find(')') else {
        return Vec::new();
    };
    rest[..end]
        .split(',')
        .map(|method| method.trim().to_string())
        .filter(|method| !method.is_empty())
        .collect()
}

/// Can riabuild's own key sign in, without a password and without falling
/// back to the developer's own agent or default identities?
#[allow(dead_code)] // consumed by Task 21, via authorise
pub async fn can_sign_in(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
) -> Result<bool> {
    let mut args = vec!["-o".to_string(), "BatchMode=yes".to_string()];
    args.extend(ssh_options(remote, paths, true));
    args.push(remote.target());
    args.push("true".to_string());

    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(runner.run("ssh", &refs, &RunOptions::default()).await?.ok())
}

/// Installs riabuild's public key on the server, if it is not already there.
///
/// Idempotent, same rule as `ensure_key`: the first thing this does is check
/// whether riabuild's key already works, and if so it returns immediately
/// without touching the server again — a second `riabuild remote` against an
/// already-authorised box is a no-op here, not a repeat `ssh-copy-id` run.
///
/// See the module doc for why a password, when one exists, never passes
/// through riabuild's own hands.
#[allow(dead_code)] // consumed by Task 21
pub async fn authorise(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
) -> Result<()> {
    if can_sign_in(remote, paths, runner.clone()).await? {
        return Ok(());
    }

    let public_key_path = key_path(remote, paths).with_extension("pub");
    let public_key = match tokio::fs::read_to_string(&public_key_path).await {
        Ok(contents) => contents,
        Err(error) => {
            // Distinct from every `paste()` branch below: those all have a
            // key to show and just disagree about why the developer has to
            // paste it by hand. This is the case where there is no key to
            // paste at all — swallowing the read error here would produce
            // "Add this line to ~/.ssh/authorized_keys…" followed by
            // nothing, which reads as riabuild having quietly done
            // something rather than having failed to find its own key.
            return Err(Failure::new(
                format!("authorising riabuild's key on {}", remote.host),
                format!(
                    "riabuild's public key is missing at {}. Run `riabuild remote` again to regenerate it.",
                    public_key_path.display()
                ),
            )
            .detail(error.to_string())
            .into());
        }
    };
    let paste = || {
        Failure::new(
            format!("authorising riabuild's key on {}", remote.host),
            format!(
                "Add this line to ~/.ssh/authorized_keys on {}, then run `riabuild remote` again:\n    {}",
                remote.host,
                public_key.trim()
            ),
        )
    };

    // What will the server actually accept? `PreferredAuthentications=none`
    // makes sshd refuse before trying any method, so its refusal names every
    // method it offers rather than just the first one attempted.
    let mut probe = vec![
        "-o".to_string(),
        "PreferredAuthentications=none".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
    ];
    probe.extend(ssh_options(remote, paths, false));
    probe.push(remote.target());
    probe.push("true".to_string());
    let probe_refs: Vec<&str> = probe.iter().map(String::as_str).collect();
    let refusal = runner
        .run("ssh", &probe_refs, &RunOptions::default())
        .await?;
    let methods = offered_methods(&refusal.stderr);

    let interactive = methods
        .iter()
        .any(|method| method == "password" || method == "keyboard-interactive");
    if !interactive {
        // Nothing to prompt for: a publickey-only server would make
        // `ssh-copy-id` sit on a prompt nobody can answer.
        return Err(paste()
            .detail("that server accepts keys only, so there is no password to ask you for")
            .into());
    }
    if runner.which("ssh-copy-id").is_none() {
        // `ssh-copy-id` ships with the OpenSSH client on both platforms
        // riabuild targets, but a machine can still be missing it — fail
        // with the same actionable paste-this-line message rather than a
        // bare "command not found".
        return Err(paste()
            .detail("ssh-copy-id is not installed on this machine")
            .into());
    }

    ui.working("Authorised", "installing the key");
    // Built explicitly rather than by extending `ssh_options`, which carries
    // its own `-i` for the private key: `ssh-copy-id` parses `-i` with its
    // own getopt and would see two. Deliberately without `IdentitiesOnly` —
    // an existing key or the agent may be what proves who we are on a server
    // with password auth disabled for everyone but the developer already
    // trusted there.
    let args = vec![
        "-i".to_string(),
        public_key_path.to_string_lossy().into_owned(),
        "-p".to_string(),
        remote.port.to_string(),
        "-F".to_string(),
        "/dev/null".to_string(),
        "-o".to_string(),
        format!(
            "UserKnownHostsFile={}",
            paths.known_hosts_file().to_string_lossy()
        ),
        "-o".to_string(),
        "StrictHostKeyChecking=yes".to_string(),
        remote.target(),
    ];
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // The terminal handoff described in the module doc: whatever `ssh`
    // prompts for here — a password, a passphrase — goes straight between
    // the developer and the real `ssh` binary. Nothing here reads it.
    let code = runner
        .run_interactive("ssh-copy-id", &refs, &RunOptions::default())
        .await?;
    if code != 0 {
        return Err(paste().command("ssh-copy-id").into());
    }

    if !can_sign_in(remote, paths, runner).await? {
        return Err(paste()
            .detail("the key was copied, but signing in with it still does not work")
            .into());
    }
    ui.applied("Authorised");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RealPaths;
    use crate::runner::FakeRunner;
    use crate::ui::Ui;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    /// Writes the `.pub` file `authorise` expects to find already generated
    /// by `ensure_key` — every scenario past the "key already works" early
    /// return needs this, now that a missing key is a distinct, actionable
    /// failure rather than something `authorise` quietly reads as empty.
    async fn write_public_key(paths: &RealPaths) {
        tokio::fs::create_dir_all(paths.identity_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(
            paths
                .identity_dir()
                .join(remote().hash())
                .with_extension("pub"),
            "ssh-ed25519 AAAA riabuild",
        )
        .await
        .expect("write pub");
    }

    #[test]
    fn the_methods_a_server_offers_are_read_from_its_refusal() {
        assert_eq!(
            offered_methods("ada@box: Permission denied (publickey,password)."),
            vec!["publickey".to_string(), "password".to_string()]
        );
        assert_eq!(
            offered_methods("Permission denied (publickey,keyboard-interactive)."),
            vec!["publickey".to_string(), "keyboard-interactive".to_string()]
        );
        assert_eq!(
            offered_methods("Permission denied (publickey)."),
            vec!["publickey".to_string()]
        );
        assert!(offered_methods("ssh: connect to host box port 22: Connection refused").is_empty());
    }

    #[tokio::test]
    async fn a_publickey_only_server_gets_the_line_to_paste_rather_than_a_prompt() {
        // Nothing to prompt for: sshd never offers the method, so a password box
        // would be a lie. Print the key and say where it goes.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        tokio::fs::create_dir_all(paths.identity_dir())
            .await
            .expect("mkdir");
        tokio::fs::write(
            paths
                .identity_dir()
                .join(remote().hash())
                .with_extension("pub"),
            "ssh-ed25519 AAAA riabuild",
        )
        .await
        .expect("write pub");

        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey).",
                ),
        );
        let error = authorise(&remote(), &paths, fake.clone(), &Ui::new(true))
            .await
            .expect_err("must not claim success");

        // `Failure`'s Display is "{attempting} — {action}" and does not include
        // `detail`, so asserting on the formatted error cannot tell the three
        // paste() branches apart — it would pass if `authorise` returned paste()
        // unconditionally.
        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(
            failure.action.contains("authorized_keys"),
            "{}",
            failure.action
        );
        assert!(
            failure.detail.contains("keys only"),
            "the reason must distinguish this from a missing ssh-copy-id: {}",
            failure.detail
        );
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.starts_with("ssh-copy-id")),
            "ssh-copy-id cannot help here and must not be run"
        );
    }

    #[tokio::test]
    async fn a_key_that_already_works_is_not_copied_again() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh -o BatchMode=yes", 0, "", ""));

        authorise(&remote(), &paths, fake.clone(), &Ui::new(true))
            .await
            .expect("already fine");
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.starts_with("ssh-copy-id"))
        );
    }

    #[tokio::test]
    async fn ssh_copy_id_runs_when_the_server_will_take_a_password() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey,password).",
                )
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey,password).",
                )
                .with("ssh-copy-id", 0, "", ""),
        );

        // The second BatchMode probe, after copying, has to succeed for the step to
        // pass. The fake returns the same stub for both, so this asserts the copy ran
        // and that a still-failing sign-in is reported.
        let result = authorise(&remote(), &paths, fake.clone(), &Ui::new(true)).await;
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh-copy-id")),
            "{:?}",
            fake.calls()
        );
        assert!(
            result.is_err(),
            "a key that still cannot sign in is not success"
        );
    }

    #[tokio::test]
    async fn a_missing_ssh_copy_id_is_a_next_action_not_a_crash() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        // FakeRunner::which only knows programs that have been stubbed.
        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey,password).",
                )
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey,password).",
                ),
        );
        let error = authorise(&remote(), &paths, fake, &Ui::new(true))
            .await
            .expect_err("no ssh-copy-id");
        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(failure.detail.contains("ssh-copy-id"), "{}", failure.detail);
    }

    #[tokio::test]
    async fn a_key_that_works_after_ssh_copy_id_is_reported_as_success() {
        // The success branch (`ui.applied("Authorised"); Ok(())`) had no
        // coverage: every other test's `BatchMode=yes` probe returns the
        // same fixed response both times it's called, so the post-copy
        // recheck could never see a different, successful answer. `.then()`
        // queues a distinct response per call to the same key: the first
        // probe (before `ssh-copy-id`) fails, the second (after) succeeds.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(
            FakeRunner::new()
                .then(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey,password).",
                )
                .then("ssh -o BatchMode=yes", 0, "", "")
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey,password).",
                )
                .with("ssh-copy-id", 0, "", ""),
        );

        authorise(&remote(), &paths, fake.clone(), &Ui::new(true))
            .await
            .expect("the key works after ssh-copy-id, so this must succeed");

        let copy_call = fake
            .calls()
            .into_iter()
            .find(|call| call.starts_with("ssh-copy-id"))
            .expect("ssh-copy-id ran");
        // The full argument vector, not just the program name: a regression
        // dropping `UserKnownHostsFile` or `StrictHostKeyChecking=yes` would
        // silently let `ssh-copy-id` trust afresh instead of routing through
        // Task 15's pinned host key, and a `starts_with` check alone would
        // not notice.
        let expected_key = key_path(&remote(), &paths).with_extension("pub");
        assert_eq!(
            copy_call,
            format!(
                "ssh-copy-id -i {} -p 2222 -F /dev/null -o UserKnownHostsFile={} -o StrictHostKeyChecking=yes ada@build-01.fly.dev",
                expected_key.to_string_lossy(),
                paths.known_hosts_file().to_string_lossy(),
            )
        );
    }

    #[tokio::test]
    async fn a_missing_public_key_fails_loudly_instead_of_pasting_nothing() {
        // No `.pub` file written: stands in for a crash between key
        // generation and authorisation, a manual deletion, or a permissions
        // problem. Silently reading it as empty would produce "Add this
        // line to ~/.ssh/authorized_keys…" followed by nothing.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with(
            "ssh -o BatchMode=yes",
            255,
            "",
            "Permission denied (publickey,password).",
        ));

        let error = authorise(&remote(), &paths, fake.clone(), &Ui::new(true))
            .await
            .expect_err("a missing public key must not read as success");

        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(
            failure.action.contains("missing"),
            "the developer must be told the key itself is missing, not shown \
             an empty paste-this-line instruction: {}",
            failure.action
        );
        assert!(
            !fake.calls().iter().any(|call| {
                call.starts_with("ssh-copy-id")
                    || call.starts_with("ssh -o PreferredAuthentications")
            }),
            "a missing key must fail before probing the server or running ssh-copy-id: {:?}",
            fake.calls()
        );
    }
}

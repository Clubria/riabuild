//! Getting riabuild's own key into `~/.ssh/authorized_keys` on the server.
//!
//! Split out of `identity.rs` (Task 15) rather than folded into it: that file
//! already sits at the crate's ~300-line guideline and Task 15 deliberately
//! kept its security-rationale comments over trimming further, so this is a
//! second file rather than a third trim pass.
//!
//! ## What a password means here
//!
//! This is the one step in remote mode that can end up talking to a server
//! that only knows a password for the developer, not riabuild's new key yet.
//!
//! `authorise` still never prompts for that password itself: it hands the
//! terminal to `ssh-copy-id` via [`CommandRunner::run_interactive`], the same
//! handoff `main.rs` uses for the environment shell, and `ssh-copy-id` execs
//! the real `ssh`, which does its own prompting and returns only an exit
//! code. What changed is where that prompt is *answered* — `askpass` puts
//! riabuild on the other end of it, so the password is asked for once rather
//! than at each of the ten connections one `riabuild remote` opens. The
//! argument for that, and against the `sshpass`-shaped alternative this file
//! used to rule out, is in `askpass`'s module doc rather than repeated here.
//!
//! ## When this stops, and when it does not
//!
//! **riabuild stops when there is no way in, not when the convenient way in
//! failed.** `authorise` establishes up front whether the server offers
//! `password` or `keyboard-interactive`, and that answer divides everything
//! below it:
//!
//! - A server offering **neither** must not be handed to `ssh-copy-id` —
//!   there is no password to ask for, so a prompt would be a lie. That is a
//!   hard failure with the key as a line to paste by hand, and so are the two
//!   other cases where nothing riabuild could do next would work: a public key
//!   that is missing, and a host key that does not match the pin.
//! - Past that guard, a way in exists. `ssh-copy-id` being absent, exiting
//!   non-zero, or succeeding while the key *still* cannot sign in are all
//!   real, ordinary outcomes — an `AuthorizedKeysFile` pointing elsewhere, a
//!   home directory on a mode sshd will not trust, an
//!   `AuthenticationMethods publickey,password` policy — and none of them
//!   means the developer cannot reach the machine. They warn, keep the
//!   paste-this-line remedy, and return `Ok`. Every later `ssh` falls back to
//!   the password on its own: `IdentitiesOnly=yes` restricts which *keys* are
//!   offered, never which *methods*.
//!
//! Stopping there was the bug this replaces. It stopped a developer who had a
//! working way onto the server, at the point where every remaining step —
//! installing the server's riabuild, minting its session, lending it a GitHub
//! sign-in — would have succeeded.

use super::Remote;
use super::host_key::entry_host;
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

/// Did `ssh` refuse over the *server's* identity rather than ours?
///
/// A host-key failure aborts the connection before any authentication method
/// is offered, so its stderr names none — and [`offered_methods`] reads an
/// empty list as "publickey only". Left to fall through, a stale pin (a VM
/// rebuilt with a new key, or a box recreated after `remote forget`, which
/// leaves the pin behind on purpose) is reported as a server that wants a key
/// pasted into `authorized_keys`. The developer pastes it, nothing changes,
/// and no riabuild command clears the pin that actually caused it.
///
/// Deliberately narrow: OpenSSH's two literals, and only when the stderr does
/// not also carry an authentication refusal — so a genuine `Permission denied
/// (publickey,password)` can never be swallowed by, say, a login banner that
/// quotes the phrase back at us.
pub fn host_key_failure(stderr: &str) -> bool {
    (stderr.contains("Host key verification failed")
        || stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"))
        && !stderr.contains("Permission denied (")
}

/// The one wording for "`ssh` refused the *server's* identity, so this never
/// reached authentication at all". Shared rather than written at each site
/// that can observe it, so the remedy — which names riabuild's own
/// `known_hosts`, invisible to the developer under `-F /dev/null` — cannot
/// drift between them.
fn stale_pin(remote: &Remote, paths: &dyn Paths, stderr: String) -> anyhow::Error {
    Failure::new(
        format!("verifying {}'s host key", remote.host),
        format!(
            "ssh refused the host key riabuild has pinned for {}, so this never got as \
             far as authenticating — the key in `authorized_keys` is not the problem. If \
             that server was rebuilt or replaced, confirm its new fingerprint with \
             whoever runs it, then remove the {} line from {} and run `riabuild remote` \
             again.",
            remote.host,
            entry_host(remote),
            paths.known_hosts_file().display()
        ),
    )
    .detail(stderr)
    .into()
}

/// Makes sure the developer's own `~/.ssh` exists, at mode 0700.
///
/// Not riabuild's `ssh_dir()` — that one is `~/.riabuild/ssh` and riabuild
/// creates it itself. This is the real one, and it belongs to `ssh-copy-id`:
/// it builds its temporary directory *under* `~/.ssh` and fails outright if
/// there is nothing to build it under, with
///
/// ```text
/// mktemp: failed to create directory via template '…/.ssh/ssh-copy-id.XXXXXXXXXX'
/// ssh-copy-id: ERROR: failed to create required temporary directory under ~/.ssh
/// ```
///
/// which says nothing about riabuild and gives a developer nothing to do.
///
/// On any laptop that has cloned over SSH or run `ssh` once, `~/.ssh` is
/// already there — which is exactly why this went unnoticed until the remote
/// e2e ran against a container whose home directory was fresh. riabuild's
/// whole claim is to be the *first* thing a developer runs, and a machine
/// that has never opened an SSH connection has no `~/.ssh` at all.
///
/// 0700 at creation rather than created-then-chmod: `ssh` refuses to use a
/// key directory other users can read, so a directory made at the umask
/// would be a second failure, later, from a different tool, with a different
/// message.
async fn ensure_dot_ssh(paths: &dyn Paths) -> Result<()> {
    let dot_ssh = paths.home().join(".ssh");
    if tokio::fs::metadata(&dot_ssh).await.is_ok() {
        // Already there. Its mode is the developer's business — repairing it
        // would be riabuild changing something it did not create.
        return Ok(());
    }

    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(&dot_ssh).await.map_err(|error| {
        Failure::new(
            "preparing this machine to authorise a key",
            "Create it yourself with `mkdir -p ~/.ssh && chmod 700 ~/.ssh`, then run `riabuild remote` again.",
        )
        .detail(format!("could not create {}: {error}", dot_ssh.display()))
    })?;
    Ok(())
}

/// Can riabuild's own key sign in, without a password and without falling
/// back to the developer's own agent or default identities?
///
/// `Err` is narrower than "no". A host key that no longer matches the pin
/// aborts the connection at the host-key step, before any authentication
/// method is offered, so the honest answer is not "riabuild's key is not
/// authorised" but "that is not the server riabuild pinned" — and it is
/// diagnosed here, in the probe every path shares, rather than at one
/// caller. `riabuild remote --check` calls this *instead of* [`authorise`]
/// (nothing may write to the server on that path), so a diagnosis living
/// only inside `authorise` left `--check` against a rebuilt — or
/// impersonated — box printing "riabuild's key is not authorised there yet"
/// and exiting 0.
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
    // `RunOptions::default()`, deliberately: this is the *only* family of ssh
    // calls in remote mode that does not carry `askpass::run_options`. The
    // question it asks is "can riabuild's key sign in", and an askpass that
    // could answer a password prompt would let a saved password make the
    // answer yes on a server where the key does not work at all — which is
    // exactly the state the warning path exists to report. `BatchMode=yes`
    // already forbids every prompt, askpass included, so this is belt and
    // braces; it is written down because the belt is invisible and someone
    // adding `..run_options(…)` here for consistency would break the probe
    // without breaking a single test.
    let probe = runner.run("ssh", &refs, &RunOptions::default()).await?;
    if host_key_failure(&probe.stderr) {
        return Err(stale_pin(remote, paths, probe.stderr));
    }
    Ok(probe.ok())
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
    // The same remedy as `paste`, said as a warning rather than as a stop.
    // Deliberately built from the same `public_key`, and deliberately still
    // naming `authorized_keys`: the developer is getting in either way, and
    // this is how they stop being asked for a password on every future run.
    let carry_on = |because: &str| {
        ui.warn(&format!(
            "riabuild's key cannot sign in to {host} yet — {because}.\n    \
             The rest of this run will use {target}'s password instead; riabuild asks \
             for it once and remembers it.\n    \
             To stop being asked at all, add this line to ~/.ssh/authorized_keys on \
             {host}:\n      {key}",
            host = remote.host,
            target = remote.target(),
            key = public_key.trim(),
        ));
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
    // A stale pin never reaches this line: both probes talk to the same host
    // through the same pinned `known_hosts`, so [`can_sign_in`] above has
    // already returned [`stale_pin`] as an `Err` by the time this one runs.
    // Deliberately not re-checked here — a second, unreachable copy of the
    // diagnosis is a copy nothing can prove still works, and the one that
    // matters now also covers `--check`, which never calls this function.
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
        // riabuild targets, but a machine can still be missing it. Not fatal:
        // it is the tool that makes the *key* work, and the server has just
        // said it will take a password, so the developer still gets in — they
        // are only left installing the key by hand.
        carry_on("ssh-copy-id is not installed on this machine");
        return Ok(());
    }

    ensure_dot_ssh(paths).await?;

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
        .run_interactive(
            "ssh-copy-id",
            &refs,
            &super::askpass::run_options(remote, paths),
        )
        .await?;
    if code != 0 {
        carry_on("ssh-copy-id could not install it");
        return Ok(());
    }

    if !can_sign_in(remote, paths, runner).await? {
        // `ssh-copy-id` exited 0, so the line reached `authorized_keys` and
        // sshd still refused it. Ordinary causes: `AuthorizedKeysFile` points
        // somewhere else, the account's home is on a mode sshd will not
        // trust, or the policy is `AuthenticationMethods publickey,password`.
        // None of them is a reason to stop a developer whose password works.
        carry_on("the key was copied, but signing in with it still does not work");
        return Ok(());
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

    /// The laptop riabuild claims to be the first thing run on: one that has
    /// never opened an SSH connection, so it has no `~/.ssh` at all.
    /// `ssh-copy-id` builds its temporary directory under that path and exits
    /// with `failed to create required temporary directory under ~/.ssh` when
    /// it is missing — a message with riabuild nowhere in it and nothing in it
    /// for a developer to do.
    #[tokio::test]
    async fn a_machine_that_has_never_used_ssh_gets_the_directory_ssh_copy_id_needs() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dot_ssh = home.path().join(".ssh");
        assert!(
            !dot_ssh.exists(),
            "the point of the test is that it is absent"
        );

        ensure_dot_ssh(&paths).await.expect("creates it");

        let mode = tokio::fs::metadata(&dot_ssh)
            .await
            .expect("created")
            .permissions()
            .mode()
            & 0o777;
        // `ssh` refuses a key directory others can read, so creating it at the
        // umask would only move the failure to a later tool with a different
        // message.
        assert_eq!(mode, 0o700, "created at {mode:o}, not 0700");
    }

    /// An existing `~/.ssh` is the developer's, including its mode. Repairing
    /// it would be riabuild changing something it did not create — and on a
    /// normal laptop this is the case that always runs.
    #[tokio::test]
    async fn an_existing_ssh_directory_is_left_exactly_as_it_was() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let dot_ssh = home.path().join(".ssh");
        tokio::fs::create_dir_all(&dot_ssh).await.expect("mkdir");
        tokio::fs::set_permissions(&dot_ssh, std::fs::Permissions::from_mode(0o755))
            .await
            .expect("chmod");
        tokio::fs::write(dot_ssh.join("config"), "Host *\n")
            .await
            .expect("write");

        ensure_dot_ssh(&paths).await.expect("no-op");

        let mode = tokio::fs::metadata(&dot_ssh)
            .await
            .expect("still there")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o755,
            "riabuild must not chmod what it did not create"
        );
        assert!(
            dot_ssh.join("config").exists(),
            "and must not disturb what is in it"
        );
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

    /// What `ssh` really prints when the pinned key no longer matches.
    const HOST_KEY_CHANGED: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
         @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
         IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
         Host key verification failed.";

    #[test]
    fn a_host_key_failure_is_told_apart_from_an_authentication_refusal() {
        assert!(host_key_failure(HOST_KEY_CHANGED));
        // A first-connection refusal under StrictHostKeyChecking=yes prints
        // only the second literal.
        assert!(host_key_failure(
            "No ED25519 host key is known for box and you have requested strict checking.\n\
             Host key verification failed."
        ));
        assert!(!host_key_failure(
            "ada@box: Permission denied (publickey,password)."
        ));
        assert!(!host_key_failure(
            "ssh: connect to host box port 22: Connection refused"
        ));
        assert!(!host_key_failure(
            "Warning: Permanently added 'box' to the list of known hosts."
        ));
        // Narrow on purpose: a real refusal wins even where the phrase also
        // appears, so nothing chatty on stderr can mask a method list.
        assert!(!host_key_failure(
            "Host key verification failed.\nPermission denied (publickey,password)."
        ));
    }

    #[tokio::test]
    async fn a_changed_host_key_makes_the_probe_itself_fail_rather_than_answer_no() {
        // `can_sign_in` is not only `authorise`'s first step: `riabuild remote
        // --check` calls it *instead of* `authorise`, and reads `false` as
        // "riabuild's key is not authorised on that server yet" — a note, and
        // exit 0. So a probe that only reports `output.ok()` hands that
        // sentence, and a success exit code, to a developer whose server's
        // host key has changed under them.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake =
            Arc::new(FakeRunner::new().with("ssh -o BatchMode=yes", 255, "", HOST_KEY_CHANGED));

        let error = can_sign_in(&remote(), &paths, fake)
            .await
            .expect_err("a refused host key is not an answer to `can this key sign in?`");
        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(
            failure.attempting.contains("host key"),
            "this has to read as a host-key problem: {}",
            failure.attempting
        );
        assert!(
            failure
                .action
                .contains(&paths.known_hosts_file().display().to_string()),
            "the remedy must name the file holding the stale pin: {}",
            failure.action
        );

        // The other direction, so this cannot pass by treating every failed
        // probe as a host-key problem: an ordinary refusal is still `false`.
        let denied = Arc::new(FakeRunner::new().with(
            "ssh -o BatchMode=yes",
            255,
            "",
            "Permission denied (publickey,password).",
        ));
        assert!(
            !can_sign_in(&remote(), &paths, denied)
                .await
                .expect("an authentication refusal is an answer, not an error")
        );
    }

    #[tokio::test]
    async fn a_stale_host_key_pin_is_reported_as_one_not_as_a_keys_only_server() {
        // C3: the probe's stderr names no methods because ssh never got as far
        // as offering any, and an empty list used to read as "publickey only".
        // The developer was then told to paste a key into `authorized_keys` —
        // which changes nothing, because the key was never the problem, and
        // riabuild's own known_hosts is invisible to them (`-F /dev/null`).
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(
            FakeRunner::new()
                .with("ssh -o BatchMode=yes", 255, "", HOST_KEY_CHANGED)
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    HOST_KEY_CHANGED,
                ),
        );

        let error = authorise(&remote(), &paths, fake.clone(), &Ui::new(true))
            .await
            .expect_err("a refused host key is not success");
        let failure = error.downcast_ref::<Failure>().expect("a Failure");

        assert!(
            failure.attempting.contains("host key"),
            "this has to read as a host-key problem: {}",
            failure.attempting
        );
        assert!(
            !failure.detail.contains("keys only"),
            "the old misdiagnosis: {}",
            failure.detail
        );
        assert!(
            failure
                .action
                .contains(&paths.known_hosts_file().display().to_string())
                && failure.action.contains(&entry_host(&remote())),
            "the remedy must name the file and the line to clear: {}",
            failure.action
        );
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.starts_with("ssh-copy-id")),
            "ssh-copy-id cannot help across a refused host key"
        );
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

    /// The refusal a server that will take a password gives, used by every
    /// test below that needs `authorise` to get past its "is there a way in?"
    /// guard.
    const TAKES_A_PASSWORD: &str = "Permission denied (publickey,password).";

    /// Both probes refusing with a method list that includes `password`.
    fn a_server_that_takes_a_password() -> FakeRunner {
        FakeRunner::new()
            .with("ssh -o BatchMode=yes", 255, "", TAKES_A_PASSWORD)
            .with(
                "ssh -o PreferredAuthentications=none",
                255,
                "",
                TAKES_A_PASSWORD,
            )
    }

    /// The one warning `authorise` printed, or a panic naming what it did
    /// print instead. Every downgraded path has to *say* something: `Ok(())`
    /// on its own is indistinguishable from a step that silently did nothing.
    fn the_one_warning(ui: &Ui) -> String {
        let warnings = ui.warned();
        assert_eq!(warnings.len(), 1, "exactly one warning, not {warnings:?}");
        warnings.into_iter().next().unwrap_or_default()
    }

    /// A downgraded outcome still has to hand over the remedy — the key, and
    /// where it goes — or the developer is left typing a password forever
    /// with nothing to do about it.
    fn assert_carries_the_remedy(warning: &str) {
        assert!(
            warning.contains("authorized_keys"),
            "the warning must still say where the key goes: {warning}"
        );
        assert!(
            warning.contains("ssh-ed25519 AAAA riabuild"),
            "…and must still carry the key itself: {warning}"
        );
        assert!(
            warning.contains("password"),
            "…and must say what happens instead, or an `Ok` reads as success: {warning}"
        );
    }

    #[tokio::test]
    async fn a_key_that_will_not_sign_in_after_copying_warns_instead_of_stopping() {
        // The reported bug. `ssh-copy-id` exits 0 — the line reached
        // `authorized_keys` — and sshd still refuses it. The developer's
        // password works, every remaining step would have succeeded, and
        // riabuild used to stop there.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        // The same refusal before and after the copy: the fake answers both
        // `BatchMode` probes identically, so the post-copy recheck fails.
        let fake = Arc::new(a_server_that_takes_a_password().with("ssh-copy-id", 0, "", ""));
        let ui = Ui::new(true);

        authorise(&remote(), &paths, fake.clone(), &ui)
            .await
            .expect("a password is a way in, so this must not stop the run");

        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("ssh-copy-id")),
            "only meaningful if the copy was actually attempted: {:?}",
            fake.calls()
        );
        let warning = the_one_warning(&ui);
        assert!(
            warning.contains("still does not work"),
            "the warning has to name this cause, not one of the other two: {warning}"
        );
        assert_carries_the_remedy(&warning);
    }

    #[tokio::test]
    async fn a_missing_ssh_copy_id_warns_rather_than_ending_the_run() {
        // `ssh-copy-id` is what makes the *key* work. The server has just said
        // it takes a password, so its absence costs the developer a key, not
        // the machine.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        // FakeRunner::which only knows programs that have been stubbed, so
        // not stubbing `ssh-copy-id` is what makes it missing here.
        let fake = Arc::new(a_server_that_takes_a_password());
        let ui = Ui::new(true);

        authorise(&remote(), &paths, fake, &ui)
            .await
            .expect("a missing ssh-copy-id is not a missing way in");

        let warning = the_one_warning(&ui);
        assert!(
            warning.contains("ssh-copy-id is not installed"),
            "{warning}"
        );
        assert_carries_the_remedy(&warning);
    }

    #[tokio::test]
    async fn an_ssh_copy_id_that_fails_warns_rather_than_ending_the_run() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(a_server_that_takes_a_password().with(
            "ssh-copy-id",
            1,
            "",
            "could not write",
        ));
        let ui = Ui::new(true);

        authorise(&remote(), &paths, fake, &ui)
            .await
            .expect("a failed copy is not a missing way in");

        let warning = the_one_warning(&ui);
        assert!(warning.contains("could not install it"), "{warning}");
        assert_carries_the_remedy(&warning);
    }

    #[tokio::test]
    async fn ssh_copy_id_is_told_which_helper_answers_its_password_prompt() {
        // Without this the developer types the password at `ssh-copy-id`, and
        // then again at every connection after it — which is the outcome the
        // saved password exists to prevent, arriving one step late.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(a_server_that_takes_a_password().with("ssh-copy-id", 0, "", ""));

        let _ = authorise(&remote(), &paths, fake.clone(), &Ui::new(true)).await;

        let env = fake.env_of("ssh-copy-id");
        assert!(
            !env.is_empty(),
            "ssh-copy-id ran with no environment at all"
        );
        let expected = super::super::askpass::ssh_env(&remote(), &paths);
        for (key, value) in expected {
            assert!(
                env.contains(&(key.clone(), value.clone())),
                "ssh-copy-id must carry {key}={value}: {env:?}"
            );
        }
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

//! Getting riabuild's own key into `~/.ssh/authorized_keys` on the server.
//!
//! Split out of `identity.rs` (Task 15) rather than folded into it: that file
//! already sits at the crate's ~300-line guideline and Task 15 deliberately
//! kept its security-rationale comments over trimming further, so this is a
//! second file rather than a third trim pass.
//!
//! This file decides *whether* to install the key and what to say about the
//! outcome; `copy` is what actually installs it, and its module doc holds the
//! argument for riabuild writing its own `ssh-copy-id` rather than running
//! the one on the machine.
//!
//! ## What a password means here
//!
//! This is the one step in remote mode that can end up talking to a server
//! that only knows a password for the developer, not riabuild's new key yet.
//!
//! `authorise` never prompts for that password itself. The copy is an
//! ordinary captured `ssh` carrying `askpass`'s environment, so `ssh` asks
//! the helper and reads the answer off its stdout pipe: nothing is typed at
//! this process, nothing lands in an argument vector, and the password is
//! asked for once rather than at each of the ten connections one `riabuild
//! remote` opens. The argument for that, and against the `sshpass`-shaped
//! alternative this file used to rule out, is in `askpass`'s module doc
//! rather than repeated here.
//!
//! On an OpenSSH older than 8.4, which ignores `SSH_ASKPASS_REQUIRE`, `ssh`
//! prompts on the terminal itself — it opens `/dev/tty` directly, so it
//! reaches the developer whether or not riabuild is capturing the child's
//! stdio, and the password still exists only in `ssh`'s own memory. There is
//! nothing here for a log line, an error message, or `Failure::detail` to
//! accidentally include.
//!
//! ## When this stops, and when it does not
//!
//! **riabuild stops when there is no way in, not when the convenient way in
//! failed.** `authorise` establishes up front whether the server offers
//! `password` or `keyboard-interactive`, and that answer divides everything
//! below it:
//!
//! - A server offering **neither** must not be handed to the copy — there is
//!   no password to ask for, so a prompt would be a lie. That is a hard
//!   failure with the key as a line to paste by hand, and so are the two
//!   other cases where nothing riabuild could do next would work: a public key
//!   that is missing, and a host key that does not match the pin.
//!
//!   **Unless a key the org issued this developer can sign in.** That is the
//!   one thing checked before the method probe below, and the reason it comes
//!   first: an identity that has just proved itself has already answered what
//!   the probe asks, and a keys-only server is exactly the machine an issued
//!   key exists for. Where the paragraph above says "no way in", it now means
//!   no way in *and* no issued key — see `issued`.
//! - Past that guard, a way in exists. The copy failing, or succeeding while
//!   the key *still* cannot sign in, are both real, ordinary outcomes — an
//!   `AuthorizedKeysFile` pointing elsewhere, a home directory on a mode sshd
//!   will not trust, an `AuthenticationMethods publickey,password` policy —
//!   and neither means the developer cannot reach the machine. They warn and
//!   return `Ok`. Every later `ssh` falls back to the password on its own:
//!   `IdentitiesOnly=yes` restricts which *keys* are offered, never which
//!   *methods*.
//!
//! Stopping there was an earlier bug this replaced. It stopped a developer who
//! had a working way onto the server, at the point where every remaining step —
//! installing the server's riabuild, minting its session, lending it a GitHub
//! sign-in — would have succeeded.
//!
//! ## Which warning
//!
//! There are two, and they are not interchangeable. When riabuild has just
//! written the line, the remedy is the key and where it goes. When the line
//! was **already there** and the server refuses it anyway, that remedy is
//! worse than useless: it tells a developer to paste a key they have pasted
//! before, which is what riabuild did on every run for as long as
//! `ssh-copy-id` was the thing deciding whether the key was installed. That
//! case names the server-side settings that can produce it instead.

pub(crate) mod copy;
mod install;
mod probe;
mod words;

use install::finish;
pub use probe::{can_sign_in, host_key_failure, offered_methods};
use words::paste;

use super::Remote;
use super::identity::key_path;
use super::ssh::Ssh;
use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::{Failure, Ui};
use std::sync::Arc;

/// Installs riabuild's public key on the server, if it is not already there.
///
/// Idempotent twice over, and it needs to be. The first check is whether
/// riabuild's key already *works*, which returns immediately without touching
/// the server — the same rule as `ensure_key`. The second lives in [`copy`]
/// and asks whether the line is already in the file, which is the only one of
/// the two that still answers usefully on a server that does not honour
/// `authorized_keys`: there, the first check says no on every run forever, so
/// without the second, every run appends another identical line.
///
/// See the module doc for why a password, when one exists, never passes
/// through riabuild's own hands.
pub async fn authorise(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    api: &riabuild_api::ApiClient,
    issued: &mut crate::issued::Issued,
) -> Result<Option<crate::issued::Working>> {
    if can_sign_in(remote, paths, runner.clone()).await? {
        // Riabuild's own key works, so no password will be asked for on this
        // run and any password sitting unconfirmed from a previous one — a run
        // killed between the askpass helper writing it and `copy`'s verdict —
        // will never be confirmed by anything. Swept here because this is the
        // one place that knows that; best effort, because a keyring that will
        // not answer must not fail a run that has nothing else to do. See
        // `askpass::discard_pending`.
        let _ = crate::askpass::discard_pending(remote, paths, runner).await;
        // The early return that makes issued keys free. `issued` is untouched
        // here, so a returning developer fetches nothing, starts no agent, and
        // never has an org private key in this process's memory at all.
        return Ok(None);
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
    // Asked only now, which is the whole of what makes this cheap: riabuild's
    // own key has already failed, so this is a server that needs setting up
    // rather than one already set up. `working` fetches and probes at most
    // once and never returns `Err` — every ordinary failure is a `None` and
    // the password path below is unchanged.
    if let Some(entry) = issued
        .working(api, remote, paths, runner.clone(), ui)
        .await
        .cloned()
    {
        // Note what is skipped: the `PreferredAuthentications=none` probe
        // below. An identity that has just signed in has already answered the
        // question that probe asks, and asking again would spend a connection
        // to be told what we know. More importantly it is what the `!interactive`
        // branch would refuse on — a keys-only server — and refusing there is
        // precisely the failure this feature exists to remove.
        ui.working(
            "Authorised",
            &format!("installing the key over the {} identity", entry.label),
        );
        return finish(remote, paths, runner, ui, &public_key, Some(&entry), issued).await;
    }

    // What will the server actually accept? `PreferredAuthentications=none`
    // makes sshd refuse before trying any method, so its refusal names every
    // method it offers rather than just the first one attempted.
    let refusal = Ssh::to(remote, paths, runner.clone())
        .every_identity()
        .option("PreferredAuthentications=none")
        .option("BatchMode=yes")
        .without_askpass()
        .run("true")
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
        // Nothing to prompt for: a publickey-only server would leave the copy
        // sitting on a prompt nobody can answer.
        //
        // Reached only when no issued key signed in above — which is what the
        // whole issued-keys feature exists to change about this branch. A
        // developer who has been issued a key for this machine never gets here.
        return Err(paste(remote, &public_key)
            .detail("that server accepts keys only, so there is no password to ask you for")
            .into());
    }

    ui.working("Authorised", "installing the key");
    finish(remote, paths, runner, ui, &public_key, None, issued).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_key::entry_host;
    use crate::issued::{Issued, Working};
    use riabuild_api::ApiClient;

    /// A client no test here ever calls through.
    ///
    /// `Issued::preset` fills the cached answer, so `working` returns without
    /// reaching `find` — nothing in this file makes a request, and nothing
    /// waits on a timeout to discover that.
    fn api() -> ApiClient {
        ApiClient::new("test")
    }
    use riabuild_paths::RealPaths;
    use riabuild_runner::FakeRunner;
    use riabuild_ui::Ui;

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
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDQMfwG+m0AkDbU6a0vxE5ktTNTso5LskpebOKYF2VHP riabuild",
        )
        .await
        .expect("write pub");
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
                .with("ssh -o BatchMode=yes", 255, "", probe::HOST_KEY_CHANGED)
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    probe::HOST_KEY_CHANGED,
                ),
        );

        let error = authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
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
        assert_eq!(
            copy_attempts(&fake),
            0,
            "nothing can be installed across a refused host key: {:?}",
            fake.calls()
        );
    }
    /// An issued key that has already proved itself against this server.
    fn working() -> Working {
        Working {
            label: "prod-bastion".into(),
            socket: "/tmp/riabuild-test/sock".into(),
            public_key_path: "/tmp/riabuild-test/k17abc.pub".into(),
        }
    }
    #[tokio::test]
    async fn a_keys_only_server_with_an_issued_key_installs_the_key_instead_of_failing() {
        // The case this whole feature exists for, and a hard failure before it:
        // `PasswordAuthentication no`, so there is no password to ask for, and
        // riabuild used to stop and tell the developer to paste a public key
        // into a file they may not be able to edit.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;

        let fake = Arc::new(
            FakeRunner::new()
                // riabuild's own key does not work, and the server offers no
                // password — the two facts that used to end the run here.
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                // The copy, over the issued identity, and then the re-probe.
                .containing("IdentityAgent=", 0, "", ""),
        );

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(Some(working())),
        )
        .await
        .expect("an issued key that works is not a failure");

        let copy = fake
            .calls()
            .into_iter()
            .find(|call| call.contains("authorized_keys"))
            .expect("the key must have been installed");
        // Authenticated by the issued identity, and by that one alone.
        assert!(
            copy.contains("IdentityAgent=/tmp/riabuild-test/sock"),
            "{copy}"
        );
        assert!(copy.contains("IdentitiesOnly=yes"), "{copy}");
        assert!(copy.contains("k17abc.pub"), "{copy}");
    }
    #[tokio::test]
    async fn an_issued_key_authorises_the_copy_and_nothing_after_it() {
        // The bootstrap rule. If an issued key carried the whole run, every
        // developer would reach the server as one fingerprint and `remote
        // forget` would have no line of their own to remove. It authenticates
        // the copy; riabuild's own key does the rest.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;

        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                .containing("IdentityAgent=", 0, "", ""),
        );

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(Some(working())),
        )
        .await
        .expect("authorised");

        // Exactly one connection carries the issued identity: the copy. The
        // sign-in probe that follows it must not, or riabuild would be
        // reporting the issued key's access as its own.
        let over_issued = fake
            .calls()
            .iter()
            .filter(|call| call.contains("IdentityAgent="))
            .count();
        assert_eq!(over_issued, 1, "{:?}", fake.calls());
    }
    #[tokio::test]
    async fn no_issued_key_leaves_the_keys_only_failure_exactly_as_it_was() {
        // The old behaviour has to survive untouched for the servers this
        // feature does not reach — which is most of them.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;

        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey).",
                ),
        );

        let error = authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect_err("must still fail");

        let failure = error.downcast_ref::<Failure>().expect("a Failure");
        assert!(failure.detail.contains("keys only"), "{}", failure.detail);
    }
    #[tokio::test]
    async fn a_server_riabuilds_own_key_already_reaches_never_asks_about_issued_keys() {
        // The property that makes this feature free for a returning developer.
        // `Issued::preset(Some(…))` would answer instantly if asked, so the
        // only way the issued identity can be absent from every call is that
        // `authorise` returned before asking.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;

        let fake = Arc::new(FakeRunner::new().with("ssh -o BatchMode=yes", 0, "", ""));

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(Some(working())),
        )
        .await
        .expect("already authorised");

        assert!(
            fake.calls()
                .iter()
                .all(|call| !call.contains("IdentityAgent=")),
            "an already-authorised server must not reach for an issued key: {:?}",
            fake.calls()
        );
        assert_eq!(copy_attempts(&fake), 0, "{:?}", fake.calls());
    }
    #[tokio::test]
    async fn a_server_that_will_not_honour_the_copied_key_carries_the_issued_one() {
        // `ssh.cloudcli.ai`, and the reason bootstrap alone was not enough: a
        // managed gateway accepts the write to `authorized_keys` and then
        // authenticates against its own registry regardless, so riabuild's key
        // can never work there. Falling back to the account password at that
        // point wasted the one credential that does.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;

        let fake = Arc::new(
            FakeRunner::new()
                // Never signs in with riabuild's own key — before or after the
                // copy.
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                // ...but the copy itself lands.
                .containing("authorized_keys", 0, "", ""),
        );

        let carried = authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(Some(working())),
        )
        .await
        .expect("a server riabuild can still reach is not a failure");

        let carried = carried.expect("the issued identity must carry the run");
        assert_eq!(carried.label, "prod-bastion");
    }
    #[tokio::test]
    async fn a_server_where_riabuilds_own_key_works_carries_nothing() {
        // The ordinary outcome, and the one that keeps a server's auth log able
        // to tell developers apart: once riabuild's own key signs in, the
        // issued identity is finished with.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;

        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                .then("ssh -o BatchMode=yes", 0, "", "")
                .containing("authorized_keys", 0, "", ""),
        );

        let carried = authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(Some(working())),
        )
        .await
        .expect("authorised");

        assert!(
            carried.is_none(),
            "riabuild's own key works, so nothing may be carried"
        );
    }
    #[tokio::test]
    async fn a_password_server_that_refuses_the_copied_key_still_carries_nothing() {
        // No issued key got in, so there is nothing to carry and the password
        // fallback is unchanged — the behaviour every ordinary server had
        // before issued keys existed.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;

        let fake = Arc::new(
            FakeRunner::new()
                .with(
                    "ssh -o BatchMode=yes",
                    255,
                    "",
                    "Permission denied (publickey).",
                )
                .with(
                    "ssh -o PreferredAuthentications=none",
                    255,
                    "",
                    "Permission denied (publickey,password).",
                )
                .containing("authorized_keys", 0, "", ""),
        );

        let carried = authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("a password is still a way in");

        assert!(carried.is_none());
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
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDQMfwG+m0AkDbU6a0vxE5ktTNTso5LskpebOKYF2VHP riabuild",
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
        let error = authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
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
            "the reason must distinguish this from a failed copy: {}",
            failure.detail
        );
        assert_eq!(
            copy_attempts(&fake),
            0,
            "there is no password to ask for, so nothing may be attempted: {:?}",
            fake.calls()
        );
    }
    #[tokio::test]
    async fn a_key_that_already_works_is_not_copied_again() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let fake = Arc::new(FakeRunner::new().with("ssh -o BatchMode=yes", 0, "", ""));

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("already fine");
        assert_eq!(copy_attempts(&fake), 0, "{:?}", fake.calls());
    }
    #[tokio::test]
    async fn a_run_that_needs_no_password_sweeps_one_left_unconfirmed() {
        // A previous run was killed between the askpass helper writing the
        // password down and `copy` deciding whether the server took it. This
        // run's key works, so nothing here will ever confirm it — `accept` and
        // `forget` both live in the copy, which this run does not perform.
        // Without the sweep it sat in the keyring indefinitely, for a server
        // that has not needed a password since.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        let unconfirmed = paths.remote_password_file(&format!("{}.pending", remote().hash()));
        tokio::fs::create_dir_all(unconfirmed.parent().expect("a directory"))
            .await
            .expect("mkdir");
        tokio::fs::write(&unconfirmed, "typo").await.expect("write");
        let fake = Arc::new(FakeRunner::new().with("ssh -o BatchMode=yes", 0, "", ""));

        authorise(
            &remote(),
            &paths,
            fake,
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("already fine");

        assert!(
            tokio::fs::metadata(&unconfirmed).await.is_err(),
            "{} is still there",
            unconfirmed.display()
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
    /// The server-side exit status meaning "the key was already there",
    /// shared with the half that emits it rather than written as a literal
    /// here — this is the contract between the two, and a test holding its
    /// own copy of it would keep passing after the contract changed.
    use copy::ALREADY_THERE;

    /// How many times `authorise` asked the server to install the key.
    ///
    /// The copy is an ordinary `ssh` now rather than a separately-named
    /// program, so it is told apart from the probes around it by the file it
    /// names — which is also the thing worth counting.
    fn copy_attempts(fake: &FakeRunner) -> usize {
        fake.calls()
            .iter()
            .filter(|call| call.contains("authorized_keys"))
            .count()
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
            warning.contains("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDQMfwG+m0AkDbU6a0vxE5ktTNTso5LskpebOKYF2VHP riabuild"),
            "…and must still carry the key itself: {warning}"
        );
        assert!(
            warning.contains("password"),
            "…and must say what happens instead, or an `Ok` reads as success: {warning}"
        );
    }
    /// The reported bug, from this side of it. A server that does not honour
    /// `authorized_keys` fails [`can_sign_in`] on every run, so `authorise`
    /// reaches the copy step on every run — that part is correct and stays.
    /// What must not happen is a second identical line, which is what
    /// `ssh-copy-id` did daily, because it decided "already installed?" by
    /// trying to log in and that answer is no on such a server forever.
    #[tokio::test]
    async fn a_key_already_in_authorized_keys_is_not_appended_again() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(a_server_that_takes_a_password().containing(
            "authorized_keys",
            ALREADY_THERE,
            "",
            "",
        ));
        let ui = Ui::new(true);

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &ui,
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("a password is still a way in");

        assert_eq!(
            copy_attempts(&fake),
            1,
            "one look at the file, and no second write: {:?}",
            fake.calls()
        );

        let warning = the_one_warning(&ui);
        assert!(
            warning.contains("already in"),
            "the developer has to be told the line is there, or the advice \
             below it makes no sense: {warning}"
        );
        assert!(
            !warning.to_lowercase().contains("add this line"),
            "telling them to paste a key that is already in the file is what \
             this whole change is about: {warning}"
        );
        assert!(
            warning.contains("AuthorizedKeysFile"),
            "…and pointed at what could actually be wrong on the server: {warning}"
        );
    }
    /// Nothing changed on the server, so nothing can have changed the answer
    /// to "can this key sign in?" — and each probe is a full connection that,
    /// on the very server this path exists for, costs the developer a
    /// password round trip.
    #[tokio::test]
    async fn a_key_that_was_already_there_is_not_probed_a_second_time() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(a_server_that_takes_a_password().containing(
            "authorized_keys",
            ALREADY_THERE,
            "",
            "",
        ));

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("a password is still a way in");

        assert_eq!(
            fake.calls()
                .iter()
                .filter(|call| call.starts_with("ssh -o BatchMode=yes"))
                .count(),
            1,
            "only the opening probe: {:?}",
            fake.calls()
        );
    }
    #[tokio::test]
    async fn a_key_that_will_not_sign_in_after_copying_warns_instead_of_stopping() {
        // The copy succeeded — the line reached `authorized_keys` — and sshd
        // still refuses it. The developer's password works, every remaining
        // step would have succeeded, and riabuild used to stop there.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        // The same refusal before and after the copy: the fake answers both
        // `BatchMode` probes identically, so the post-copy recheck fails.
        let fake =
            Arc::new(a_server_that_takes_a_password().containing("authorized_keys", 0, "", ""));
        let ui = Ui::new(true);

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &ui,
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("a password is a way in, so this must not stop the run");

        assert_eq!(
            copy_attempts(&fake),
            1,
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
    async fn a_copy_that_fails_warns_rather_than_ending_the_run() {
        // A read-only home, a full disk, a connection that dropped. The
        // server has just said it takes a password, so this costs the
        // developer a key, not the machine.
        let home = tempfile::TempDir::new().expect("tempdir");
        let paths = RealPaths::rooted_at(home.path());
        write_public_key(&paths).await;
        let fake = Arc::new(a_server_that_takes_a_password().containing(
            "authorized_keys",
            1,
            "",
            "Read-only file system",
        ));
        let ui = Ui::new(true);

        authorise(
            &remote(),
            &paths,
            fake,
            &ui,
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("a failed copy is not a missing way in");

        let warning = the_one_warning(&ui);
        assert!(warning.contains("could not add it"), "{warning}");
        assert_carries_the_remedy(&warning);
    }
    #[tokio::test]
    async fn a_key_that_works_after_being_installed_is_reported_as_success() {
        // The success branch (`ui.applied("Authorised"); Ok(())`) needs a
        // `BatchMode=yes` probe that answers differently before and after the
        // copy, which a single `with` stub cannot express — every other test
        // in this file returns the same fixed response both times, so the
        // recheck could never see a successful answer. `.then()` queues one
        // response per call: the first probe fails, the second succeeds.
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
                .containing("authorized_keys", 0, "", ""),
        );
        let ui = Ui::new(true);

        authorise(
            &remote(),
            &paths,
            fake.clone(),
            &ui,
            &api(),
            &mut Issued::preset(None),
        )
        .await
        .expect("the key works once installed, so this must succeed");

        assert_eq!(copy_attempts(&fake), 1, "{:?}", fake.calls());
        assert!(
            ui.warned().is_empty(),
            "a key that works is not something to warn about: {:?}",
            ui.warned()
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

        let error = authorise(
            &remote(),
            &paths,
            fake.clone(),
            &Ui::new(true),
            &api(),
            &mut Issued::preset(None),
        )
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
                call.contains("authorized_keys")
                    || call.starts_with("ssh -o PreferredAuthentications")
            }),
            "a missing key must fail before probing the server or writing anything: {:?}",
            fake.calls()
        );
    }
}

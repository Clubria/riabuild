//! Installing the key, and what the run uses once it has.
//!
//! Reached by both ways in — an account password and an issued key — because
//! only the credential differs: the same script runs on the server, the same
//! three outcomes are possible, and none of them is a reason to stop a
//! developer who has a working way onto the machine.

use std::sync::Arc;

use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::{Detail, Ui};

use super::copy;
use super::probe::can_sign_in;
use super::words::{carry_on, fallback};
use crate::Remote;

/// Installing the key and reporting what came of it — shared by the two ways
/// in, because only the credential differs.
///
/// `entry` is `Some` when an issued key is what authenticates the copy, and
/// `None` when the account password is. Every branch below is identical either
/// way: the same script runs on the server, the same three outcomes are
/// possible, and none of them is a reason to stop a developer who has a working
/// way onto the machine.
pub(super) async fn finish(
    remote: &Remote,
    paths: &dyn Paths,
    runner: Arc<dyn CommandRunner>,
    ui: &Ui,
    public_key: &str,
    entry: Option<&crate::issued::Working>,
    issued: &mut crate::issued::Issued,
) -> Result<Option<crate::issued::Working>> {
    let fallback = fallback(remote, entry);
    let installed = match copy::install_key(remote, paths, runner.clone(), public_key, entry).await
    {
        Ok(installed) => installed,
        Err(error) => {
            // Said out loud, because the developer is about to be asked for
            // that password again and an unexplained second prompt reads as
            // riabuild having forgotten how to remember one. `copy` has
            // already cleared it by the time this runs — see
            // `askpass::verdict`, which is the single reading of this stderr
            // that both the action and this sentence come from.
            if crate::askpass::refused_a_password(&error.to_string()) {
                ui.note(&format!(
                    "{} refused the password riabuild had saved for it, so riabuild has \
                     forgotten it rather than offering it again — the next run will ask.",
                    remote.target()
                ));
            }
            // A read-only home, a full disk, a connection that dropped. Not
            // fatal: there is a working way in — a password, or the issued key
            // that just proved itself — so this costs the developer a key
            // rather than the machine.
            carry_on(
                ui,
                remote,
                public_key,
                "the key could not be written to the server",
                &format!("riabuild could not add it to authorized_keys there ({error})"),
                &fallback,
            );
            return carry(ui, runner.clone(), issued, entry).await;
        }
    };

    if installed == copy::Installed::AlreadyThere {
        // Nothing was written, so nothing can have changed the answer
        // `can_sign_in` gave at the top of this function — re-probing would
        // spend another connection, and on the very server this branch exists
        // for, another password round trip, to be told the same thing.
        //
        // This warning deliberately does *not* end in `paste`'s remedy. The
        // line is already in the file: telling the developer to add it is how
        // they end up pasting a key they have pasted before, and it is what
        // riabuild itself did on every run for as long as `ssh-copy-id` was
        // the thing deciding whether the key was installed.
        ui.unresolved(
            "Authorised",
            "the server refuses the key it already has",
            &[
                Detail::Prose(&format!(
                    "riabuild's key is already in ~/.ssh/authorized_keys on {}, and that \
                     server still refuses it — so riabuild has left the file alone rather \
                     than adding another copy of the same line.",
                    remote.host
                )),
                Detail::Prose(&fallback),
                Detail::Prose(&format!(
                    "Something on {} is not honouring that file. The usual causes are an \
                     `AuthorizedKeysFile` in sshd_config pointing somewhere else, an \
                     `AuthenticationMethods` that needs more than a key, or a home \
                     directory whose mode `StrictModes` rejects.",
                    remote.host
                )),
            ],
        );
        return carry(ui, runner.clone(), issued, entry).await;
    }

    if !can_sign_in(remote, paths, runner.clone()).await? {
        // The line reached `authorized_keys` just now and sshd still refuses
        // it — same causes as the branch above, but this run is the one that
        // put it there, so the developer is hearing it for the first time.
        // None of them is a reason to stop a developer whose password works.
        carry_on(
            ui,
            remote,
            public_key,
            "the key is installed and still refused",
            "the key was copied, but signing in with it still does not work",
            &fallback,
        );
        return carry(ui, runner.clone(), issued, entry).await;
    }
    ui.applied("Authorised");
    // riabuild's own key works now, so nothing is carried and the agent is
    // finished with — which is the ordinary outcome and the one that keeps a
    // server's auth log able to tell developers apart.
    Ok(None)
}

/// Commits to using the issued identity for the rest of the run.
///
/// Reached only from the three branches where riabuild's own key has been
/// installed and *still* cannot sign in. On an ordinary server that is a
/// misconfiguration to report; on a managed SSH gateway it is simply how the
/// machine works — it accepted the write to `authorized_keys` and authenticates
/// against its own registry regardless — and there is nothing riabuild can do
/// to that server to change it.
///
/// Before this existed the run fell back to the account password at that point,
/// which meant an issued key authorised one `ssh-copy-id` for a key that would
/// never work and then went unused. Now the identity that demonstrably opens
/// the server is the one that carries the run.
async fn carry(
    ui: &Ui,
    runner: Arc<dyn CommandRunner>,
    issued: &mut crate::issued::Issued,
    entry: Option<&crate::issued::Working>,
) -> Result<Option<crate::issued::Working>> {
    let Some(entry) = entry else {
        // No issued key got us in either, so the password is still the answer
        // and `carry_on` has already said so.
        return Ok(None);
    };
    ui.note(&format!(
        "riabuild will use the {} key issued to you for the rest of this run, rather than \
         asking for a password.",
        entry.label
    ));
    // Reloaded without an expiry: the probe's bounded lifetime is right for a
    // question, and wrong for an identity that has to outlive an interactive
    // shell.
    issued.hold(runner, ui).await;
    Ok(Some(entry.clone()))
}

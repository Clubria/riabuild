//! The SSH keys the org issued to this developer, and finding the one that
//! opens the server they chose.
//!
//! ## Why this is lazy
//!
//! Nothing here happens until [`Issued::working`] is asked, and `authorise`
//! asks only after riabuild's *own* key has already failed to sign in. So on
//! every run after the first against a given server — which is nearly every run
//! — there is no fetch, no agent, and no org private key anywhere in this
//! process's memory. The cost of this feature is paid only by the runs that
//! need it.
//!
//! ## Why nothing here returns `Err`
//!
//! An issued key is an *additional* way in. Before it existed, a server
//! riabuild's key could not sign in to was handled by asking for the account
//! password — and that path is still there, immediately below this one. So
//! every ordinary failure (no keys issued, a fetch that did not work, no
//! `ssh-agent` on this machine, keys that none of them signs in with) is a
//! `None` and at most a warning, never a stop. The rule is `authorise`'s:
//! riabuild stops when there is no way in, not when the convenient way in
//! failed.

pub mod agent;

use crate::Remote;
use agent::Agent;
use riabuild_api::ApiClient;
use riabuild_api::issued::{IssuedKey, fetch_issued};
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::Ui;
use std::path::PathBuf;
use std::sync::Arc;

/// An issued key that has just proved it can sign in to this server.
#[derive(Debug, Clone)]
pub struct Working {
    /// What the terminal calls it, and what the audit log on the other side
    /// recorded serving.
    pub label: String,
    pub socket: PathBuf,
    pub public_key_path: PathBuf,
}

/// The issued keys for one `riabuild remote`, fetched at most once.
///
/// Every field defaults to "nothing has happened yet", which is exactly what a
/// run that never needs an issued key should cost.
#[derive(Default)]
pub struct Issued {
    agent: Option<Agent>,
    /// The key behind [`Working`], kept so [`Issued::hold`] can reload it
    /// without a lifetime once riabuild commits to carrying it.
    chosen: Option<IssuedKey>,
    /// `None` until [`working`](Issued::working) has been asked; `Some(None)`
    /// once it has been asked and the answer was "no key gets in".
    ///
    /// Two levels rather than one so a second call cannot re-probe: `authorise`
    /// asks, and `--check` asks, and re-probing would spend another round of
    /// connections to be told what is already known.
    answer: Option<Option<Working>>,
}

impl Issued {
    pub fn new() -> Self {
        Issued {
            agent: None,
            chosen: None,
            answer: None,
        }
    }

    /// An `Issued` whose answer is already decided, so nothing is fetched and
    /// no agent is started.
    ///
    /// `#[cfg(test)]`, and deliberately so: `authorise`'s tests need to drive
    /// both sides of the issued-key branch, and the alternative — a real
    /// `ApiClient` pointed at an unreachable URL — would put a network attempt
    /// and its timeout into every test in that file.
    #[cfg(test)]
    pub(crate) fn preset(answer: Option<Working>) -> Issued {
        Issued {
            agent: None,
            chosen: None,
            answer: Some(answer),
        }
    }

    /// The first issued key that signs in to this server, if any does.
    ///
    /// Probed one key at a time, in the order riabuild-web served them — which
    /// is by label, so the answer is stable between runs rather than depending
    /// on which key an agent happened to offer first.
    pub async fn working(
        &mut self,
        api: &ApiClient,
        remote: &Remote,
        paths: &dyn Paths,
        runner: Arc<dyn CommandRunner>,
        ui: &Ui,
    ) -> Option<&Working> {
        if self.answer.is_none() {
            let found = self.find(api, remote, paths, runner, ui).await;
            self.answer = Some(found);
        }
        self.answer.as_ref().and_then(|answer| answer.as_ref())
    }

    async fn find(
        &mut self,
        api: &ApiClient,
        remote: &Remote,
        paths: &dyn Paths,
        runner: Arc<dyn CommandRunner>,
        ui: &Ui,
    ) -> Option<Working> {
        let fetched = match fetch_issued(api).await {
            Ok(fetched) => fetched,
            Err(error) => {
                // A note rather than a warning: on the overwhelming majority of
                // servers no key was ever going to be issued, and a run that
                // shouted about a missing optional endpoint would be shouting
                // at nearly everyone.
                ui.note(&format!(
                    "Could not ask riabuild-web for the SSH keys issued to you: {error}"
                ));
                return None;
            }
        };
        // Said out loud, one line each. A key that has quietly vanished from a
        // developer's ring is a support ticket, and the reason it went is the
        // whole content of the answer.
        for reason in &fetched.refused {
            ui.warn(&format!("Ignoring an issued key — {reason}"));
        }
        if fetched.keys.is_empty() {
            return None;
        }

        let agent = match Agent::start(remote, paths, runner.clone()).await {
            Ok(Some(agent)) => agent,
            Ok(None) => {
                ui.note(
                    "You have SSH keys issued to you, but this machine has no `ssh-agent` for \
                     riabuild to hold them in — so it will ask for a password instead.",
                );
                return None;
            }
            Err(error) => {
                ui.warn(&format!(
                    "Could not start an ssh-agent for the issued keys: {error}"
                ));
                return None;
            }
        };

        ui.working(
            "Issued keys",
            &format!("trying {} issued to you", count(fetched.keys.len())),
        );

        let mut working = None;
        for key in &fetched.keys {
            let public = match agent
                .add(runner.clone(), key, Some(agent::PROBE_LIFETIME))
                .await
            {
                Ok(public) => public,
                Err(error) => {
                    ui.warn(&format!("Could not load the {} key: {error}", key.label));
                    continue;
                }
            };
            match agent.probe(remote, paths, runner.clone(), &public).await {
                Ok(true) => {
                    working = Some(Working {
                        label: key.label.clone(),
                        socket: agent.socket().to_path_buf(),
                        public_key_path: public,
                    });
                    self.chosen = Some(key.clone());
                    break;
                }
                Ok(false) => {}
                Err(error) => {
                    ui.warn(&format!("Could not try the {} key: {error}", key.label));
                }
            }
        }

        match &working {
            Some(found) => ui.applied(&format!("Issued keys — {} works", found.label)),
            None => ui.note("None of the SSH keys issued to you can sign in to that server."),
        }

        // Held whether or not one worked: the socket in `Working` is only alive
        // while this agent is, and `stop` is the caller's to call either way.
        self.agent = Some(agent);
        working
    }

    /// Commits to carrying this identity for the rest of the run.
    ///
    /// Reloads the key with no expiry. The probe loaded it with a bounded
    /// lifetime, which is right while riabuild is only asking a question — but
    /// a carried identity has to survive an interactive shell, and a key that
    /// expired mid-session would break the clipboard channel's reconnect with
    /// nothing on screen explaining it.
    ///
    /// Best effort: if the reload fails the identity is still loaded under its
    /// original lifetime, so the run continues and only a very long session
    /// would notice.
    pub async fn hold(&mut self, runner: Arc<dyn CommandRunner>, ui: &Ui) {
        let (Some(agent), Some(key)) = (self.agent.as_ref(), self.chosen.as_ref()) else {
            return;
        };
        if let Err(error) = agent.add(runner, key, None).await {
            ui.warn(&format!(
                "Could not extend the {} key for the rest of this run ({error}); a long \
                 session may need `riabuild remote` again.",
                key.label
            ));
        }
    }

    /// Ends the agent, if one was ever started.
    ///
    /// The orderly teardown, to be called on every path out of `connect` that
    /// can reach it. It is deliberately no longer the *guarantee*: this doc
    /// used to claim every path called it, and only `--check` and a failed
    /// `authorise` did, which left the agent directory behind on every
    /// successful run. `Drop for Agent` is what makes teardown unmissable —
    /// see its comment for what each half of it is actually cleaning up.
    pub async fn stop(&mut self) {
        if let Some(agent) = self.agent.take() {
            agent.stop().await;
        }
        // The answer is dropped with the agent: a `Working` naming a socket
        // that no longer exists is worse than no answer at all.
        self.answer = None;
    }
}

fn count(keys: usize) -> String {
    if keys == 1 {
        "the 1 key".to_string()
    } else {
        format!("the {keys} keys")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_reads_as_a_sentence_either_way() {
        assert_eq!(count(1), "the 1 key");
        assert_eq!(count(3), "the 3 keys");
    }
}

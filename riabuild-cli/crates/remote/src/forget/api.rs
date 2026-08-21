//! The two things `forget` asks riabuild-web for, each behind a seam.
//!
//! Same shape and same reason as `install.rs`'s `Downloads`: without them,
//! revoking a session and resolving an issued key are reachable only by a test
//! that stands up a real riabuild-web, which this crate's scaffolding has
//! never done — so the steps whose *failure* must stop the whole function had
//! no coverage at all.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use riabuild_api::ApiClient;
use riabuild_paths::Paths;
use riabuild_runner::CommandRunner;
use riabuild_ui::Ui;

use crate::{Remote, issued};

/// The one network call `forget` makes, behind a seam.
///
/// Same shape and same reason as `install.rs`'s `Downloads`: without it, step
/// 1 is only reachable by a test that stands up a real riabuild-web, which
/// this crate's scaffolding has never done. Every `forget` test therefore left
/// `session_id` empty, and the step whose *failure* must stop the whole
/// function before anything local changes had no coverage at all.
#[async_trait]
pub(crate) trait Revokes: Send + Sync {
    async fn revoke(&self, session_id: &str) -> Result<()>;
}

/// What production uses: `DELETE /api/v1/cli/sessions/<id>` (Task 3b).
pub(super) struct ApiRevokes<'a>(pub(super) &'a ApiClient);

#[async_trait]
impl Revokes for ApiRevokes<'_> {
    async fn revoke(&self, session_id: &str) -> Result<()> {
        self.0
            .delete_json::<serde_json::Value>(&format!("/api/v1/cli/sessions/{session_id}"))
            .await
            .map(|_| ())
    }
}

/// How `forget` reaches a server riabuild's own key cannot sign in to.
///
/// A seam for the same reason [`Revokes`] is one: resolving an issued identity
/// means a fetch from riabuild-web and an `ssh-agent`, neither of which this
/// crate's scaffolding stands up — so without it the branch that exists
/// entirely for managed gateways would be reachable only on a real gateway,
/// which is how it came to be hardcoded to `None` in the first place.
#[async_trait]
pub(crate) trait Carries: Send + Sync {
    /// `None` when nothing beyond riabuild's own key can sign in either.
    async fn carry(
        &self,
        remote: &Remote,
        paths: &dyn Paths,
        runner: Arc<dyn CommandRunner>,
        ui: &Ui,
    ) -> Option<Carried>;
}

/// An identity that can sign in, and the agent that is holding it.
///
/// The socket named in `working` is alive only while `issued` is, so the two
/// travel together and [`Carried::stop`] is what ends both.
pub(crate) struct Carried {
    pub(super) working: issued::Working,
    pub(super) issued: issued::Issued,
}

impl Carried {
    pub(super) async fn stop(mut self) {
        self.issued.stop().await;
    }
}

/// What production uses: the keys riabuild-web issued to this developer,
/// resolved exactly the way `authorise` resolves them.
pub(super) struct IssuedCarries<'a>(pub(super) &'a ApiClient);

#[async_trait]
impl Carries for IssuedCarries<'_> {
    async fn carry(
        &self,
        remote: &Remote,
        paths: &dyn Paths,
        runner: Arc<dyn CommandRunner>,
        ui: &Ui,
    ) -> Option<Carried> {
        let mut issued = issued::Issued::new();
        let working = issued
            .working(self.0, remote, paths, runner, ui)
            .await
            .cloned();
        match working {
            Some(working) => Some(Carried { working, issued }),
            None => {
                issued.stop().await;
                None
            }
        }
    }
}

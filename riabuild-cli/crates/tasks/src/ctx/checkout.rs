//! Which repository a run is about, and where its checkout lives.
//!
//! `repo` and `project_dir` are what a task reads; the rest is how the two
//! answer on a machine an older riabuild recorded a single path on, and on
//! a server where several developers share one Unix account.

use crate::Ctx;
use anyhow::Result;
use riabuild_api::Repo;

impl Ctx {
    /// The repository this run is about: what the picker settled on, else what
    /// this machine last used, else the org default.
    ///
    /// Fallible only in the last case, and only for a dashboard slug nobody
    /// could clone — see `OrgConfig::default_repo`.
    pub fn repo(&self) -> Result<Repo> {
        match &self.repo {
            Some(repo) => Ok(repo.clone()),
            None => self.org()?.default_repo(),
        }
    }

    /// Takes the repository this machine recorded, so a command that never puts
    /// the picker's question — `status`, `env`, `shell`, anything under
    /// `--check` — still reads the checkout of the repository the developer was
    /// last working on rather than the org default's.
    ///
    /// A recorded slug that will not parse is dropped rather than fatal: it can
    /// only have come from a riabuild that wrote one, and falling back to the
    /// default repository is a working machine where `?` here would be a broken
    /// one.
    pub(super) fn adopt_recorded_repo(&mut self) {
        if self.repo.is_some() {
            return;
        }
        self.repo = self
            .config
            .active_repo
            .as_deref()
            .and_then(|slug| Repo::parse(slug).ok());
    }

    /// The checkout of the repository this run is about, if it has one.
    ///
    /// Falls back to the single path an older riabuild recorded, and only when
    /// the repository asked about is the org default — the only repository that
    /// path can be a checkout of. That is what lets `riabuild status`, `riabuild
    /// env`, `riabuild shell` and `riabuild --check` find an existing checkout
    /// on a machine whose migration has not run: none of them puts the picker's
    /// question, and none of them may write. Read both, write one.
    pub fn project_dir(&self) -> Option<std::path::PathBuf> {
        let recorded = match self.repo().ok() {
            Some(repo) => self
                .config
                .checkout_of(repo.slug())
                .or_else(|| self.legacy_checkout_for(&repo)),
            // Not signed in, and nothing has said which repository this is
            // about. The one checkout an older riabuild recorded is still the
            // answer — `riabuild move-project` on a laptop with no session is
            // the case that reaches this.
            None => self.config.legacy_checkout(),
        };
        recorded.map(|path| riabuild_paths::expand_tilde(path, &self.paths.home()))
    }

    /// The pre-picker checkout, where it is an answer about `repo`.
    ///
    /// Two conditions, and both are about not handing back a path that is a
    /// checkout of something else. It answers only for the org default, because
    /// that is the only repository riabuild could have cloned before it asked;
    /// and only while nothing has chosen, because a machine that has chosen has
    /// a map, and a path outside it is one the map does not claim.
    fn legacy_checkout_for(&self, repo: &Repo) -> Option<&str> {
        if self.config.active_repo.is_some() {
            return None;
        }
        match self.org.as_ref().and_then(|org| org.default_repo().ok()) {
            Some(default) if *repo != default => None,
            _ => self.config.legacy_checkout(),
        }
    }

    /// Where a checkout goes when the developer has not chosen a place.
    ///
    /// On a laptop this is simply the platform default. On a server several
    /// developers share one Unix account, so the checkout is grouped under the
    /// developer's own GitHub login instead — never the shared default — so one
    /// developer's branches, uncommitted work, and `.env.local` never land in
    /// another's session. See `paths::remote_project_dir`.
    pub async fn default_checkout(&self) -> std::path::PathBuf {
        let named = self.repo().ok();
        let repo = named.as_ref().map(Repo::name).unwrap_or("repo");
        let home = self.paths.home();
        let Some(login) = self
            .server
            .as_ref()
            .and(self.member.as_ref())
            .map(|member| member.github_login.clone())
        else {
            return riabuild_paths::default_project_dir(&home, repo);
        };

        // A GitHub login can be freed and taken by somebody else, and a
        // directory can predate riabuild. Claim beside it rather than into it.
        for suffix in 1.. {
            let name = if suffix == 1 {
                login.clone()
            } else {
                format!("{login}-{suffix}")
            };
            let candidate = riabuild_paths::remote_project_dir(&home, &name, repo);
            let taken = tokio::fs::try_exists(&candidate).await.unwrap_or(false);
            if !taken || self.owned_by_this_namespace(&candidate).await {
                return candidate;
            }
        }
        unreachable!("suffix range 1.. never ends")
    }

    /// Whether a checkout candidate carries this namespace's own
    /// [`project::OWNER_MARKER`](crate::project::OWNER_MARKER) file, so a re-run recognises its own tree
    /// rather than claiming the next suffix every time. A missing or unreadable
    /// marker is treated as "somebody else's" — the safe direction, since
    /// claiming a directory nobody marked as ours is exactly the sharing this
    /// exists to prevent.
    ///
    /// The name comes from the constant `project` writes it under. Spelling it
    /// again here is how the reader and the writer drift apart.
    async fn owned_by_this_namespace(&self, candidate: &std::path::Path) -> bool {
        let marker = candidate.join(crate::project::OWNER_MARKER);
        tokio::fs::read_to_string(&marker)
            .await
            .map(|contents| contents.trim() == self.paths.root().to_string_lossy())
            .unwrap_or(false)
    }
}

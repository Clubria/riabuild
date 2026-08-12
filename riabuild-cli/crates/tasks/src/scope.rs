//! Whether this riabuild runs on a developer's own machine or on a server a
//! laptop provisions.
//!
//! One variable, `RIABUILD_REMOTE`, carrying the server's name. Four things
//! follow from it, and they are one idea — *this riabuild is managed from a
//! laptop*:
//!
//! - the session lives in a file in the namespace, not in a keyring
//! - the GitHub configuration lives in a per-session runtime directory
//! - self-update is suppressed, because no package manager owns this binary
//! - the shell banner says which server you are on

use anyhow::Result;
use riabuild_paths::Paths;
use riabuild_ui::Failure;
use std::path::PathBuf;

pub struct Scope {
    /// The server's name, when riabuild is running on one.
    pub server: Option<String>,
}

impl Scope {
    /// Split from `detect` so the decision is testable without setting a
    /// process-wide variable every other test in this binary would then see.
    pub fn read(value: Option<&str>) -> Scope {
        Scope {
            server: value.filter(|name| !name.is_empty()).map(str::to_string),
        }
    }

    pub fn detect() -> Scope {
        Scope::read(std::env::var("RIABUILD_REMOTE").ok().as_deref())
    }

    pub fn is_remote(&self) -> bool {
        self.server.is_some()
    }

    pub fn banner(&self) -> String {
        match &self.server {
            Some(name) => format!(
                "● Clubria environment active on {name} — type `exit` to leave, \
                 `claude` to start working"
            ),
            None => crate::shell::BANNER.to_string(),
        }
    }

    /// Where a *server* keeps its own riabuild session; `None` on a laptop,
    /// where the platform keychain holds it.
    ///
    /// The two facts that make a riabuild "a server" — `RIABUILD_REMOTE` naming
    /// one, and `RIABUILD_ROOT` pointing into `.riabuild-remote/<member-id>` —
    /// are set together by `remote::env_prefix` and mean nothing apart.
    /// Choosing off the variable alone is what let `RIABUILD_REMOTE=x riabuild
    /// login` on a laptop write a bearer token in cleartext to
    /// `~/.riabuild/session.token` — the exact path `riabuild-cli/CLAUDE.md`'s
    /// invariant forbids — while `describe()` called it "this server's riabuild
    /// namespace". So the file-backed store is derived from the namespace, the
    /// fact that is actually load-bearing, and a scope contradicting it is
    /// refused rather than quietly resolved either way.
    pub fn server_session_token_file(&self, paths: &dyn Paths) -> Result<Option<PathBuf>> {
        match (member_id_from_root(paths).is_ok(), self.is_remote()) {
            (true, true) => Ok(Some(paths.session_token_file())),
            (false, false) => Ok(None),
            (namespaced, _) => Err(Failure::new(
                "working out whether this riabuild is running on a managed server",
                "Run `riabuild remote <server>` from your laptop again — it sets RIABUILD_ROOT and \
                 RIABUILD_REMOTE together, and only one of them arrived here.",
            )
            .detail(format!(
                "RIABUILD_ROOT={:?} {} a server namespace, but RIABUILD_REMOTE {}",
                paths.root(),
                if namespaced { "is" } else { "is not" },
                if namespaced {
                    "is not set"
                } else {
                    "names one"
                },
            ))
            .into()),
        }
    }
}

/// The member id this root is namespaced under — and, by being `Ok` at all,
/// the single source of truth for "this riabuild is running inside a server
/// namespace", read off the path that *is* the namespace rather than off a
/// separate variable that would have to agree with it (R14's lesson).
///
/// Only the `<home>/.riabuild-remote/<member-id>` shape `remote::env_prefix`
/// sets `RIABUILD_ROOT` to qualifies, and never with an empty id:
/// `gh_session::open`/`attach` join the id verbatim onto `riabuild-gh-`, so an
/// empty one would collide every developer on a shared server onto one runtime
/// directory — and onto each other's GitHub credential.
pub fn member_id_from_root(paths: &dyn Paths) -> Result<String> {
    let root = paths.root();
    root.parent()
        .and_then(std::path::Path::file_name)
        .filter(|parent| *parent == ".riabuild-remote")
        .and_then(|_| root.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            Failure::new(
                "working out which developer this server session belongs to",
                "This is a bug in riabuild — send your team lead the value of RIABUILD_ROOT on that server.",
            )
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_paths::RealPaths;

    #[test]
    fn an_unset_variable_is_a_laptop() {
        assert!(!Scope::read(None).is_remote());
        assert!(!Scope::read(Some("")).is_remote());
    }

    #[test]
    fn a_named_server_is_remote_and_names_itself() {
        let scope = Scope::read(Some("build-01"));
        assert!(scope.is_remote());
        assert!(scope.banner().contains("build-01"), "{}", scope.banner());
        assert!(
            scope.banner().contains("exit"),
            "the way out is always on screen"
        );
    }

    #[test]
    fn a_laptop_banner_is_the_one_it_always_was() {
        assert_eq!(Scope::read(None).banner(), crate::shell::BANNER);
    }

    #[test]
    fn member_id_comes_from_the_roots_last_component() {
        let paths = RealPaths::with_root("/home/dev", "/home/dev/.riabuild-remote/550e8400");
        assert_eq!(member_id_from_root(&paths).expect("id"), "550e8400");
    }

    #[test]
    fn a_server_writes_its_session_to_a_file_only_inside_a_real_namespace() {
        let paths = RealPaths::with_root("/home/dev", "/home/dev/.riabuild-remote/550e8400");
        assert_eq!(
            Scope::read(Some("build-01"))
                .server_session_token_file(&paths)
                .expect("the sanctioned combination"),
            Some(paths.session_token_file())
        );
    }

    #[test]
    fn a_laptop_never_reaches_the_file_backed_keychain() {
        let paths = RealPaths::rooted_at("/Users/ada");
        assert_eq!(
            Scope::read(None)
                .server_session_token_file(&paths)
                .expect("a laptop"),
            None
        );
    }

    #[test]
    fn a_remote_scope_over_a_laptop_root_is_refused_not_written_to() {
        // The whole of S2: `RIABUILD_REMOTE=x riabuild login` with no
        // `RIABUILD_ROOT` used to write the session token in cleartext to
        // `~/.riabuild/session.token` — the path CLAUDE.md's invariant
        // forbids — because "server" was one environment variable rather
        // than a namespace. Any process running as the developer could flip
        // keychain-backed storage into a plaintext file read.
        let paths = RealPaths::rooted_at("/Users/ada");
        let error = Scope::read(Some("build-01"))
            .server_session_token_file(&paths)
            .expect_err("a laptop root is not a server namespace");
        // `expect` takes a `&str`, so a `{error}` in it prints literally.
        let failure = error.downcast_ref::<Failure>().unwrap_or_else(|| {
            panic!("must be the actionable Failure, not a generic error: {error}")
        });
        assert!(
            failure.detail.contains("RIABUILD_ROOT"),
            "{}",
            failure.detail
        );
    }

    #[test]
    fn a_namespaced_root_without_a_remote_scope_is_refused_too() {
        // The mirror image, refused for the same reason: the two facts are
        // set together by `remote::env_prefix`, so one without the other is a
        // machine riabuild cannot describe honestly either way.
        let paths = RealPaths::with_root("/home/dev", "/home/dev/.riabuild-remote/550e8400");
        assert!(Scope::read(None).server_session_token_file(&paths).is_err());
    }

    #[test]
    fn a_root_with_no_final_component_is_a_failure_not_an_empty_id() {
        // An empty member id is what would make every developer on a shared
        // server collide onto one runtime directory (and each other's
        // GitHub credential) — this must hard-error, never fall back.
        let paths = RealPaths::with_root("/", "/");
        let error = member_id_from_root(&paths).expect_err("no component to read");
        assert!(
            error.downcast_ref::<Failure>().is_some(),
            "must be the actionable Failure, not a generic error: {error}"
        );
    }
}

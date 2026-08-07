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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

//! A repository this developer may work on, named the way GitHub names one.
//!
//! This is a newtype rather than another `String` because of where the value
//! goes. Until the repository picker existed, `owner/repo` arrived from
//! `/api/v1/org/config` and nowhere else, so the only person who could choose it
//! was a team lead typing into the dashboard. It now also arrives from a
//! developer typing at a prompt, and it reaches two places that cannot take
//! arbitrary text:
//!
//! - `gh repo clone <slug> <dir>` argv, where a value beginning with `-` is a
//!   flag rather than a repository, and
//! - a **directory name**, via `paths::default_project_dir`, where `..` or a
//!   path separator puts a checkout — and the brokered `.env` files
//!   `env_local` writes into it — somewhere the developer never named.
//!
//! `org::version_only` states the reasoning for the field beside this one: the
//! client-side check exists so the CLI survives a server that forgets its own.
//! Here it is one step stronger. `org.update` validates both version fields
//! against a regex and accepts `repoSlug` as a bare string, so for this value
//! there has never been a server-side check to forget.

use anyhow::{Result, anyhow};
use std::fmt;

/// GitHub's own ceiling is 39 characters for a login and 100 for a repository
/// name. One cap for both, at the looser of the two: this is here to keep a
/// pasted essay out of argv and out of a directory name, not to re-implement
/// GitHub's validation and start refusing names GitHub allows.
const MAX_COMPONENT: usize = 100;

/// `owner/name`, both halves checked.
///
/// Ordering is derived so a list of repositories sorts predictably in a box a
/// developer reads; `Hash`/`Eq` so one can key a map of checkouts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Repo {
    /// Always exactly `owner/name`, and always with one `/`.
    slug: String,
    /// Where that `/` is, so `owner()` and `name()` are borrows rather than
    /// allocations — they are called inside loops that render the box.
    slash: usize,
}

impl Repo {
    /// A slug that must name its owner, as `/api/v1/org/config` serves it.
    pub fn parse(raw: &str) -> Result<Repo> {
        Self::parse_inner(raw, None)
    }

    /// A slug a developer typed, where a bare name means "in our org".
    ///
    /// The owner comes from the org default's own owner, so nothing new has to
    /// be added to `/api/v1/org/config` for a developer to type `payments`.
    pub fn parse_with_owner(raw: &str, default_owner: &str) -> Result<Repo> {
        Self::parse_inner(raw, Some(default_owner))
    }

    fn parse_inner(raw: &str, default_owner: Option<&str>) -> Result<Repo> {
        let trimmed = strip_url(raw.trim());
        if trimmed.is_empty() {
            return Err(anyhow!("give a repository as owner/repo"));
        }

        let (owner, name) = match trimmed.split_once('/') {
            Some((owner, name)) => (owner, name),
            // A developer types `payments`, not `Clubria/payments`. The server
            // has no such excuse: a config slug naming no owner would clone
            // whichever `payments` the signed-in account happens to own.
            None => match default_owner {
                Some(owner) => (owner, trimmed),
                None => {
                    return Err(anyhow!(
                        "{raw:?} names no owner — a repository is owner/repo"
                    ));
                }
            },
        };

        check(owner, "owner")?;
        check(name, "repository")?;
        Ok(Repo {
            slug: format!("{owner}/{name}"),
            slash: owner.len(),
        })
    }

    pub fn slug(&self) -> &str {
        &self.slug
    }

    pub fn owner(&self) -> &str {
        &self.slug[..self.slash]
    }

    /// The repository half, which is what the checkout directory is named after.
    pub fn name(&self) -> &str {
        &self.slug[self.slash + 1..]
    }

    /// Accepts every spelling of a GitHub remote for the same repository, so a
    /// developer who cloned over SSH is not told their checkout is wrong.
    pub fn matches_remote(&self, remote: &str) -> bool {
        let remote = remote.trim().trim_end_matches(".git").to_lowercase();
        let slug = self.slug.to_lowercase();
        [
            format!("https://github.com/{slug}"),
            format!("http://github.com/{slug}"),
            format!("git@github.com:{slug}"),
            format!("ssh://git@github.com/{slug}"),
        ]
        .iter()
        .any(|candidate| remote == *candidate)
    }
}

impl fmt::Display for Repo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.slug)
    }
}

/// What a developer is likely to have on the clipboard when they mean a
/// repository: the URL of the page they were just looking at, or the remote
/// they cloned with.
///
/// Stripped rather than refused, because "that is not a repository name" is a
/// poor answer to a value that names one perfectly well. What is stripped is
/// *gone* — the `Repo` is rebuilt from the two halves, so no part of the
/// original string reaches argv or a directory name either way.
fn strip_url(raw: &str) -> &str {
    let without_scheme = raw
        .strip_prefix("https://github.com/")
        .or_else(|| raw.strip_prefix("http://github.com/"))
        .or_else(|| raw.strip_prefix("ssh://git@github.com/"))
        .or_else(|| raw.strip_prefix("git@github.com:"))
        .unwrap_or(raw);
    without_scheme
        .trim_end_matches('/')
        .trim_end_matches(".git")
}

/// Why one half of a slug is not usable, if it is not.
fn check(component: &str, half: &str) -> Result<()> {
    if component.is_empty() {
        return Err(anyhow!("a repository needs both halves — owner/repo"));
    }
    if component.len() > MAX_COMPONENT {
        return Err(anyhow!(
            "that {half} name is longer than {MAX_COMPONENT} characters"
        ));
    }
    // `.` and `..` are refused by name because this becomes a directory:
    // `default_project_dir(home, "..")` is the parent of the directory riabuild
    // meant to clone into, and a checkout full of brokered secrets lands there.
    if component == "." || component == ".." {
        return Err(anyhow!("{component:?} is not a {half} name"));
    }
    // A leading dash is read by `gh repo clone` as a flag, not a repository.
    if component.starts_with('-') {
        return Err(anyhow!(
            "a {half} name cannot start with a dash — `gh` would read {component:?} as an option"
        ));
    }
    // Everything else GitHub allows. A separator is excluded by construction,
    // which is what keeps the name a single directory rather than a path.
    if let Some(bad) = component
        .chars()
        .find(|character| !matches!(character, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-'))
    {
        return Err(anyhow!(
            "{bad:?} cannot appear in a {half} name — letters, digits, dot, dash and underscore only"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_slug_splits_into_its_halves() {
        let repo = Repo::parse("Clubria/ai-builders-hub").expect("parses");
        assert_eq!(repo.owner(), "Clubria");
        assert_eq!(repo.name(), "ai-builders-hub");
        assert_eq!(repo.slug(), "Clubria/ai-builders-hub");
        assert_eq!(repo.to_string(), "Clubria/ai-builders-hub");
    }

    #[test]
    fn a_bare_name_is_completed_with_the_org() {
        // What a developer actually types at the prompt.
        let repo = Repo::parse_with_owner("payments", "Clubria").expect("parses");
        assert_eq!(repo.slug(), "Clubria/payments");
    }

    #[test]
    fn a_bare_name_from_the_server_is_refused() {
        // The old `repo_name()` answered "ai-builders-hub" for this and let the
        // run continue, so `gh repo clone ai-builders-hub` resolved to whichever
        // repository of that name the signed-in account owned.
        let error = Repo::parse("ai-builders-hub").expect_err("names no owner");
        assert!(
            format!("{error}").contains("names no owner"),
            "unhelpful message: {error}"
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_rather_than_refused() {
        assert_eq!(
            Repo::parse("  Clubria/payments \n").expect("parses").slug(),
            "Clubria/payments"
        );
    }

    #[test]
    fn what_a_developer_pastes_instead_of_typing_is_understood() {
        for pasted in [
            "https://github.com/Clubria/payments",
            "https://github.com/Clubria/payments/",
            "https://github.com/Clubria/payments.git",
            "http://github.com/Clubria/payments",
            "git@github.com:Clubria/payments.git",
            "ssh://git@github.com/Clubria/payments",
        ] {
            assert_eq!(
                Repo::parse(pasted)
                    .unwrap_or_else(|error| panic!("{pasted} should parse: {error}"))
                    .slug(),
                "Clubria/payments"
            );
        }
    }

    #[test]
    fn a_value_that_would_escape_the_checkout_directory_is_refused() {
        // `name()` becomes a directory name under
        // `paths::default_project_dir`. Every one of these would put a
        // checkout, and the brokered .env files written into it, outside the
        // directory riabuild chose.
        for bad in [
            "Clubria/..",
            "Clubria/.",
            "../etc",
            "Clubria/../../etc",
            "Clubria/sub/dir",
            "Clubria/pay ments",
            "Clubria/pay\\ments",
            "Clubria/pay:ments",
            "Clubria/pay\0ments",
        ] {
            assert!(
                Repo::parse(bad).is_err(),
                "{bad:?} must not be usable as a repository"
            );
        }
    }

    #[test]
    fn a_value_gh_would_read_as_a_flag_is_refused() {
        for bad in ["-upload-pack=touch x/repo", "Clubria/-x", "--help/x"] {
            assert!(Repo::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn an_empty_half_is_refused() {
        for bad in ["", "   ", "/", "Clubria/", "/payments"] {
            assert!(Repo::parse(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(Repo::parse_with_owner("", "Clubria").is_err());
    }

    #[test]
    fn an_absurdly_long_name_is_refused() {
        let long = "a".repeat(MAX_COMPONENT + 1);
        assert!(Repo::parse(&format!("Clubria/{long}")).is_err());
        assert!(Repo::parse(&format!("{long}/payments")).is_err());
        // The ceiling itself is allowed: this refuses essays, not long names.
        let at_the_limit = "a".repeat(MAX_COMPONENT);
        assert!(Repo::parse(&format!("Clubria/{at_the_limit}")).is_ok());
    }

    #[test]
    fn every_spelling_of_the_same_remote_matches() {
        let repo = Repo::parse("Clubria/ai-builders-hub").expect("parses");
        for remote in [
            "https://github.com/Clubria/ai-builders-hub",
            "https://github.com/Clubria/ai-builders-hub.git",
            "https://github.com/clubria/AI-Builders-Hub",
            "git@github.com:Clubria/ai-builders-hub.git",
            "ssh://git@github.com/Clubria/ai-builders-hub",
            "  https://github.com/Clubria/ai-builders-hub  ",
        ] {
            assert!(repo.matches_remote(remote), "{remote} should match");
        }
        for other in [
            "https://github.com/Clubria/payments",
            "https://gitlab.com/Clubria/ai-builders-hub",
            "",
        ] {
            assert!(!repo.matches_remote(other), "{other} should not match");
        }
    }
}

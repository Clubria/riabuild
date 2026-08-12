//! riabuild's own version, and version parsing and comparison.
//!
//! Every `--version` on a developer's machine has its own shape: `gh version
//! 2.96.0 (2026-07-02)`, `v22.23.1`, `1.2.3 (Claude Code)`. Checks compare
//! numbers, so this pulls the first dotted-numeric run out of whatever it is
//! given and ignores the rest.

// `unwrap_used` is denied workspace-wide. In tests a panic *is* the reporting
// mechanism for a failed precondition, so unwrapping a fixture there is
// correct and this keeps the deny from forcing ceremony into every test module.
#![cfg_attr(test, allow(clippy::unwrap_used))]

/// riabuild is versioned by release date, not by semver.
///
/// The version comes from the git tag, injected by the release workflow, and
/// deliberately **not** from `CARGO_PKG_VERSION`: Cargo requires valid semver,
/// which forbids both the leading zeros in `2026.08.04` and the fourth
/// component a same-day rebuild needs. Taking it from the tag also makes the
/// tag the only place a version is written down, so a binary that reports a
/// different version than the release it shipped in is not a mistake anyone
/// can make.
///
/// A local `cargo build` has no tag, and gets a sentinel that sits above every
/// real date. That is the useful direction to fail in: it reads as obviously
/// not-a-release, it clears any `minCliVersion` the server enforces, and
/// `update::decide` already leaves a build ahead of the published latest alone
/// — so working on riabuild never triggers riabuild upgrading itself.
///
/// It lives here rather than beside the clap definitions because it is the
/// product's version, not a command-line concern: `art::banner` prints it, the
/// API client sends it, and neither has any business reaching into the parser
/// to find it.
pub const VERSION: &str = match option_env!("RIABUILD_VERSION") {
    Some(version) => version,
    None => "9999.0.0-dev",
};

pub fn parse(text: &str) -> Option<Vec<u64>> {
    let mut current: Vec<u64> = Vec::new();
    let mut number = String::new();
    let mut chars = text.chars().peekable();

    while let Some(character) = chars.next() {
        if character.is_ascii_digit() {
            number.push(character);
            continue;
        }
        if character == '.' && !number.is_empty() {
            let next_is_digit = chars.peek().is_some_and(|c| c.is_ascii_digit());
            if next_is_digit {
                current.push(number.parse().ok()?);
                number.clear();
                continue;
            }
        }
        if !number.is_empty() {
            current.push(number.parse().ok()?);
            number.clear();
            // A run of at least two components is a version; one number on its
            // own is more likely a year or a build id.
            if current.len() >= 2 {
                return Some(current);
            }
            current.clear();
        }
    }

    if !number.is_empty() {
        current.push(number.parse().ok()?);
    }
    (current.len() >= 2).then_some(current)
}

pub fn compare(left: &[u64], right: &[u64]) -> std::cmp::Ordering {
    let length = left.len().max(right.len());
    for index in 0..length {
        let l = left.get(index).copied().unwrap_or(0);
        let r = right.get(index).copied().unwrap_or(0);
        match l.cmp(&r) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Is the version named in `text` at least `minimum`?
pub fn at_least(text: &str, minimum: &str) -> bool {
    match (parse(text), parse(minimum)) {
        (Some(found), Some(floor)) => compare(&found, &floor).is_ge(),
        _ => false,
    }
}

/// Exact equality on the numeric components, for pinned toolchains.
pub fn same(left: &str, right: &str) -> bool {
    match (parse(left), parse(right)) {
        (Some(l), Some(r)) => compare(&l, &r).is_eq(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_out_of_real_tool_output() {
        assert_eq!(
            parse("gh version 2.96.0 (2026-07-02)"),
            Some(vec![2, 96, 0])
        );
        assert_eq!(parse("v22.23.1"), Some(vec![22, 23, 1]));
        assert_eq!(parse("2.1.221 (Claude Code)"), Some(vec![2, 1, 221]));
        assert_eq!(parse("Infisical CLI v0.41.89"), Some(vec![0, 41, 89]));
        assert_eq!(parse("10.20.0"), Some(vec![10, 20, 0]));
    }

    #[test]
    fn a_bare_number_is_not_a_version() {
        assert_eq!(parse("2026"), None);
        assert_eq!(parse("no version here"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn compares_numerically_not_lexically() {
        assert!(at_least("v22.10.0", "22.9.0"));
        assert!(!at_least("v22.9.0", "22.10.0"));
        assert!(at_least("2.96.0", "2.96.0"));
    }

    #[test]
    fn missing_components_count_as_zero() {
        assert!(at_least("22.23", "22.23.0"));
        assert!(!at_least("22.22", "22.23.0"));
    }

    #[test]
    fn unparseable_output_never_passes_a_floor() {
        // A tool that printed something unexpected is not evidence of a good
        // machine; it is a reason to run the task.
        assert!(!at_least("command not found", "1.0.0"));
    }

    #[test]
    fn release_dates_compare_correctly() {
        // riabuild's own version is a release date. Zero padding is
        // presentation only and must not change the value, or a server floor
        // of "2026.08.04" would reject a CLI reporting "2026.8.4".
        assert_eq!(parse("2026.08.04"), Some(vec![2026, 8, 4]));
        assert!(same("2026.08.04", "2026.8.4"));

        assert!(at_least("2026.08.12", "2026.08.04"));
        assert!(!at_least("2026.08.04", "2026.08.12"));
        assert!(at_least("2026.11.03", "2026.08.04"));
        assert!(at_least("2027.01.02", "2026.11.03"));

        // A second release on the same day carries a fourth component, which
        // semver could not express and which must sort above the first.
        assert!(at_least("2026.08.04.1", "2026.08.04"));
        assert!(!at_least("2026.08.04", "2026.08.04.1"));
    }

    #[test]
    fn the_development_sentinel_outranks_every_real_date() {
        // cli::VERSION falls back to this when no tag injected a version. It
        // has to clear any floor the server might set, or `cargo run` would be
        // refused by /api/v1 with `cli_too_old`.
        assert!(at_least("9999.0.0-dev", "2026.08.04"));
        assert!(!at_least("2026.08.04", "9999.0.0-dev"));
    }

    #[test]
    fn pinned_toolchains_compare_exactly() {
        assert!(same("v22.23.1", "22.23.1"));
        assert!(!same("v22.23.2", "22.23.1"));
    }
}

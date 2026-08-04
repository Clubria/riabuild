//! Version parsing and comparison.
//!
//! Every `--version` on a developer's machine has its own shape: `gh version
//! 2.96.0 (2026-07-02)`, `v22.23.1`, `1.2.3 (Claude Code)`. Checks compare
//! numbers, so this pulls the first dotted-numeric run out of whatever it is
//! given and ignores the rest.

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
    fn pinned_toolchains_compare_exactly() {
        assert!(same("v22.23.1", "22.23.1"));
        assert!(!same("v22.23.2", "22.23.1"));
    }
}

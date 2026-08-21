//! The shape every riabuild error takes, and how it is printed.
//!
//! Four parts, none of them optional to the developer: what was being
//! attempted in their words, the exact command and its stderr, one concrete
//! next action, and whether re-running is safe.

use riabuild_theme::Role;

use crate::Ui;
use crate::wrap::Detail;

/// A failure a developer can act on.
#[derive(Debug, Clone)]
pub struct Failure {
    /// What riabuild was trying to do, in the developer's words.
    pub attempting: String,
    /// The exact command that failed, if there was one.
    pub command: Option<String>,
    /// stderr, or whatever else explains it.
    pub detail: String,
    /// One concrete next action.
    pub action: String,
    pub safe_to_rerun: bool,
}

impl Failure {
    pub fn new(attempting: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            attempting: attempting.into(),
            command: None,
            detail: String::new(),
            action: action.into(),
            safe_to_rerun: true,
        }
    }

    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.attempting, self.action)
    }
}

impl std::error::Error for Failure {}

impl Ui {
    /// The four things every failure must say.
    pub fn failure(&self, failure: &Failure) {
        self.end_status_line();
        eprintln!();
        eprintln!(
            "  {} {}",
            self.paint(Role::Danger, "riabuild stopped:"),
            failure.attempting
        );
        if let Some(command) = &failure.command {
            eprintln!("    {} {}", self.paint(Role::Muted, "ran"), command);
        }
        let body = self.width.saturating_sub(crate::wrap::INDENT.len());
        for line in failure
            .detail
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(8)
            .flat_map(|line| crate::wrap::fold(line, body))
        {
            eprintln!("{}{}", crate::wrap::INDENT, self.paint(Role::Muted, &line));
        }
        // The label is folded *with* the sentence rather than printed in front
        // of it, so the first line is measured including the nine columns it
        // occupies. An action is the longest thing a failure carries — the
        // remedy for a stale host key names a file, a host and two commands —
        // and it is the line the developer has to act on.
        let mut paragraphs = failure.action.split('\n');
        let opening = paragraphs.next().unwrap_or_default().trim();
        for line in crate::wrap::fold(&format!("do this: {opening}"), body) {
            match line.strip_prefix("do this:") {
                Some(rest) => eprintln!(
                    "{}{}{rest}",
                    crate::wrap::INDENT,
                    self.paint(Role::Strong, "do this:")
                ),
                None => eprintln!("{}{line}", crate::wrap::INDENT),
            }
        }
        // Anything past the first paragraph is a line to copy — the public key
        // in `authorise`'s paste-it-by-hand remedy is the only one today — and
        // gets the same treatment as a warning's. That is what a `\n` in an
        // action means, and the only thing it means: `Failure` is a plain
        // struct built at a hundred call sites, so the alternative is asking
        // each of them to classify a paragraph none of them has.
        let rest: Vec<Detail> = paragraphs.map(Detail::Verbatim).collect();
        for line in crate::wrap::detail_lines(self.theme, self.width, &rest) {
            eprintln!("{line}");
        }
        eprintln!(
            "    {}",
            self.paint(
                Role::Muted,
                if failure.safe_to_rerun {
                    "running `riabuild` again is safe once that is done"
                } else {
                    "do not re-run riabuild until that is done"
                },
            )
        );
        eprintln!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_carries_all_four_parts() {
        let failure = Failure::new("checking your GitHub sign-in", "run `gh auth login`")
            .command("gh auth status")
            .detail("You are not logged into any GitHub hosts.");

        assert!(failure.command.is_some());
        assert!(!failure.detail.is_empty());
        assert!(failure.safe_to_rerun);
        assert_eq!(
            failure.to_string(),
            "checking your GitHub sign-in — run `gh auth login`"
        );
    }
}
